//! HMR 热重载（对应 Cordis `cordis-plugin-hmr` 的 `registerConfig`）。
//!
//! 注册 `(路径, refresh 回调)`；`poll()` 检测 add/change/unlink（存在性 +
//! 内容 hash），变化则**串行**调用 refresh（失败记录到 errors，对应 Cordis 的
//! `hmr/error` 事件）。首次 `poll` 建立快照不触发（对应 chokidar `ready`）。
//!
//! M35：**事件驱动**（对齐 Cordis chokidar）——`watch(paths)` 启动后台
//! notify watcher（OS 文件系统通知），事件经 mpsc 桥接回主线程；`poll()`
//! 先消费事件队列（事件只作**唤醒信号**），对命中的注册路径做**指纹确认**
//! 后再 refresh（notify 事件可能重复/合并/误报临时文件——指纹兜底）。
//! 无 watcher 时 `poll()` 退化为全量轮询（向后兼容）。
//!
//! 单线程纪律：`Hmr` 保持 `Rc<RefCell>`（非 Send，refresh 回调同）；后台线程
//! 仅持有 `Sender<PathBuf>`（Send），不触碰 Hmr 本身。

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use dsh_core::CordisError;
use notify::Watcher;

/// 文件内容指纹：存在性 + 内容 hash（std::hash，非加密但足够检测变化）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    exists: bool,
    hash: u64,
}

fn fingerprint(path: &Path) -> Fingerprint {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut h = DefaultHasher::new();
            bytes.hash(&mut h);
            Fingerprint {
                exists: true,
                hash: h.finish(),
            }
        }
        Err(_) => Fingerprint {
            exists: false,
            hash: 0,
        },
    }
}

/// 一个被监视的配置文件。
struct WatchEntry {
    /// 上次 poll 的指纹（注册时/首次 poll 时建立）。
    last: Fingerprint,
    /// refresh 回调（串行执行；宿主通常是 `Include::refresh`）。
    refresh: Rc<dyn Fn() -> Result<(), CordisError>>,
}

/// M38：refresh 失败的事件通知（对齐 Cordis `hmr/config-update-failed`）。
type ErrorSink = dyn Fn(&str, &CordisError);

/// 事件驱动桥接（M35）：后台 notify watcher → mpsc 事件队列。
/// watcher 句柄必须保活（drop 即停止监视）；Receiver 供主线程 poll 消费。
struct WatchBridge {
    /// notify watcher（内部线程；drop 停止监视）。
    #[allow(dead_code)]
    watcher: notify::RecommendedWatcher,
    /// 变化路径事件队列（后台线程 send，主线程 try_recv）。
    rx: mpsc::Receiver<PathBuf>,
}

/// HMR：文件变化 → refresh 回调（串行）。线程安全不需要（单线程纪律）。
#[derive(Default)]
pub struct Hmr {
    entries: RefCell<HashMap<PathBuf, WatchEntry>>,
    /// 最近一次 poll 的失败记录 `(path, error)`（对应 Cordis `hmr/error`）。
    errors: RefCell<Vec<(String, CordisError)>>,
    /// 事件驱动桥接（可选；None = 纯轮询）。
    bridge: RefCell<Option<WatchBridge>>,
    /// M38：refresh 失败的事件通知（对齐 Cordis `hmr/config-update-failed`
    /// 的 `ctx.parallel(filename, error)`——宿主可注入 Cordis emit 或告警）。
    /// None = 仅记录 errors（向后兼容，`take_errors` 查询仍可用）。
    error_sink: RefCell<Option<Rc<ErrorSink>>>,
}

impl Hmr {
    pub fn new() -> Self {
        Hmr::default()
    }

    /// 设置 refresh 失败的事件通知（M38）：refresh 失败时调用
    /// `sink(filename, error)`——对应 Cordis `hmr/config-update-failed`
    /// 的 parallel 事件（宿主把此回调接到 `ctx.parallel` 或告警日志）。
    /// 与 `take_errors()` 查询并存（双通道：事件式通知 + 查询式拉取）。
    pub fn set_error_sink(&self, sink: Rc<ErrorSink>) {
        *self.error_sink.borrow_mut() = Some(sink);
    }

    /// 注册一个被监视文件。**立即建立初始快照**（首次 poll 不触发该文件）。
    /// 重复注册同路径 → 覆盖（等价 Cordis 的「config path already registered」报错
    /// 之外，这里允许替换回调）。
    pub fn register_config(
        &self,
        path: impl AsRef<Path>,
        refresh: Rc<dyn Fn() -> Result<(), CordisError>>,
    ) {
        let path = path.as_ref().to_path_buf();
        let last = fingerprint(&path);
        self.entries.borrow_mut().insert(
            path,
            WatchEntry {
                last,
                refresh,
            },
        );
    }

    /// 取消监视。
    pub fn unregister(&self, path: impl AsRef<Path>) {
        self.entries.borrow_mut().remove(path.as_ref());
    }

    /// 启动事件驱动 watcher（M35）：对给定路径做 OS 文件系统监视，变化事件
    /// 经后台线程 + mpsc 桥接回本 Hmr（单线程纪律：后台只持有 Sender）。
    /// 事件仅作唤醒信号——`poll()` 消费事件后仍做指纹确认。
    ///
    /// 返回 Err = notify 启动失败（如路径不存在/无权限）；此时 Hmr 仍可用，
    /// `poll()` 退化为轮询。
    pub fn watch(&self, paths: &[PathBuf]) -> Result<(), CordisError> {
        if self.bridge.borrow().is_some() {
            return Ok(()); // 幂等：已有 watcher
        }
        let (tx, rx) = mpsc::channel::<PathBuf>();
        // 事件处理器：把变化路径发回主线程（只关心路径；kind 由指纹确认决定）
        let handler = move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                for p in event.paths {
                    let _ = tx.send(p);
                }
            }
        };
        let mut watcher = notify::recommended_watcher(handler)
            .map_err(|e| CordisError::Internal(format!("hmr watcher: {e}")))?;
        for path in paths {
            watcher
                .watch(path, notify::RecursiveMode::NonRecursive)
                .map_err(|e| CordisError::Internal(format!("hmr watch {}: {e}", path.display())))?;
        }
        *self.bridge.borrow_mut() = Some(WatchBridge { watcher, rx });
        Ok(())
    }

    /// 停止事件驱动 watcher（`poll()` 退回轮询）。
    pub fn unwatch(&self) {
        *self.bridge.borrow_mut() = None;
    }

    /// 轮询：变化（add/change/unlink）→ 串行调用 refresh；返回触发过 refresh
    /// 的路径列表。refresh 失败 → 记入 errors（指纹已更新，下次不重试同一指纹）。
    ///
    /// M35：有 watcher 时先消费事件队列（仅对**注册过**且指纹变化的路径
    /// refresh——事件路径可能重复/是临时文件）；无 watcher 时全量轮询。
    pub fn poll(&self) -> Vec<String> {
        // 1. 收集候选路径：事件队列命中的注册路径；无 watcher 则全部注册路径
        let mut candidates: Vec<PathBuf> = Vec::new();
        if self.bridge.borrow().is_some() {
            // 事件队列：只收集**注册过**的路径（事件可能重复/是临时文件）
            let mut seen = std::collections::HashSet::new();
            loop {
                let event_path = {
                    let bridge = self.bridge.borrow();
                    match bridge.as_ref() {
                        Some(b) => b.rx.try_recv(),
                        None => break,
                    }
                };
                match event_path {
                    Ok(p) => {
                        if seen.insert(p.clone()) && self.entries.borrow().contains_key(&p) {
                            candidates.push(p);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        } else {
            candidates = self.entries.borrow().keys().cloned().collect();
        }

        // 2. 指纹确认 + refresh（与旧轮询逻辑一致）
        let mut changed = Vec::new();
        for path in candidates {
            let now = fingerprint(&path);
            let should_refresh = {
                let entries = self.entries.borrow();
                entries
                    .get(&path)
                    .map(|e| e.last != now)
                    .unwrap_or(false)
            };
            if !should_refresh {
                continue;
            }
            // 更新指纹（无论 refresh 成败：事件已消费）
            {
                let mut entries = self.entries.borrow_mut();
                if let Some(e) = entries.get_mut(&path) {
                    e.last = now.clone();
                }
            }
            let result = {
                let entries = self.entries.borrow();
                entries.get(&path).map(|e| (e.refresh)())
            };
            match result {
                Some(Ok(())) => changed.push(path.to_string_lossy().to_string()),
                Some(Err(e)) => {
                    let path_str = path.to_string_lossy().to_string();
                    self.errors.borrow_mut().push((path_str.clone(), e.clone()));
                    // M38：事件式通知（对齐 Cordis `hmr/config-update-failed`
                    // 的 parallel 事件；宿主注入 emit/告警）。失败记录与通知
                    // 并存（查询式 take_errors + 事件式 sink）。
                    if let Some(sink) = self.error_sink.borrow().as_ref() {
                        sink(&path_str, &e);
                    }
                    changed.push(path_str);
                }
                None => {}
            }
        }
        changed
    }

    /// 取走失败记录（hmr/error 语义：宿主可据此告警）。
    pub fn take_errors(&self) -> Vec<(String, CordisError)> {
        std::mem::take(&mut *self.errors.borrow_mut())
    }
}

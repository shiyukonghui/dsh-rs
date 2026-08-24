//! `/plugins/events` HMR SSE 通道（D-099，对齐 TS `client/hmr` 宿主半）。
//!
//! 前端 web 组合包**无条件挂载** `@deepseek-ai/dsh-client-hmr`（always-on 客户端插件
//! 重载链），其浏览器半在 `ctx.effect` 无条件 `new EventSource('/plugins/events')`。
//! 本模块提供 TS 对等的宿主面：
//! - 连接建立：写 `: connected\n\n` 注释（心跳可识别）＋ `{type:"graph", graph}` 初始帧；
//! - 之后对 client bundle 的**内容**变化广播 `{type:"rebuilt", id, rev}` 帧（rev 即新内容
//!   `short_hash`，与 `/plugins/<id>/client.js?rev=` 一致）；
//! - 无重建 watcher 改写 bundle 时，通道保持空闲（与 TS「the chain stays idle」一致）。
//!
//! Watch 集取自启动时的 boot manifest（静态；宿主不在运行时挂/卸客户端插件——TS 的
//! `onGraphChanged` 动态增删行超出 Rust 侧能力，D-099 已知限制①）。`poll_once` 纯
//! 可重入（stat→hash→返回变化），单测零时序驱动；`run` 是薄循环。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::web::{short_hash, BootManifest};

/// `/plugins/events` SSE 通道端点（wire 常量，对齐 TS `EVENTS_ENDPOINT`）。
pub const EVENTS_ENDPOINT: &str = "/plugins/events";

/// watcher 扫描间隔（毫秒；TS 默认 pollIntervalMs）。
pub const HMR_POLL_INTERVAL_MS: u64 = 500;

/// `/plugins/events` 连接建立时的 SSE 注释行（事件源可识别的心跳，解析时自然跳过）。
fn connected_line() -> String {
    ": connected\n\n".to_string()
}

/// 序列化一条 `{type:"graph", graph}` SSE 帧。
fn graph_line(graph_rev: &str, rows: &[WatchedRow]) -> String {
    let graph = serde_json::json!({
        "type": "graph",
        "graph": {
            "rev": graph_rev,
            "entries": rows.iter().map(|r| {
                let mut m = serde_json::Map::new();
                m.insert("id".into(), serde_json::Value::String(r.id.clone()));
                m.insert("url".into(), serde_json::Value::String(format!(
                    "/plugins/{}/client.js?rev={}", r.id, r.rev
                )));
                m.insert("rev".into(), serde_json::Value::String(r.rev.clone()));
                if !r.inject.is_empty() {
                    m.insert("inject".into(), serde_json::to_value(&r.inject).unwrap_or(serde_json::Value::Null));
                }
                m.insert("immediately".into(), serde_json::Value::Bool(r.immediately));
                serde_json::Value::Object(m)
            }).collect::<Vec<_>>(),
        }
    });
    let json = serde_json::to_string(&graph).unwrap_or_default();
    format!("data: {json}\n\n")
}

/// 序列化一条 `{type:"rebuilt", id, rev}` SSE 帧（字段语义对齐 TS `sseData`；serde_json
/// Map 为 BTreeMap 会按键排序，字段序对浏览器无语义影响——JSON 解析按名取值）。
fn rebuilt_line(id: &str, rev: &str) -> String {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::Value::String("rebuilt".into()));
    m.insert("id".into(), serde_json::Value::String(id.to_string()));
    m.insert("rev".into(), serde_json::Value::String(rev.to_string()));
    let json = serde_json::to_string(&serde_json::Value::Object(m)).unwrap_or_default();
    format!("data: {json}\n\n")
}

/// 一条被 watch 的 bundle（对齐 TS `WatchedBundle`）。
struct WatchedRow {
    id: String,
    path: PathBuf,
    mtime_ms: u64,
    size: u64,
    rev: String,
    inject: Vec<String>,
    immediately: bool,
    /// 上次扫描未找到文件（ENOENT）→ 标脏等待恢复。
    dirty: bool,
}

/// stat 一次 bundle 文件 → `(mtime_ms, size)`；缺失 → None。
fn stat_row(path: &PathBuf) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// 共享通道：静态 watch 表 + 活跃连接集（每条连接一个 mpsc；watcher 线程绝不碰 socket，
/// 广播经 mpsc 发送——对端 drop 即移除，避免并发写同一连接的写面）。
pub struct HmrChannel {
    inner: Mutex<Inner>,
}

struct Inner {
    graph_rev: String,
    rows: Vec<WatchedRow>,
    clients: HashMap<u64, Sender<String>>,
    next_client: u64,
}

impl HmrChannel {
    /// 从启动时的 boot manifest 建静态 watch 集（D-099 已知限制①）。启动时 stat 一次
    /// 建基线；缺失的 bundle 标脏（等价 TS `watchRow` 的 ENOENT→dirty）。
    pub fn new(manifest: &BootManifest) -> Self {
        let rows = manifest
            .entries
            .iter()
            .map(|e| {
                let path = e.bundle_root.join("lib/client.js");
                let stat = stat_row(&path);
                let (mtime_ms, size) = stat.unwrap_or((0, 0));
                WatchedRow {
                    id: e.id.clone(),
                    path,
                    mtime_ms,
                    size,
                    rev: e.rev.clone(),
                    inject: e.inject.clone(),
                    immediately: e.immediately,
                    dirty: stat.is_none(),
                }
            })
            .collect();
        Self {
            inner: Mutex::new(Inner {
                graph_rev: manifest.rev.clone(),
                rows,
                clients: HashMap::new(),
                next_client: 0,
            }),
        }
    }

    /// 注册一个客户端连接：返回 `(id, receiver, 初始帧)`（connected 注释 + graph 帧）。
    pub fn connect(&self) -> (u64, Receiver<String>, Vec<String>) {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_client;
        inner.next_client += 1;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        inner.clients.insert(id, tx);
        let initial = vec![connected_line(), graph_line(&inner.graph_rev, &inner.rows)];
        (id, rx, initial)
    }

    /// 注销一个客户端连接。
    pub fn disconnect(&self, id: u64) {
        self.inner.lock().unwrap().clients.remove(&id);
    }

    /// 扫描一遍 watch 集，返回 `(id, 新 rev)` 的 rebuilt 列表（纯可重入、零时序）。
    /// stat 未变且未标脏 → 跳过；缺失 → 标脏重试（对齐 TS ENOENT→dirty）；变化 →
    /// 重读内容哈希，rev 变化才算 rebuilt（对齐 TS `rebuilt()` 的同 rev 静默）。
    pub fn poll_once(&self) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap();
        let mut rebuilt = Vec::new();
        for row in inner.rows.iter_mut() {
            let cur = stat_row(&row.path);
            let unchanged = !row.dirty
                && cur
                    .map(|(m, s)| m == row.mtime_ms && s == row.size)
                    .unwrap_or(false);
            if unchanged {
                continue;
            }
            match cur {
                // 缺失：标脏，重试直到文件出现（不当作 rebuilt）。
                None => row.dirty = true,
                Some((m, s)) => {
                    let bytes = std::fs::read(&row.path).unwrap_or_default();
                    let new_rev = short_hash(&bytes);
                    row.mtime_ms = m;
                    row.size = s;
                    if new_rev != row.rev {
                        row.rev = new_rev.clone();
                        rebuilt.push((row.id.clone(), new_rev));
                    }
                    row.dirty = false;
                }
            }
        }
        rebuilt
    }

    /// 广播 rebuilt 帧到所有活跃连接；发送失败（对端关闭/接收者 drop）即移除该连接
    /// （`retain` 遇失败项移除，防连接表泄漏）。
    pub fn broadcast(&self, rebuilt: &[(String, String)]) {
        if rebuilt.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        for (id, rev) in rebuilt {
            let line = rebuilt_line(id, rev);
            inner.clients.retain(|_, tx| tx.send(line.clone()).is_ok());
        }
    }

    /// watcher 循环：每 `interval_ms` 扫描一次并广播（TS 默认 500，`HMR_POLL_INTERVAL_MS`）。
    pub fn run(&self, interval_ms: u64) {
        loop {
            std::thread::sleep(Duration::from_millis(interval_ms));
            let rebuilt = self.poll_once();
            if !rebuilt.is_empty() {
                self.broadcast(&rebuilt);
            }
        }
    }

    /// 当前活跃连接数（测试观察；生产不调用）。
    #[cfg(test)]
    fn client_count(&self) -> usize {
        self.inner.lock().unwrap().clients.len()
    }
}

/// 写原始字节；失败返回 None（连接关闭）。
fn write_all_err<W: std::io::Write + ?Sized>(w: &mut W, data: &[u8]) -> Option<()> {
    std::io::Write::write_all(w, data).ok()?;
    std::io::Write::flush(w).ok()?;
    Some(())
}

/// 服务一条 `/plugins/events` SSE 连接（运行在独立线程）：SSE 头 → connected + graph
/// 初始帧 → 增量 rebuilt 帧（来自 watcher 广播）；接收超时写 keepalive 注释探测关闭。
/// 任何写失败或通道断开 → 注销并退出。
pub fn stream_hmr_events(mut writer: Box<dyn std::io::Write + Send>, channel: Arc<HmrChannel>) {
    if write_all_err(
        &mut writer,
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )
    .is_none()
    {
        return;
    }
    let (id, rx, initial) = channel.connect();
    for line in initial {
        if write_all_err(&mut writer, line.as_bytes()).is_none() {
            channel.disconnect(id);
            return;
        }
    }
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(line) => {
                if write_all_err(&mut writer, line.as_bytes()).is_none() {
                    channel.disconnect(id);
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if write_all_err(&mut writer, b": keepalive\n\n").is_none() {
                    channel.disconnect(id);
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                channel.disconnect(id);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{BootEntry, BootManifest};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 临时目录里建含一个 web 插件的 boot manifest（每调用唯一序号避免并行冲突）。
    fn make_manifest() -> (PathBuf, BootManifest) {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("dsh-web-hmr-{}-{n}", std::process::id()));
        let pkg = root.join("@deepseek-ai").join("dsh-client-demo");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        let bundle = pkg.join("lib/client.js");
        std::fs::write(&bundle, "load('v1');").unwrap();
        let entry = BootEntry {
            id: "@deepseek-ai/dsh-client-demo".to_string(),
            bundle_root: pkg,
            rev: short_hash(b"load('v1');"),
            inject: vec![],
            immediately: true,
        };
        let m = BootManifest {
            rev: "graph-rev-1".to_string(),
            entries: vec![entry],
        };
        (root, m)
    }

    fn bundle_path(manifest: &BootManifest) -> PathBuf {
        manifest.entries[0].bundle_root.join("lib/client.js")
    }

    /// 初始帧：connected 注释 + graph 帧（含 rev/url/inject/immediately）。
    #[test]
    fn connect_sends_connected_and_graph_frames() {
        let (_root, m) = make_manifest();
        let ch = HmrChannel::new(&m);
        let (id, _rx, initial) = ch.connect();
        assert_eq!(initial.len(), 2, "connected comment + graph frame");
        assert_eq!(initial[0], ": connected\n\n");
        assert!(
            initial[1].starts_with("data: {"),
            "graph is an SSE data line"
        );
        assert!(
            initial[1].contains(r#""type":"graph""#),
            "graph frame tagged"
        );
        assert!(initial[1].contains(r#""rev":"graph-rev-1""#));
        assert!(initial[1].contains("@deepseek-ai/dsh-client-demo"));
        assert!(initial[1].contains("/plugins/@deepseek-ai/dsh-client-demo/client.js?rev="));
        ch.disconnect(id);
        std::fs::remove_dir_all(&_root).ok();
    }

    /// rebuilt 帧：`data: {JSON}\n\n`，JSON 字段语义对齐 TS `sseData({type:'rebuilt',id,rev})`
    /// （框架 + type/id/rev 三字段；键序由 BTreeMap 决定，浏览器按名解析，无语义影响）。
    #[test]
    fn rebuilt_line_wire_format() {
        let line = rebuilt_line("@deepseek-ai/dsh-client-demo", "abc123");
        assert!(line.starts_with("data: "), "SSE data line prefix: {line}");
        assert!(line.ends_with("\n\n"), "SSE frame terminator: {line}");
        let payload = line.strip_prefix("data: ").unwrap().trim_end_matches('\n');
        let v: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(v["type"], "rebuilt");
        assert_eq!(v["id"], "@deepseek-ai/dsh-client-demo");
        assert_eq!(v["rev"], "abc123");
    }

    /// 初次扫描建立基线后，无变化 → poll_once 返回空（通道空闲）。
    #[test]
    fn poll_once_idle_when_no_change() {
        let (_root, m) = make_manifest();
        let ch = HmrChannel::new(&m);
        assert!(ch.poll_once().is_empty(), "baseline scan is not a rebuild");
        std::fs::remove_dir_all(&_root).ok();
    }

    /// 内容变化（size 变）→ poll_once 报 rebuilt（含新 rev），再扫一次稳定为空。
    #[test]
    fn poll_once_reports_rebuilt_on_content_change() {
        let (_root, m) = make_manifest();
        let ch = HmrChannel::new(&m);
        assert!(ch.poll_once().is_empty());
        std::fs::write(bundle_path(&m), "load('v2'); /* changed */").unwrap();
        let rebuilt = ch.poll_once();
        assert_eq!(rebuilt.len(), 1, "exactly one row changed");
        let (id, rev) = &rebuilt[0];
        assert_eq!(id, "@deepseek-ai/dsh-client-demo");
        assert_ne!(rev, &m.entries[0].rev, "new rev differs from boot rev");
        assert!(
            ch.poll_once().is_empty(),
            "second scan after rebuild is idle"
        );
        std::fs::remove_dir_all(&_root).ok();
    }

    /// 缺文件 → 标脏（无重建）；恢复后新内容 → rebuilt；再恢复基线 → 空闲。
    #[test]
    fn poll_once_dirty_when_missing_then_recovers() {
        let (_root, m) = make_manifest();
        let ch = HmrChannel::new(&m);
        std::fs::remove_file(bundle_path(&m)).unwrap();
        assert!(
            ch.poll_once().is_empty(),
            "missing file is dirty, not rebuilt"
        );
        std::fs::write(bundle_path(&m), "load('v3'); /* recovered */").unwrap();
        let rebuilt = ch.poll_once();
        assert_eq!(
            rebuilt.len(),
            1,
            "reappeared file with new content rebuilds"
        );
        assert_ne!(&rebuilt[0].1, &m.entries[0].rev);
        assert!(ch.poll_once().is_empty());
        std::fs::remove_dir_all(&_root).ok();
    }

    /// broadcast 送达活跃连接；对端 drop → 下次广播移除该连接。
    #[test]
    fn broadcast_delivers_and_removes_closed_connection() {
        let (_root, m) = make_manifest();
        let ch = HmrChannel::new(&m);
        let (_id, rx, _initial) = ch.connect();
        ch.broadcast(&[(
            "@deepseek-ai/dsh-client-demo".to_string(),
            "abc123".to_string(),
        )]);
        let line = rx.recv().unwrap();
        assert_eq!(line, rebuilt_line("@deepseek-ai/dsh-client-demo", "abc123"));
        // 对端关闭（receiver drop）→ 下一次广播把它移除，不泄漏连接。
        drop(rx);
        ch.broadcast(&[(
            "@deepseek-ai/dsh-client-demo".to_string(),
            "def456".to_string(),
        )]);
        assert_eq!(
            ch.client_count(),
            0,
            "closed connection pruned on next broadcast"
        );
        std::fs::remove_dir_all(&_root).ok();
    }
}

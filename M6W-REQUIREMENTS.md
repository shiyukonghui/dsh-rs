# M6W 需求结论：SQLite 接入 dsh web（持久化后端选项）

> 阶段：①需求分析（第一性原理 + 自上而下/自下而上）。本文件为阶段①可验收工件——
> 通过后才进入②系统设计。方法论只在分析阶段使用；本文件**不包含实现**。

## 1. 第一性原理：这件事的真正目标

`dsh web` 已具备会话持久化：`--session-dir` → JSONL 后端 → `PersistenceCoordinator`
（`SessionPersistence` 缝）→ `SessionHost`（store 观察者自动 create/append + flush 落盘 +
启动 `restore_all` 恢复快照）。D-091 已交付 SQLite 后端（`SqliteBackend: PersistenceBackend`，
backend 级 7 测绿），但**未接入 web**。

**根本目的**：让 dsh web 的会话持久化可以选 SQLite 作为物理后端（`--sqlite-store <file>`），
得到事务性、单文件、无需压缩模式仲裁的存储；且**接入不破坏既有 JSONL 语义与任何 RPC/loop 契约**。

**剥到不可再分的基本事实**：
- 缝存在且后端可插拔：`PersistenceCoordinator::new(Box<dyn PersistenceBackend>)`；
- `SessionHost` 的一切持久化副作用都经 `coord`（观察者 create/append、flush、restore_all、
  恢复回灌 `coord.append(id,&full)`）；
- 后端替换即可获得整条持久化链路，无需改 store/loop/RPC/下链。

## 2. 自上而下（Top-down）：目标分解

```
目标：dsh web 可选 SQLite 持久化
 ├─ 配置面：`--sqlite-store <file>` → WebConfig.sqlite_store（优先级 sqlite > jsonl > 内存）
 ├─ 构造面：SessionHost::with_sqlite(path)（SqliteBackend→coordinator→观察者→restore_all）
 ├─ 服务面：serve() 主机选择（含「同时给定」的显式警告，绝不清零）
 └─ 验收面：冷重启恢复 / 恢复后续写 / 优先级 / 零回归
每层验收标准见 §6。
```

## 3. 自下而上（Bottom-up）：现有条件校核

- `session_host.rs::new_impl(root: Option<PathBuf>)`：`Some` → JSONL backend → coordinator；
  观察者闭包（on_event → create+append+publish、on_flush → flush）、`restore_all`
  （`coord.list()` → `coord.inspect()` → `Session::from_restore` → `store.enter/announce` →
  回灌 `coord.create` + `coord.append(id,&full)`）——**全部与后端无关，可整体复用**。
- `coordinator.rs`：`list()`→`backend.list_snapshots()`、`inspect()`→`backend.load_stored()`
  —— SqliteBackend（D-091）均实现 ✓；`coord.append` 首 append 走 `materialize_batch`。
- `main.rs::web_main`：`--session-dir` → `WebConfig.session_dir` → serve()；新增 flag 同构。
- `sqlite.rs`（D-091）：backend 级 7 测绿（含跨 reopen 持久/surface 保真/repair/coordinator 无缝）。

**双视角冲突（回到第一性原理判定）——越级发现，必须回到早期工件修正**：
- JSONL `materialize_batch` = `write_tmp_then_publish`（**原子覆盖 / create-or-replace**）；
  seam.rs 文档字面写「重复 materialize 拒绝」，但 **JSONL 从未执行该拒绝**。
- `SessionHost::restore_one` 恢复后 `coord.append(id,&full)` 重灌游标 → 首 append 走
  materialize_batch。SQLite（D-091）实现为「重复 materialize → Err」，与恢复路径冲突：
  恢复后游标停留在 0/未对齐，**该会话后续 append 将永久失败**（`must start at cursor 0`）。
- 裁决：**SQLite materialize 改为 create-or-replace（原子覆盖，镜像 JSONL）**；同步修正
  seam.rs 文档措辞（移除「拒绝」的误导性字样）与 D-091 对应测试（`rejects_duplicate`
  → 幂等覆盖断言）。这是既有工件（D-091）的契约修正，按瀑布流**回到正确阶段修正工件**
  （设计阶段 D-092 一并裁决记录、编码阶段改测改码），不静默打补丁。

## 4. 目标 / 非目标 / 假设 / 约束

### 目标
- `dsh web --sqlite-store <file>` 会话经 SQLite 落盘/回读/冷重启恢复。
- 与 JSONL 并列可选（优先级 sqlite > jsonl > 内存），既有行为零回归。

### 非目标
- 不做 SQLite↔JSONL 双写或迁移工具（互斥选择；迁移工具是后续课题）。
- 不改 loop 语义、RPC 形状（`session.history` / `session.prompt`）、SSE/WS 下链。
- 不做 SQLite 专属高级查询（仅持久化面，与 JSONL 等价）。

### 假设
- 单 web 进程一次挂一个持久化后端（host 单 coordinator）。
- SqliteBackend（D-091）backend 级验证可信；本阶段只做接线与集成验证。

### 约束（硬性）
- key/秘密纪律不变（无新增落盘面）。
- 单线程纪律（D-006）：SQLite 连接仅在 web 服务线程使用（RefCell 内部可变）。
- 既有 `--session-dir`（JSONL）路径零回归。
- 兼容既有恢复语义：restore_all 对 SQLite 复用（list/inspect/from_restore/回灌）。

## 5. 边界与取舍记录

- **config 面**：新增独立 flag `--sqlite-store <file>`（不污染既有 `--session-dir` 语义；
  不引入 scheme 前缀复用——避免 `sqlite:` 与路径歧义）。
- **同时给定**：`--sqlite-store` + `--session-dir` → sqlite 生效 + `eprintln!` 显式警告
  （fail-loud，绝不清零静默）。
- **文件面**：db 文件父目录不存在时 `create_dir_all`（镜像 JSONL 惰性建根）。
- **恢复面**：快照读取/恢复失败不阻断启动（沿用 restore_all 既有的逐会话跳过语义，
  错误在诊断路径可见）。
- SQLite 无 per-session artifact（`supports_raw_artifacts=false`、`read_raw=None`）——
  与既存 JSONL 的 raw-artifact 能力差异如实保留（非本阶段引入）。

## 6. 验收标准（Acceptance Criteria，进入设计的通过条件）

| # | 验收项 | 验证方式 |
|---|--------|----------|
| A1 | `with_sqlite` 落盘→冷重启→恢复 | 单测：adopt+flush → 新 host(same file) → is_live + events 恢复（含 end-seed 边界标记） |
| A2 | 恢复后继续写 seq 连续 | 单测：恢复后 adopt → 13 事件 / seq 12，不因游标错位失败 |
| A3 | serve 主机选择优先级 + 显式警告 | 单测：sqlite>jsonl>内存；同时给 → eprintln 警告 |
| A4 | backend materialize create-or-replace 修正 | dsh-persistence 单测：幂等覆盖；`rejects_duplicate` 移除 |
| A5 | 零回归 | workspace 全量 test 绿 + clippy `-D warnings` 零告警 + check 绿（含既有 JSONL 两测） |
| A6 | 文档/决策可追溯 | D-091 修正 + D-092 脚本化（commit/DECISIONS 互查）；部署/回滚说明 |

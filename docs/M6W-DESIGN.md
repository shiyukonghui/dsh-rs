# M6W 系统设计：SQLite 接入 dsh web

> 阶段：②系统设计。本文件为阶段②可验收工件——编码（③）前须通过。
> 承接 M6W-REQUIREMENTS §6 验收标准 A1–A6。设计决策同步落 DECISIONS D-092。

## 1. 设计约束回顾（来自需求结论）

- 复用既有 `SessionPersistence` 缝与 `SessionHost` 持久化链路（观察者/restore_all），
  后端可插拔（`PersistenceCoordinator::new(Box<dyn PersistenceBackend>)`）。
- config 面独立 flag `--sqlite-store <file>`，优先级 sqlite > jsonl > 内存，冲突显式警告。
- 越级修正：SQLite `materialize_batch` 改 create-or-replace（镜像 JSONL 原子覆盖），
  并修正 seam.rs 误导性文档与 D-091 遗留测试。

## 2. 组件设计

### 2.1 `dsh-persistence`（D-091 契约修正 + seam 措辞）
`crates/dsh-persistence/src/sqlite.rs` `materialize_batch`：
- 事务内 create-or-replace：`INSERT OR REPLACE INTO sessions(id, header, revision)` +
  `DELETE FROM events WHERE id=?` + 首批次写入 + `bump_revision`；
- 保留「首批次 seq0 + 连续性」校验（既存语义）；
- 移除「重复 materialize → Err」路径（与 JSONL 一致）。
`crates/dsh-persistence/src/seam.rs`：`materialize_batch` 文档改为 create-or-replace 语义。

### 2.2 `dsh-cli::session_host`（`SessionHost::with_sqlite`）
`crates/dsh-cli/src/session_host.rs`：
- 新 `SessionHost::with_sqlite(path: &Path) -> Rc<Self>`：
  父目录不存在则 `create_dir_all`（镜像 JSONL 惰性建根）→ `SqliteBackend::open(path)`
  （失败 fail-loud）→ `PersistenceCoordinator` → 共享观察者接线 → `restore_all()`。
- 重构：把观察者接线从 `new_impl` 提取为 `fn new_from_backend(Option<Box<dyn PersistenceBackend>>)`
  （单一观察者来源）；`with_root`/`with_sqlite` 各自构造后端后复用。
- 新诊断 `persistence_kind(&self) -> &'static str`（"mem"/"jsonl"/"sqlite"，构造时标记），
  供优先级/诊断测试断言（不新增效果面）。

### 2.3 `dsh-cli::web`（serve 主机选择 + 优先级警告）
`crates/dsh-cli/src/web.rs`：
- `WebConfig` 新增 `sqlite_store: Option<PathBuf>`；
- 提取 `fn session_host_for(cfg: &WebConfig) -> Rc<SessionHost>`：
  `Some(sqlite_store)` → 若同时有 `session_dir` 则 `eprintln!` 显式警告（fail-loud，不清零）
  → `SessionHost::with_sqlite`；`None`+`Some(session_dir)` → `with_root`；全无 → `in_memory`；
- `serve()` 以 `session_host_for(&cfg)` 替换第 177–180 行 match。

### 2.4 `dsh-cli::main`（CLI 面）
`crates/dsh-cli/src/main.rs`：
- 新 flag `--sqlite-store` 解析 → `sqlite_store`；
- `WebConfig` 构造补 `sqlite_store` 字段；用法注释（doc 头）补 `[--sqlite-store <file>]`。

## 3. 键路径（编码后将由测试背书的行为）

```
启动：serve(cfg) → session_host_for(cfg)
  └─ cfg.sqlite_store=Some(f)  ─┐
       (若 session_dir 同时给定 → eprintln 警告)
       └─ SessionHost::with_sqlite(f)
            = SqliteBackend::open(f) → PersistenceCoordinator
              → 观察者(create/append/flush) → restore_all()
                 = coord.list()→inspect()→Session::from_restore→enter/announce
                   → 回灌 coord.create+append(id,&full)   [materialize create-or-replace 幂等]

运行：store.append → 观察者 → coord.create(幂等)+coord.append → SqliteBackend 落盘
             store.flush → 观察者 → coord.flush
重启：同一 f 冷启动 → restore_all 恢复快照；恢复后 adopt → seq 连续（cursor 对齐）
```

## 4. 测试设计（TDD 红→绿；阶段③逐步实现）

| 用例 | 位置 | 断言（对应验收） |
|------|------|------------------|
| `with_sqlite_restart_restores_snapshot_into_store` | session_host.rs | A1：adopt+flush → 重启 with_sqlite → is_live + 7 事件（含 end-seed） |
| `with_sqlite_restore_then_adopt_continues_seq` | session_host.rs | A2：恢复后 adopt → 13 事件 / seq 12，无游标错位错误 |
| `persistence_kind_reports_sqlite_and_memory` | session_host.rs | 诊断：with_sqlite→"sqlite"；with_root→"jsonl"；in_memory→"mem" |
| `serve_session_host_precedence_sqlite_over_jsonl` | web.rs | A3：双设定 → 写落 sqlite 文件、jsonl 根为空（sqlite 生效） |
| `sqlite_materialize_is_idempotent_create_or_replace` | dsh-persistence/sqlite.rs | A4：二次 materialize 覆盖（load 见新事件），非 Err |

既有 JSONL 两测（restart_restores / restore_then_adopt）保持常绿 = A5 追加证据。
红阶：`SessionHost::with_sqlite` / `session_host_for` / `new_from_backend` 缺失（E0425/E0599）。

## 5. 部署 / 回滚（阶段⑤工件，先于实现定型）

- **部署**：`dsh web <cordis.yml> [--agent-loop …] --sqlite-store <file>`；启动打印
  实际恢复会话数（restore_all 返回）供诊断；db 单文件便于备份。
- **回滚**：撤 D-092 提交（含 Cargo.lock 无涉）回既有 JSONL/内存行为；或直接改用
  `--session-dir`；删 db 文件即回到全新存储（事务无 torn）。
- 决策链 D-091（修正）→ D-092 与提交互查。

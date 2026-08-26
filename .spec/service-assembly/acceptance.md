# 验收报告：服务装配单元 Phase 1（服务插件 entry 化 + A1 身份键 + A7 持久化写回）

日期：2026-08-26
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件。
依据：`.spec/service-assembly/requirements.md`（需求定稿）· `.spec/service-assembly/design.md`（设计定稿）。

---

## 1. 交付范围（requirements.md §1 逐条对应）

| 需求验收项 | 实现 | 证据 |
|---|---|---|
| E1 服务插件 entry 化：新增自定义服务 entry 可声明装配、按名解析、apply 生效、零新增 boot 特判 | boot/hmr 只认 `config.wasm` 为 loop；`register_host_service_plugins` 登记面 + `boot_with_host_plugins` | D-119 / commit `849aaf8`；m9_boot 21/21 |
| E2 A1 身份键 = 实现为本身份（与 harness 一致） | `PluginIdentity(Arc token)`/`PluginRecord+generation`；同名同实现幂等/同名新实现换代；Entry 记录身份 | D-118 / commit `2a509f8`；m16_identity 4/4 |
| E3 A7 持久化写回：运行时 create/update/remove 真实写回 cordis.yml，重启恢复 | loader `PersistSink` seam(fail-loud) + `attach_config_persist`(原子写主配置) + serve 接线 | D-120 / D-122 / commit `7e763a1`+`c15a884`；m17_persist 4/4 + 重启恢复 e2e |
| E4 等价性：服务依赖激活 dsh-diff golden | `loader-13`（loader 按名 entry + provide→inject 等待激活→卸载→再激活）TS golden ↔ Rust 逐行一致 | D-121 / commit `06baf72`；`node verify-diff.mjs` 18/18 PASS |

## 2. 测试验证（阶段 4）逐条证据

1. **全 workspace 全绿**：`cargo test --workspace` → EXIT=0，无失败/panic（dsh-cli 含 m9_boot 21/
   m16_identity、m17_persist、dsh-loader 全量等新套件）。
2. **clippy 零告警**：`cargo clippy --workspace --all-targets -- -D warnings` → EXIT=0。
3. **等价性**：`node diff/ts-host/verify-diff.mjs` → **18/18 PASS**（含新 `loader-13-service-entry-
   dependency-activation.golden` 27 行与 Rust 逐行一致；既有 17 场景 golden 逐字节未变零回归）。
   分钟级：`loader-create:consumer`（PENDING 等待 svc，无 apply）→ `loader-create:provider` →
   `apply:provider` → `provide:svc:"v1"` → `status:consumer:Pending:Loading` → `apply:consumer` →
   `log:consumer-applied` → Active；remove provider → 双 Unload → 再挂载 → 再激活。
4. **红→绿纪律**：S1-S3 每步先写红测（m16/m17/m9_boot 新用例引用尚不存在 API → E0599/E0425 编译
   红），实现后转绿；S4 的 golden 由 TS 原版 cordis（vendored `@deepseek-ai/cordis-plugin-loader`）
   生成，Rust 首次对齐即 PASS。
5. **回归**：`cargo test -p dsh-core -p dsh-loader -p dsh-diff -p dsh-wasmrt -p dsh-cli` 每步全绿；
   `dsh web` / `--agent-loop` 路径零回归（见 §3 部署冒烟）。

## 3. 部署与维护（阶段 5）

### 3.1 活体冒烟（真实 serve，生产配置 target/web/cordis.yml）
- `dsh web target/web/cordis.yml --port 60881 --web-root <harness-dist> --workspace-root target/web-workspace
  --sqlite-store target/web/sessions.sqlite`：`/` **HTTP 200**（前端 dist 服务正常）、`/api/host.describe`
  RPC 返回真实宿主信息（cwd/model/provider/attachedSessions），stderr 干净——boot + serve 装配零回归，
  persist seam 已挂载（D-122）。
- **agent-turn 冒烟按门控纪律诚实跳过**：无 `DEEPSEEK_API_KEY`（P1/D-077 裁定无 key → agent.turn
  fail-loud AUTH，不伪造）；key 仅进程环境注入，永不落盘入 git（P4）。
- 冒烟进程已停止（不占 `target/debug/dsh.exe`，后续测试不受锁）。

### 3.2 运行方式
```
dsh web <cordis.yml> [--overlay ...] [--agent-loop] [--llm-base-url ...] [--llm-model ...]
```
- cordis.yml 追加一行服务插件 entry（name 指向 `register_host_service_plugins` 已登记的宿主插件）
  → 声明即装配；运行时动态装配（dynamicCordisRunner loader.create/remove）自动原子写回 cordis.yml
  （D-122 接线），重启恢复。
- 未登记的 name → boot `unknown plugin {name}` fail-loud（诚实）。

### 3.3 回滚（逐独立提交）
- S1 身份键：`git revert 2a509f8`（仓库键型；独立回滚点）。
- S2 entry 化：`git revert 849aaf8`（boot/hmr 判定；独立）。
- S3 写回：`git revert 7e763a1`（PersistSink seam；独立）。
- 部署接线：`git revert c15a884`（WebConfig 字段 + serve 三行；独立）。
- S4 差分：`git revert 06baf72`（纯 diff 基建；独立）。
- 各步互不耦合，可单独回退而保持其余装配面可用。

### 3.4 维护注意
- 先前遗留的 60880 演示服务因占用 `target/debug/dsh.exe` 于 S1 前被 Stop-Process（D-118 记录）；
  需要时以原命令行重启（用户决定）。本次冒烟用 60881 完成验证后已停。
- A7 写回为「权威入口列表 → 主 cordis.yml 原子写」：overlay 变更会物化进主文件（DIV-2）；
  YAML 注释不保真（D-086 沿用）——配置文件属机器写受众。

## 4. 诚实边界（明确非本次交付）

- A2 `!!js` 条件装配（D-S3=记录为边界，spike 另立）；A3/A4 依赖激活核对、A5 intercept 合并、
  A6 生成器 effect、B 类对齐项（extend/invoke、Group 折叠、HMR 模块热更、Config.simplify 完整版）——
  均留后续阶段（handoff §3 清单）。
- A1 完整 HMR 换代链路（B3）后续；本阶段完成注册语义 + 可观察身份 + Entry 记录。
- 前端组件行的 Rust 引擎激活——显式排除（D-S1，另一条大线）。
- 浏览器端真实 E2E（`--dump-dom` / 逐帧断言）按仓纪律属浏览器验收通道；本阶段以 handle_rpc /
  serve 冒烟 + dsh-diff 代偿（与 D-022/D-036/D-077 同口径）。

## 5. 决策链与 git 互查
requirements（`8cfd547` D-116）→ design（`78ba136` D-117）→ S1（`2a509f8` D-118）→ S2（`849aaf8`
D-119）→ S3（`7e763a1` D-120）→ S4（`06baf72` D-121）→ 部署接线（`c15a884` D-122）。改动 → 提交 →
DECISIONS 条目逐条可互查。

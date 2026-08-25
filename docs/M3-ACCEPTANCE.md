# M3 验收报告（M3-ACCEPTANCE）

**阶段**：M3 宿主方法面 + settings/credentials/guard（M3a → M3e 编码 + M3f 验收）
**结论**：✅ **通过**——7/7 验收标准全部满足；workspace 全绿 + clippy 零告警 + 每子步
DECISIONS/git 互查。

---

## 1. 验收标准逐项核对（M3-REQUIREMENTS §5）

| # | 验收标准 | 证据 |
|---|---|---|
| 1 | `cargo test --workspace` 全绿；clippy `-D warnings` 零告警 | ✅ 见下「验证矩阵」 |
| 2 | host.listDirectory/createDirectory 真实 fs 场景断言 | ✅ `dsh-cli/host_dir.rs` 12 测试 + web.rs `rpc_host_list_directory_real_fs`/`rpc_host_create_directory_real_fs`（temp 目录含子目录/点文件/大目录、hidden/truncated/错误文案逐字） |
| 3 | settings 注册→describe(redact)→update(merge)→mutate(path-op)→replace(reset)→revision/conflict；文件落盘→重启恢复 | ✅ `dsh-settings` 11 测试（8 lib + `yaml_persist_and_reload`）+ `rpc_settings_full_wire_real_driver` 7 段接线；`dsh-settings/runtime.rs` `SettingsProvider` |
| 4 | credentials resolve/describe/set/unset + env 遮蔽拒 + 空值拒 + 幂等 unset；`.credentials.yaml` 落盘恢复 | ✅ `dsh-credentials` 8 测试（`file_set_resolve_unset_roundtrip` 落盘恢复）+ `rpc_credentials_full_wire_real_driver` 8 段接线 |
| 5 | web.rs 12 方法（settings 5 + credentials 3 + host 4 目录类）经 handle_rpc_host 全真实服务驱动 | ✅ web.rs 29 测试（settings.describe/update/replace/mutate/openDocument、credentials.describe/set/unset、host.describe/pickDirectory/listDirectory/createDirectory/openPath）——空桩全部移除 |
| 6 | guard：TOOL_TIMEOUT 消息逐字 + 阈值提醒逐字（gentle/detailed） | ✅ `dsh-tools/guard.rs` 22 测试（`tool call timed out after {ms}ms`、GENTLE_REMINDER、detailedReminder 全逐字） |
| 7 | 每子步 DECISIONS 条目 + git 提交互查 | ✅ D-037…D-043 + 提交 f7c698f/b61a1fa/3da232b/bd5e853/c81cf18（见下） |

---

## 2. 验证矩阵（本轮实际运行）

- `cargo test --offline --workspace`：全绿（0 failed；dsh-cli 55、dsh-tools 22+回归、dsh-settings 11、dsh-credentials 8、host_dir 12、web 29 等）。
- `cargo clippy --offline --workspace --all-targets -- -D warnings`：零告警。
- 各独立门禁均已通过：dsh-schema to_json 11 绿 / dsh-persistence atomic_write 3 绿 / dsh-settings 11 绿 / dsh-credentials 8 绿 / dsh-tools guard 22 绿 / web 29 绿。

---

## 3. M3 子步提交链（DECISIONS ↔ git 互查）

| 子步 | 交付 | DECISIONS | 提交 |
|---|---|---|---|
| 需求分析（reprise） | M3-REQUIREMENTS.md（需求结论 + 设计） | D-037 | d1765fd |
| M3a | host 目录方法面（host_dir.rs + web 接线） | D-038 | f7c698f |
| M3b | dsh-schema::to_json/extra/secret + dsh-persistence::fs_atomic + dsh-settings | D-039 | b61a1fa |
| M3c | dsh-credentials（env→file 两层 + shadowed/空值 guard + 持久化） | D-040 | 3da232b |
| M3d | web.rs 接线（settings 5 + credentials 3 + host.describe provider/model） | D-041 | bd5e853 |
| M3e | guard 切片（timeout-policy + repeat-tool-reminder） | D-042 | c81cf18 |
| M3f | 本报告 + 工作树收口 | D-043 | 本提交 |

## 4. 边界不变量（M3-REQUIREMENTS §5）——全部断言覆盖

- 任何 wire settings 值已 redact（无 `role('secret')` 残留）——`redact.rs` 测试 + `rpc_settings_full_wire_real_driver`（token 缺席 user）。
- `credentials.resolve()` 永不返回空串（空 = 未配置）——`resolve` 跳过空值 + `rejects_empty_value`。
- set/unset 在 env 遮蔽时必拒（shadowed）——`env_layers_readonly_and_wins` + RPC 层 `credential-rejected`。
- settings 写 revision 冲突必 `SETTINGS_CONFLICT`——`stale_revision_conflict` + RPC 层 wire code。
- host 目录操作绝不相对路径重基（fully-qualified 围栏）——host_dir 测试。

## 5. 差异记录（对 TS 语义的非目标/降级，D-037 已声明）

- 无 OS 级 settings/credentials 文件 watch（写路径自一致 + 启动读）；无 YAML 注释保真
  leaf-diff（全 YAML 重写 `{ns:section}`）。
- credentials 只做 env→file 两层（project/env dotenv 留后续）。
- settings revision 不持久化（新进程从 0 起）。
- guard timeout 用同步 wall-clock 后置度量（无并发抢占，D-004 单线程）；真抢占留 M4/M5。
- pickDirectory `{path:null}` / openPath `{opened:true}` / settings.openDocument `{opened:true}`
  均为无 native dialog/桌面 opener 的诚实降级。
- llm.discoverModels 保持 `{models:[]}`（真实 provider/凭据 M4+）。

## 6. 遗留与下一里程碑（M4 候选）

- guard 的 agent-loop 完整接线（post-execute 折叠 reminders、deadline 信号抢占）。
- credentials records half（grant/api-key）。
- 真浏览器 E2E（本环境不可跑；handle_rpc_host 集成已代偿，延续 D-022/D-036）。
- settings 文件 provider 缺省 `$DSH_HOME/settings.yaml` 接线（当前 Boot 用 memory + 显式注册）。

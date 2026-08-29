# 验收结论：面板改写 #3 —— panel-dynamic-plugins（动态插件清单卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-188** | git：本提交（上游 `013a8f1`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | v2 list 卡契约 | m35 `describe_ui_returns_valid_list_declaration` | ✅ |
| S2 | 一份契约 | m35 `static_ui_json_matches_describe_ui` | ✅ |
| S3 | 行投影（running/defined、name 取 currentPackageId 包，回落首包） | m35 `list_projects_dynamic_plugins` | ✅ |
| S4 | 服务失败 fail-loud 无 items | m35 `list_service_failure_is_fail_loud` | ✅ |
| S5 | 未知端点 fail-loud；scan 挂载 + 清单第四卡 | m35 `unknown_endpoint_fail_loud` + `scan_mounted_units_appear_in_manifest` 扩断言 | ✅ |
| S6 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m32/m33/m34/m35）、clippy **0**、node **16/16** | ✅ |

## TDD 记录
m35 先对不存在包红（FAILED，构建不可达）→ 包落地 **5/5**；构建一次通过（无类型事故）。

## 诚实台账
1. **只读边界**：stop/undefine 等写动作未做——宿主 `set` 面（dynamicStop/dynamicUndefine）
   现成，但「卡内破坏性动作确认」的渲染形态未定契约，写动作卡留待独立流程先行定契约。
2. 浏览器端到端手测仍缺（无浏览器基建）；数据面形状与 `listRows` 契约位双向锁定。
3. 进度 **3/N**（台账 progress.md）：#1 静态装配、#3 动态装配、#2 聚合状态、试点 form——
   三种渲染档（form/list/status）都有真实卡，改写型经三次复制证明可批量。

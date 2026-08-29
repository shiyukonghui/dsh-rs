# 验收结论：面板改写 #4 —— panel-workspace-files（工作区文件卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-190** | git：本提交（上游 `32e81bc`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | v2 list 契约 + type resource | m36 `describe_ui_returns_valid_list_declaration` | ✅ |
| S2 | 一份契约 | m36 `static_ui_json_matches_describe_ui` | ✅ |
| S3 | 行投影 + 调用序 = 探测序 | m36 `list_projects_workspace_files`（桩记录断言 `["agentWorkspace","workspaceFiles"]`） | ✅ |
| S4 | 解析失败 fail-loud 且**零枚举调用** | m36 `list_fail_loud_when_workspace_probe_fails`（调用记录只有一条） | ✅ |
| S5 | 枚举失败透传 | m36 `list_enumeration_failure_passthrough` | ✅ |
| S6 | 未知端点 fail-loud | m36 `unknown_endpoint_fail_loud` | ✅ |
| S7 | scan 挂载 + 清单第五卡 | `scan_mounted_units_appear_in_manifest` 扩 resource 卡断言（宿主零改动） | ✅ |
| S8 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m32/m33/m34/m35/m36 齐）、clippy **0** | ✅ |

## TDD 记录
m36 先对不存在包红（FAILED + 构建不可达）→ 包落地 **6/6**；「失败零枚举」断言让
「不猜目录」纪律成为可执行契约（任何在解析失败后仍触达枚举的实现必红）。

## 诚实台账
1. 真实 fs 语义（目录缺失 → workspaceFiles 返回空 paths 而非错误）由宿主投影既有行为
   决定——卡显示「工作区没有文件」；「工作区未配置」是错误态（agentWorkspace 失败），
   两态在卡上分列，文案不混用。
2. 递归树/搜索框/文件操作未做（需求 §4 边界）；浏览器端到端手测仍缺（无基建，纯函数
   + 端点面已全测）。
3. 进度 **4/N**：分类覆盖 model（试点）/ runtime（#1/#2/#3）/ **resource（#4）**。

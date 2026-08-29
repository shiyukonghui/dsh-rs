# 验收结论：面板改写 #5 —— panel-sessions（会话清单卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-191** | git：本提交（上游 `cdcaf21`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | v2 list 契约 + type session | m37 `describe_ui_returns_valid_list_declaration` | ✅ |
| S2 | 一份契约 | m37 `static_ui_json_matches_describe_ui` | ✅ |
| S3 | 行零加工（epoch 原样）+ 单服务探测 | m37 `list_projects_session_candidates_verbatim`（调用计数=1） | ✅ |
| S4 | 服务失败 fail-loud 无 items | m37 `list_service_failure_is_fail_loud` | ✅ |
| S5 | 未知端点 fail-loud | m37 `unknown_endpoint_fail_loud` | ✅ |
| S6 | scan 挂载 + 清单第六卡 | `scan_mounted_units_appear_in_manifest` 扩 session 卡断言 | ✅ |
| S7 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m32–m37 齐）、clippy **0** | ✅ |

## TDD 记录
m37 先对不存在包红（FAILED + 构建不可达）→ 包落地 **5/5**。

## 诚实台账
1. **发现端先行的边界**：列举真实（sessionCandidates = sessionReferenceResolver 同源投影），
   但「点开会话」未实现——卡上无任何暗示可点的 affordance（诚实的只读）。
2. 时间列显示原始 epoch ms（丑但诚实）；date 类格式化属渲染器演进（columns.type 已预留）。
3. 进度 **5/N**：分类覆盖 model / runtime×3 / resource / **session**——D-181 分类表
   四个已证语义位全部有真实卡。

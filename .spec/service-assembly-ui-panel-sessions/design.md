# 设计结论：面板改写 #5 —— panel-sessions

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-191**。C4 改写型第五次复制（零新型）。

## 1. 包与声明
`wasm-plugins/panel-sessions/`（照型）。声明：cardId `panel-sessions.list`、type `session`、
size 4×4、view list、dataRpc `["panel-sessions","list"]`、columns
`[{sessionId,"会话"},{createdAt,"创建 (epoch ms)"}]`、rowsPath items、emptyText「还没有会话」。

## 2. `list` 端点
`get("sessionCandidates", {})` → `ok!=true` 透传；`candidates[]` 逐行取
`{sessionId,label,createdAt}` 原样直传（缺失字段 → null，不造值）→
`{ok:true, value:{items}}`。

## 3. 测试（m37：红→绿）
`describe_ui_returns_valid_list_declaration`（type=session）/ `static_ui_json_matches_describe_ui` /
`list_projects_session_candidates_verbatim`（epoch 原样 + 调用计数=1）/
`list_service_failure_is_fail_loud` / `unknown_endpoint_fail_loud`；
清单联动 `scan_mounted_units_appear_in_manifest` 扩第六卡。

## 4. 回滚点
撤包目录 + m37 + 清单断言行 = 回到 `cdcaf21`。

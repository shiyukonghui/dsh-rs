# 设计结论：面板改写 #4 —— panel-workspace-files

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-190**。C4 改写型第四次复制（零新型）。

## 1. 包（照型）与声明

`wasm-plugins/panel-workspace-files/`（wit/lib/plugin.json/lock 照抄型 + web/ui.json）。
声明：cardId `panel-workspace-files.list`、type `resource`、size 4×4、view list、
dataRpc `["panel-workspace-files","list"]`、columns `[{path,"文件路径"}]`、rowsPath items、
emptyText「工作区没有文件」。

## 2. `list` 端点（两段式，每段诚实失败）

1. `get("agentWorkspace")`：`ok!=true` → 透传；`cwd` 空/非串 → `no-workspace` fail-loud
   ——**此后不得调用枚举**（不猜目录；m36 以桩调用记录断言调用序）；
2. `get("workspaceFiles", {cwd, query:""})`：`ok!=true` → 透传；
3. 行：`paths.map(p => {path:p})` → `{ok:true, value:{items}}`。

## 3. 测试（m36：红→绿）

`describe_ui_returns_valid_list_declaration`（含 type=resource 断言）/
`static_ui_json_matches_describe_ui` / `list_projects_workspace_files`（调用序断言）/
`list_fail_loud_when_workspace_probe_fails`（**零枚举调用**断言）/
`list_enumeration_failure_passthrough` / `unknown_endpoint_fail_loud`；
清单联动：`scan_mounted_units_appear_in_manifest` 扩第五卡（resource）。

## 4. 回滚点
撤包目录 + m36 + 清单断言行 = 回到 `32e81bc`。

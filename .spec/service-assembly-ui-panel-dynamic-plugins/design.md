# 设计结论：面板改写 #3 —— panel-dynamic-plugins

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-188**。C4 改写型第三次复制（零新型）。

## 1. 包结构（照 panel-plugin-inventory 型）

```
wasm-plugins/panel-dynamic-plugins/
  wit/panel-dynamic-plugins.wit     # 复用 dsh:host-remote 接口身份
  src/lib.rs                        # describeUI + list（dynamicPlugins 投影行化）
  plugin.json                       # { wasm 约定路径, web:"web", caps:["remote"], world:"remote" }
  Cargo.lock                        # 离线照抄改根包名
  web/ui.json                       # 与 describeUI 逐字段一致
```

## 2. 声明（web/ui.json == describeUI）

v2 card：cardId `panel-dynamic-plugins.list`、type `runtime`、title「动态插件」、
size 4×4、view：kind list、dataRpc `["panel-dynamic-plugins","list"]`、
rowsPath `items`、columns `[{pluginId,"插件 ID"},{name,"包名"},{state,"状态"}]`、
actions []（只读，写动作留待"卡内确认"渲染形态的契约演进）、
emptyText「没有已定义的动态插件」。

## 3. `list` 端点（wasm 内）

1. `host_services.get("dynamicPlugins", {})`；`ok!=true` → 透传错误（无 items）；
2. 行投影（单元自持语义）：对每个 plugin：
   - `name` = packages 中 `packageId == currentPackageId` 的 name（缺配回落首包 name / null）；
   - `state` = `activeRun` 存在 → `"running"`，否则 `"defined"`；
3. `{ok:true, value:{items:[{pluginId,name,state}]}}`。

## 4. 测试（m35，先红后绿）

`describe_ui_returns_valid_list_declaration` / `static_ui_json_matches_describe_ui` /
`list_projects_dynamic_plugins`（running+defined、name 取当前包）/
`list_service_failure_is_fail_loud` / `unknown_endpoint_fail_loud`；
清单联动：`scan_mounted_units_appear_in_manifest` 扩第四卡断言。

## 5. 回滚点
撤包目录 + m35 + 清单断言行 = 回到 `013a8f1`。

# 设计结论：面板改写 #2 —— panel-runtime-status

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-187**。C4 改写型完全复制（零新型）。

## 1. 包结构（照 panel-plugin-inventory 型）

```
wasm-plugins/panel-runtime-status/
  wit/panel-runtime-status.wit        # 复用 dsh:host-remote 接口身份
  src/lib.rs                          # describeUI + status（双服务聚合）
  plugin.json                         # { wasm 约定路径, web:"web", caps:["remote"], world:"remote" }
  Cargo.lock                          # 离线照抄改根包名
  web/ui.json                         # 与 describeUI 逐字段一致
```

## 2. 声明（web/ui.json == describeUI）

```jsonc
{ "$schema":"dsh/plugin-ui/v2", "kind":"card",
  "cardId":"panel-runtime-status.status", "type":"runtime",
  "title":"运行时状态", "description":"loader / 动态包实时聚合（只读）",
  "size":{ "w":2, "h":2 },
  "view":{ "kind":"status",
    "dataRpc":["panel-runtime-status","status"],
    "actions":[] } }   // 刷新 = 渲染器 affordance（C4 既定），items 全由数据面驱动
```

## 3. `status` 端点逻辑（wasm 内，单元自持）

1. `get("loader")` → entries（group 过滤）：`total / active(fiber!=null) / disabled`；
2. `get("dynamicPlugins")` → plugins 数组长度 `dyn`；
3. 任一 `ok!=true` → 透传 `{ok:false,error}`（**不部分伪造**，S4）；
4. items：
   - `{label:"loader 条目", value:total, kind:"number"}`
   - `{label:"fiber 活跃", value:active, kind:"number", tone: active>0?"ok":"idle"}`
   - `{label:"禁用", value:disabled, kind:"number", tone: disabled>0?"warn":"ok"}`
   - `{label:"动态包", value:dyn, kind:"number", tone:"idle"}`
   → `{ok:true, value:{items:[…]}}`（statusItems 契约的 `value.items` 位）。

## 4. 测试（m34，仿 m32/m33；红→绿）

1. `describe_ui_returns_valid_status_declaration`（status/dataRpc/无静态 items/size 2×2 无坐标/type 闭集）
2. `static_ui_json_matches_describe_ui`
3. `status_aggregates_loader_and_dynamic_plugins`（双服务桩：2 total/1 active/1 disabled/3 dyn → items 值与 tone 断言）
4. `status_fail_loud_when_any_service_down`（dynamicPlugins 挂 → ok:false 无 items）
5. `unknown_endpoint_fail_loud`
6. 清单联动：并入既有 `scan_mounted_units_appear_in_manifest`（第三卡断言，宿主零改动即得）

## 5. 回滚点
撤包目录 + m34 + 清单断言行 = 回到 `62b7802`；scan/watch/渲染器零改动。

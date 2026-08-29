# 设计结论：桌布 C6 —— rowActions 渲染 + `confirm` 契约收敛

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-189**。契约回写：canvas design §4.1（confirm 字段 + 行动作线形状）+ §13 加 C6 行。

## 1. 契约收敛（canvas design §4.1 的增量）

```jsonc
// list 视图 rowActions 项（形状既有，本轮定义行为）：
{ "name": "stop", "label": "停止", "rpc": ["panel-dynamic-plugins", "stop"],
  "scope": "row",          // v1 唯一取值："row"
  "confirm": true          // 新增可选字段：true → 渲染器执行前必须用户确认
}
// 动作参数线形状（渲染器职责）：
POST /api/<ns>/<m>  body = { args: { row: {<该行完整对象>} } }
// 单元职责：校验 row 内身份字段（缺失/类型错 → fail-loud，绝不信任渲染器）。
```

`confirm` 语义：**严格 `true` 才确认**（缺省/其他值 = 直接执行，向后兼容）；
v1 确认实现 = `window.confirm("对「<行主键/名称>」执行 <label>？")`。

## 2. 分层落点

```
core.js（纯函数，node 钉死）
  export function rowActionBody(row)            // { row: row }
  export function needsConfirm(action)          // action && action.confirm === true
  validateDeclaration：list 体校验扩展——
    view.rowActions 存在但非数组 → view-malformed
    每项缺 name / rpc 非 [ns,m] 二元组 → view-malformed
app.js paintList
  view.rowActions?.length → 表尾加「操作」列；按钮 click：
    needsConfirm && !window.confirm(...) → 直接 return（**不发 RPC**）
    rpc(rpc.join("/"), rowActionBody(row)) → 卡状态行；ok → 重放 load() 刷新
panel-dynamic-plugins（升级）
  src/lib.rs：stop/undefine 端点——row.pluginId 非空字符串校验 → 
    host_services.set("dynamicStop"/"dynamicUndefine", {pluginId}) → 透传
  describeUI + web/ui.json：view.rowActions 两项（均 confirm:true）——**一份契约继续 m35 守**
```

## 3. 测试计划（红→绿）

**node（core.test.mjs）**：`rowActionBody wraps the full row untouched` /
`needsConfirm only strict true` / `validateDeclaration rowActions malformed rows → view-malformed`
（非数组、缺 name、rpc 三元组三例）+ 合法 rowActions 的 list 卡直通。

**m35 扩展（Rust）**：
- `stop_requires_row_plugin_id_fail_loud`（空 body / 空串 / 非串三例；桩记录 set 调用数 = 0）
- `stop_passthrough_success`（set 桩 {ok:true} → 单元 ok:true；桩收到 ("dynamicStop", pluginId)）
- `stop_service_failure_passthrough`（桩 "not running" → ok:false 同码）
- `undefine_passthrough_and_validation`（成功 + 缺 pluginId 两路）

**回归**：全套 + clippy + node。

## 4. 回滚点
core/app 两处增量 + 单元两端点 + 声明一行 + 测试；撤销回到 `ffd7e20`。
confirm 是**新增可选字段**——旧声明零影响（无确认字段的既有卡行为不变）。

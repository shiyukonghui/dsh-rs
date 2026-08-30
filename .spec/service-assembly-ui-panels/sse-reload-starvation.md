# 缺陷档案：SSE 事件通道长会话饥饿（2026-09-05，P2 期间暴露，待专项 spike）

## 现象
长审计会话（多次 reload 累积后）中，页面 manifest 热更新失效：
- T7 目录 rename 卸载方向正常（≤700ms 落地）；rename **复原方向 15s+ 不落地**；
- 宿主侧完全正确：`m1=12 / mhost=13`（RPC 轮询铁证）；
- `Page.navigate` 重载后一切恢复（T9 恒绿）；
- 新鲜页面（e2e-readd.mjs 探针）同场景 700ms 内双向全绿（含 graph+双 manifest 帧捕获）。

## 复现线索（已固化的最小复现）
`PROFILE=<profile> SEED=1 node .spec/service-assembly-ui-panels/e2e-readd.mjs`：
种子 reload 一次后，页面自开 EventSource **零帧到达**（连连接即发的 graph 帧都没有）
→ 饥饿在「重载后的新页面」上即成立，无需长会话。

## 排除项（已实证）
- 宿主广播：探针曾捕获 `graph` + 双 `ui-manifest-changed` 帧（新鲜页全速）；
- 服务端清理：`hmr_events.rs` `retain(tx.send(..).is_ok())` 死客户端剪枝在位；
- 反节流旗无效（`--disable-background-timer-throttling` 等三连照旧卡死）；
- P2 代码无关：协商关对无声明单元=no-op；同 build 短时序审计（run#1）dom2 正常。

## 嫌疑（spike 待验）
1. **浏览器侧连接配额饥饿**：页面常驻 2 条 SSE（/plugins/events + /api/events.mux），
   每次 reload 若有任何连接未被真正关闭（headless 下尤其），6 连接/主机配额耗尽后
   新 EventSource 静默排队、fetch 也排队 → 「SSE 死 + 10s 兜底 poll 也死」的
   双盲态与观察完全吻合；重载释放 → 恢复，吻合。
2. shell SSE 句柄在 unload 时的关闭时机（Dioxus 的 spawn_local 持有的 Closure/EventSource
   是否有未显式 close 的路径）。

## 影响与定级
产品面：反复重载后页面事件通道退化（刷新即愈）——中等。
审计面：T7 `dom2` 降为观察字段（宿主面 m1/mhost 仍为硬断言）；dom2 卡死视为
**本缺陷指纹**而非热插拔回归。修复候选方向：unload 时显式 `es.close()`（interop
watch_manifest/watch_session_events 两处）+ 用 CDP Network 面板数连接做 spike 实证。

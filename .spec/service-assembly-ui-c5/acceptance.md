# 验收结论：桌布 C5 —— 热插拔 watch + `ui-manifest-changed` SSE

日期：2026-09-05 | 关卡：自主过闸（用户授权）| 决策记录 **D-186** | git：本提交（上游 `11021e8`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | 帧形状 `data:{type,rev}\n\n` | `ui_manifest_changed_line_wire_format` | ✅ |
| S2 | 装：新单元 → 挂载 + 广播 | `ui_manifest_watch_mount_edit_unmount_flow` 前段（packages/carriers 增长 + rx 收帧含新 rev） | ✅ |
| S3 | 改：ui.json 内容变 → rev 变 | 同测试中段（`rev2 != rev1`） | ✅ |
| S4 | 卸：删目录 → 卸载 + 广播；只动 scan 挂载 | 同测试后段 + `ui_manifest_watch_unmount_only_touches_scan_mounted`（boot-manifest 包无损） | ✅ |
| S5 | 节流窗口内不重扫 | `ui_manifest_watch_throttles_within_window`（合成时钟零等待） | ✅ |
| S6 | 缺构件：不挂载/不构建/不 panic | `ui_manifest_watch_skips_unit_without_component`（`mounted` 空 + `None`） | ✅ |
| S7 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m32 8/8、m33 5/5）、clippy **0**、node **16/16**、verify-diff ALL PASS | ✅ |

## TDD 记录
- 桩红：3 条 watch 流程 + S1 帧形状全部 FAILED（skip 测试桩下平凡 pass，属负向断言预期）。
- 实现转绿；**clippy 额外战果**：抓到 C2 编辑事故中 `#[test]` 被并入注释行、
  `disabled_entry_excludes_card` 长期未跑——复活后计入（251 = 246 + watch 4 + hmr 帧 1
  含复活计数校正）。

## 诚实台账
1. 真实「往 wasm_base 放目录 → 浏览器即时增删卡」端到端手测未执行（无浏览器基建）；
   同步/挂载/广播/rev 面已全测，EventSource 消费与 `pollDecision` 幂等属 DOM 边界层。
2. watch 只同步 **scan 挂载的 remote 单元**；loader entries 的 create/remove（dynamicCordisRunner
   路径）同样会改清单（disabled 交叉），其变化靠 tick 重算 rev 兜住——同一无死角机制。
3. `Box::leak` 载体名（与 dynamic_activate 同纪律）；反复装卸有界泄漏，已知可接受。

## 意义
「热插拔是第一等要求」自此**闭环**：装/卸/改装配单元 → 宿主 tick 同步 → `/plugins/events`
推送 → 桌布即时增删卡片；轮询仅兜底。C1–C5 桌布主线全部点亮。

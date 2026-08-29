# 取证反转：聊天中断——宿主面已在（session.cancel），缺的只是渲染器入口

日期：2026-09-05 | 教训复用：D-202「按动作逐个取证，不打包悲观」。

## 事实（本轮 grep 实证）
1. `session.cancel` = dispatch 原生臂（web.rs:3975）；D-114 真实 driver 取消接线：
   按会话定位驱动、幂等 accepted（测 10387）；**并发安全**——刻意排除长 RPC 白名单，
   turn 阻塞中 accept 线程立即送达（测 11153/11245，abort 落定 aborted 事件）。
2. 结论：D-193 划界的「聊天中断按钮未做」= **渲染器无入口**，宿主协议完备。

## 下轮实现锚点（按此直接开工，预计半轮收口）
1. **D-196 补漏**：dispatch 内 `"session.cancel"` 臂（3975）读 `payload.get("sessionId")`
   直读字段 → 画布 `{args}` 信封丢参。**先加一行遮蔽解包**（同 decide/settings 先例）。
2. **渲染器**：renderChat 发送行加「停止」按钮，仅当 `view.cancelRpc` 声明存在时绘制；
   体 = `rpc(view.cancelRpc.join("/"), { sessionId: 当前会话 })`；结果走既有 stat 行。
   无 core 纯函数（DOM 层，S3 先例；validateDeclaration 对 chat 视图额外键天然容忍）。
3. **声明三件套**：panel-chat ui.json + lib.rs 加 `"cancelRpc": ["session","cancel"]`，
   m39 声明测试补一键断言（wasm 重建，一份契约）。
4. **宿主测试**：`session.cancel` 画布形（payload {args:{sessionId}}）→ 遮蔽后仍
   accepted（沿用 10468 测的最小改造）。
5. 文档：D-203、E2E 清单 §1 聊天行 + §2 划账（中断项；附件维持边界）、台账 C8 行。

## 边界不变
附件/审批线仍属 D-193 划界；「停止」语义 = 取消当前驱动（不删历史）。

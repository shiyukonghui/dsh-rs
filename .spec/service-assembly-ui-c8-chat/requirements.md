# 需求结论：桌布 C8 —— chat 渲染契约（替代旧前端的最后一块主视图）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 **D-193（契约设计，实现排期）**。
原生面实证：`session.prompt` 长 RPC（web.rs:697/3798，content 首 text 块为输入）；
宿主事件 SSE `stream_sse_events`（hello + EventKind 帧 + approval 线，serve:1096）；
会话面 `{sessionId,updatedAt,running,blank}`（sessions.list）；投影注册表已有折叠先例。

## 1. 目标（第一性）

「前端全部由服务单元组成」的最后一个主视图 = 聊天。不可再"只读先行"——聊天的本质
是**双向 + 流式**。基本事实：
- 发送与历史可以走请求/响应（单元端点，既有型）；
- **流式事件不能经单元**（wasm 单元是请求/响应模型，无订阅原语；宿主 `host_services`
  同样没有）——但渲染器本来就直连宿主（fetch RPC 同源），**订阅宿主 SSE 是渲染器
  既有能力的自然延伸**，不需要单元代理，也不需要新后端。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 会话绑定 | 卡内会话选择器：`sessionSource` 数据面（list 形状，含 sessionId/label）驱动 `<select>`；卡内状态记住所选 sessionId；无选择器时默认会话 |
| 2 | 历史 | `historyRpc` 单元端点 `{sessionId}` → `{messages:[{role,text,ts}]}`（折叠归单元，投影注册表先例） |
| 3 | 发送 | `sendRpc` 单元端点 `{sessionId,text}` → 单元转宿主（见设计 §3 裁决）；发送即追加本地乐观气泡，流事件对齐 |
| 4 | 流 | `stream:"session-events"`（**闭集单值**）→ 渲染器订阅宿主既有 SSE，按 sessionId 过滤 EventKind 帧折叠进消息流；契约不写 SSE 路径（宿主基建细节） |
| 5 | 中断 | v1 **不做**（宿主中断面需先实证；诚实缺省比假按钮好） |

## 3. 验收判据（契约层面）
S1 契约字段校验入 core（chat 体：三 RPC 二元组齐 + stream 闭集 + 缺项 view-malformed）；
S2 折叠纯函数可测（EventKind 帧 → 消息增量；历史形状 → 气泡）；
S3 发送线形状 `{sessionId,text}` + 乐观追加；S4 选择器形状钉死（list 行 → options）；
S5 既有卡零影响（chat 是新增 view.kind，闭集扩展经 m32 双模型防线）。

## 4. 边界
不做富文本/markdown 渲染（v1 纯文本气泡）· 不做附件 · 不做多会话并排 · 不做审批交互
（approval SSE 线已有旧前端消费，卡片侧留作 C9 候选）· 本轮只出契约设计，实现 C8 编码轮另起。

# 设计结论：桌布 C8 —— chat 视图契约（设计定稿，实现排期）

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-193**。**本轮只出契约**，编码轮另起。

## 1. 核心裁决：谁拥有会话协议？

**考虑过的选项**：
- **(A) wasm chat 单元代理全部**：send 经单元 `set("sessionPrompt")`。否决——长 RPC
  `session.prompt` 需要 `&mut Boot`（accept 循环内），而单元→`host_services.set`→
  `RemoteHost::set` 只拿 `&self`，注入 turn-driver 回调要做 Arc/Box<dyn Fn> 反向钩子，
  **装配倒挂**（RemoteHost 先于 agent loop 构造）；且会话域本就是宿主概念，
  「单元优先」不该演变成「一切都套一层 wasm 中转」。
- **(B) 声明单元 + 宿主协议面**（选定）：`panel-chat` 单元**只拥有声明**（describeUI
  返回 v2 chat 卡，无自有数据端点）；三个数据面 RPC 全部声明指向**宿主原生臂**
  （会话协议 = 宿主域，slash 别名薄臂）。声明仍是数据、渲染仍在浏览器、Rust 不渲染
  ——三条不变量不动。

## 2. 契约（v2 新增 `view.kind:"chat"`，闭集扩展经 m32 双模型防线）

```jsonc
"view": {
  "kind": "chat",
  "sessionSource": ["session", "list"],   // 既有 sessions.list 之 slash 别名；行 {sessionId,updatedAt,running,blank}
  "historyRpc":    ["session", "history"],// 新宿主薄臂：投影器 sessionHistory
                                          //   args {sessionId} → {ok,value:{messages:[{role:"user"|"assistant",text,ts}]}}
                                          //   （RemoteHost 已有 session_events + User/Assistant 折叠先例，同型扩展）
  "sendRpc":       ["session", "prompt"], // 长 RPC 臂的 slash 别名，args 简化形状 {sessionId,text}
                                          //   （臂内映射到既有 content:[{type:"text",…}] 输入形状）
  "stream": "session-events"              // 闭集单值：渲染器订阅宿主既有 SSE（同源），
                                          //   按所选 sessionId 过滤 EventKind 帧折叠进消息流
}
```

校验（core.validateDeclaration）：三个面必须是 `[ns,method]` 二元字符串组；
`stream` 必须恰为 `"session-events"`；任一缺/错 → `view-malformed`。

## 3. 渲染器行为（chat 档）

1. **会话选择器**：`sessionSource` → `<select>`（行 → options：label=`sessionId·running?"忙":"闲"`）；
   卡内状态记住选中；无源或失败 → 诚实错误态（**不猜会话**）。
2. **历史**：选中会话 → `historyRpc{sessionId}` → 气泡列（纯文本 v1）；错误态显示错误。
3. **发送**：输入框 + 发送 → `sendRpc{sessionId,text}`；**乐观追加** user 气泡；
   错误 → 标红保留输入。
4. **流折叠（core.js 纯函数 `chatFoldFrame(state, frame)`）**：
   - `user/message` → 对齐/吞并乐观气泡；`assistant/message` → 追加/延续 assistant 气泡；
   - `turn/start|turn/end` → 忙态标记；`command/run|done`、`plan/mode` 等 → 系统行（单行说明）；
   - 非所选 sessionId 的帧 → 忽略。
   SSE 连接 = 既有宿主端点（EventSource，同源 cookie 面与旧前端一致）；断线 → 状态行提示 +
   重连（浏览器 EventSource 自带）。

## 4. 实现切片（编码轮排期，各自红→绿）

| 片 | 内容 | 层 |
|---|---|---|
| C8-1 | core：chat 校验 + `chatFoldFrame` + `chatOptions`（node 先红） | 纯函数 |
| C8-2 | 宿主：`session/history` 薄臂 + `session/prompt`、`session/list` slash 别名（RemoteHost sessionHistory 折叠 + 测试） | Rust |
| C8-3 | app.js chat 渲染器（选择器/历史/发送/SSE 接线——DOM 层，纯函数面已钉死） | JS |
| C8-4 | `panel-chat` 声明单元（describeUI + web/ui.json 一份契约 + mNN + scan 自动挂载第八卡） | 单元 |

## 5. 回滚点
纯设计轮：撤本文档 + D-193 即回到 `f8a5d68`；实现片 C8-1..4 各自独立可撤。

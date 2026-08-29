# 验收结论：桌布 C8 —— chat 全链路（契约定稿 + 四切片落地）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-193**（设计 + C8-1..4 切片实测逐段入账）。

## 切片总览

| 片 | 内容 | 验证 |
|---|---|---|
| C8-1 | core：chat 校验（三面 + stream 闭集，形状先于保留档）+ `chatFoldFrame` + `chatOptions` | 桩红 5 → node 26/26 |
| C8-2 | 宿主对齐：**复用既有 session.history 面** + list/prompt slash 别名 + `{sessionId,text}` 简化形状（自造臂遮蔽旧面被旧测红暴露后回正） | rpc_session 9/9（旧 2 测复活） |
| C8-3 | `renderChat`（选择器/历史归一折叠/发送乐观气泡/轮询同事实源）；chat 升入 IMPLEMENTED 四档 | node 26/26（测试随语义迁移） |
| C8-4 | `panel-chat` **声明单元**（describeUI 只拥有声明；无自有数据端点，数据面调用 fail-loud）+ 清单第八卡 | m39 先红 → **3/3**；scan_mounted 第八卡断言 |

## 架构不变量核对（三条全绿）
声明 = 数据（ui.json 与 describeUI 一份契约，m39 守）· 渲染在浏览器（renderChat/
chatFoldFrame 全在 JS）· Rust 不渲染（宿主只出事件/列表/prompt 数据面）。
「单元优先 ≠ 一切套 wasm 中转」的裁决（D-193-B）落地为可运行形态：第八张卡上桌布，
其数据 RPC 直连宿主既表面，与旧前端**同一事实源**。

## 诚实台账
1. SSE `stream:"session-events"` 直订未接（宿主帧形状未取证）；v1 = 5s 轮询走同一
   折叠事实源，接入时仅换驱动不改语义。
2. 浏览器端到端手测未执行（无基建）；折叠/选择/校验纯函数面 node 全钉死，DOM 为接线。
3. 中断/审批交互未做（D-193 边界：宿主面未实证，诚实缺省）。

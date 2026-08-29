# 需求结论：桌布 C5 —— `ui-manifest-changed` SSE 热插拔最小验证

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸（用户授权）| 决策记录 D-186。
上游：canvas design §6.2（变更通知契约）、D-099（`/plugins/events` SSE 通道）、C4（scan 发现挂载）。

## 1. 目标（第一性拆解）

热插拔是第一等要求。现状：运行时**没有任何**包增删通路（scan 只在 serve 启动跑一次），
桌布实时性 = 4s 轮询。C5 要不可再分地做到：
1. **运行时同步**：`wasm_base` 出现/消失合格的 remote 单元 → boot.packages/remote_carriers
   在 ≤N 秒内增删（这是广播的前提，没有它 SSE 只能报「ui.json 内容变了」报不了「装卸」）；
2. **变更推送**：清单 rev 变 → `/plugins/events` 广播 `{type:"ui-manifest-changed", rev}`
   （复用 D-099 通道与 mpsc 广播面，不新建端点）；
3. **桌布消费**：app.js `EventSource` 收帧即重取清单（`pollDecision` keep/replace 语义
   不变）；轮询降级为兜底（间隔放宽）。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 变更检测机制 | **serve 主循环 tick 挂钩 + 2s 节流重扫**（单线程非 Send 纪律下唯一零新线程方案；无 fs watcher 依赖） |
| 2 | 运行时构建 | watch 重扫**不触发** `cargo component build`（会阻塞 accept 循环分钟级）；缺构建物 = 未就绪，静默视之（下次 tick 再见） |
| 3 | 运行时装载体失败 | eprintln 跳过该单元（**不炸 serve**、不上死卡）——与启动 fail-loud 区分：启动是装配决策，运行时是热插事件 |
| 4 | 卸载范围 | 只卸 **watch/启动 scan 挂载过**的包（`state.mounted` 名单），绝不碰 boot manifest 装配的其它 packages |
| 5 | 广播时机 | rev 变才广播（内容哈希自带去抖）；启动基线不广播（无客户端） |
| 6 | 轮询去留 | 保留但放宽 10s（SSE 断线兜底；`unchanged` 协商让兜底几乎免费） |

## 3. 验收判据

| # | 判据 |
|---|---|
| S1 | 帧形状：`data: {"rev":…,"type":"ui-manifest-changed"}\n\n`（D-099 SSE 纪律） |
| S2 | 装：temp wasm_base 放入合格单元 → tick 挂载（packages+carriers 增长）→ 广播新 rev；再 tick 静默 |
| S3 | 改：单元 ui.json 内容变 → tick 后 rev 变并广播 |
| S4 | 卸：删除单元目录 → tick 卸载（packages 收缩）→ 广播；`state.mounted` 正确收缩，不碰其它包 |
| S5 | 节流：窗口内重复 tick 不重扫不广播 |
| S6 | 缺构建物：不挂载、不 panic、不构建（无 Cargo.toml 参与） |
| S7 | 回归：全套 0 新增失败；clippy 0；node 16/16 不变（app.js 属 DOM 边界层） |

## 4. 边界

不新建 SSE 端点 · 不做 fs watcher 依赖 · 不做布局动画 · 不改清单 wire · 不动 harness 前端 ·
「最终前端全部服务单元化」是本目标的远景分段（C5 只做热插拔闭环；面板改写继续 ×N）。

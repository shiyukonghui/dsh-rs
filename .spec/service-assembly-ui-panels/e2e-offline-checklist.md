# 旧前端下线判定：真实浏览器 E2E 验证清单（v1，2026-09-05）

目的：「前端全部由服务单元组成，不再使用 deepseek 前端」的**下线判定**必须由真实浏览器
端到端验证 + 用户拍板（自主轮次不代宣）。本清单 = 判定所需的逐项烟测脚本与已知缺口。

## 0. 准备
1. `cargo run -- serve`（确保 wasm_base=wasm-plugins/ 存在已构建组件；首次启动 scan 会自动构建缺失单元）。
2. 打开 `http://127.0.0.1:<port>/canvas`。验收基线：清单应含 **9 卡**、侧栏五分类齐
   （model/runtime/resource/session/config）、无「渲染器未实现/校验失败」红卡。
3. 热插拔抽查：运行中把 `wasm-plugins/panel-dynamic-plugins` 改名 → ≤2s 卡片消失、分类收缩；
   改回 → 卡片恢复（C5 watch 链路）。

## 1. 逐卡 E2E（对照服务单元 ↔ 旧面板能力）
| 卡 | 验证点 | 对应旧前端能力 |
|---|---|---|
| Provider 设置（llm-deepseek, form） | 字段预填自 kv；保存回读一致 | Provider 配置 |
| 插件清单（list） | loader 行齐；禁用条目不出现 | 插件清单 |
| 运行时状态（status） | 计数与 loader/动态包实况一致 | 概览状态 |
| 动态插件（list+写） | 定义/在跑状态；**停止/卸载弹确认**；取消不发 RPC；成功后行刷新 | dynamicCordisRunner 面板（除 define/activate 表单） |
| 工作区文件（list） | 与默认工作区顶层实况一致；未配置工作区=错误态非空表 | 文件面板（只读子集） |
| 会话清单（list） | sessionId/epoch 与实况一致（只读） | 会话面板（列举子集） |
| 设置概览（list） | ns/字段/值与 settings.describe 一致；secret 仅存在性 | 设置面板（只读子集） |
| **聊天（chat）** | 选会话→历史气泡与旧前端一致；发送→乐观气泡→SSE 真回复；切会话不串线 | **聊天主视图（关键路径）** |
| 设置编辑（form+fieldsFrom） | ui-theme 字段投影自 schema；保存成功；改并发→SETTINGS_CONFLICT 显式 | 设置面板（写端，ui-theme 子集） |

## 2. 已知缺口（判定=不通过项，如实列）
- ~~调度/任务面板未迁移~~ → **读端已落地（D-195 第十卡，只读）**；写端（create/delete
  表单与确认）仍是缺口。
- 动态插件 **define/activate**（表单参数写）未做；审批交互（approval asked/decide）未做。
- 设置编辑 v1 仅 `ui-theme` 一卡（其余 ns = 复制声明单元，机械工作）；secrets 编辑不支持。
- 聊天：中断按钮、审批线、附件未做（D-193 边界）。
- 全部卡片浏览器手测此前**未执行过**（无基建）——本清单即首次执行。

## 3. 判定规则
- 第 1 节全部 ✅ 且第 2 节缺口经用户接受（或补齐）→ 可下线旧前端路由。
- 任一 ❌ → 记录现象回对应面板切片修复（卡级 bug 不阻塞其他面板判定）。
- 下线动作本身 = 后续独立决策轮（保留 `serve` 旧前端路由至判定通过，回滚点=撤那一个提交）。

## 4. 回归基线（E2E 前先绿）
cargo test 全套（dsh-cli 256/0，wasmrt m32–m40）、clippy 0、node 30/30、verify-diff ALL PASS。

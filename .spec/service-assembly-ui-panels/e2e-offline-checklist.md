# 旧前端下线判定：真实浏览器 E2E 验证清单（v1，2026-09-05）

目的：「前端全部由服务单元组成，不再使用 deepseek 前端」的**下线判定**必须由真实浏览器
端到端验证 + 用户拍板（自主轮次不代宣）。本清单 = 判定所需的逐项烟测脚本与已知缺口。

## 0. 准备
1. 起服（D-206 实证配方）：`target\debug\dsh.exe web scenarios\web-smoke.cordis.yml --port <端口>`
   （生产换正式 cordis.yml；`dsh serve` 不存在，子命令 = `web` + 配置必填）。
2. 打开 `http://127.0.0.1:<port>/canvas`。验收基线：清单应含 **13 卡**、侧栏五分类齐
   （model/runtime/resource/session/config）、无「渲染器未实现/校验失败」红卡。
3. 热插拔抽查：运行中把 `wasm-plugins/panel-dynamic-plugins` 改名 → ≤2s 卡片消失、分类收缩；
   改回 → 卡片恢复（C5 watch 链路）。

## 1. 逐卡 E2E（对照服务单元 ↔ 旧面板能力）
| 卡 | 验证点 | 对应旧前端能力 |
|---|---|---|
| Provider 设置（llm-deepseek, form） | 字段预填自 kv；保存回读一致 | Provider 配置 |
| 插件清单（list） | loader 行齐；禁用条目不出现 | 插件清单 |
| 运行时状态（status） | 计数与 loader/动态包实况一致 | 概览状态 |
| 动态插件（list+写） | 定义/在跑状态；**启用**→running（真实装配 loader）；**停止/卸载弹确认**；取消不发 RPC；成功后行刷新 | dynamicCordisRunner 面板（除 define 表单） |
| 工作区文件（list） | 与默认工作区顶层实况一致；未配置工作区=错误态非空表 | 文件面板（只读子集） |
| 会话清单（list） | sessionId/epoch 与实况一致（只读） | 会话面板（列举子集） |
| 设置概览（list） | ns/字段/值与 settings.describe 一致；secret 仅存在性 | 设置面板（只读子集） |
| **聊天（chat）** | 选会话→历史气泡与旧前端一致；发送→乐观气泡→SSE 真回复；切会话不串线；**停止**→turn 中止（aborted 事件）不删历史 | **聊天主视图（关键路径）** |
| 设置编辑（form+fieldsFrom+**nsSelect**） | 下拉列出全部 ns；切换重投影；保存成功；改并发→SETTINGS_CONFLICT 显式；Restart 类 ns 保存后显示「需重启生效」；**秘密字段**=password 框（设→存在性转「已设」；留空保存不改写） | 设置面板（写端**全 ns**，D-201/204） |
| ↳ 自主 E2E 进度（CDP） | ✅ 13 卡渲染+37 行真数据（D-207）；✅ nsSelect 切换重投影、表单诚实错误、侧栏过滤、Console 零错误（D-208）；⬜ 保存成功类写路径（SETTINGS_CONFLICT/重启文案/password 框）待你或下轮交互 | — |
| 设置编辑 · locale（form，固定 ns） | 同契约换 ns=locale（D-201 后与上卡重叠，可合并裁撤） | 设置面板（冗余保留，待拍板） |
| 调度任务（list+删） | after/at/every 行与实况一致；**删除弹确认**；删后行消失（fold 回读） | 调度面板（读+删） |
| 创建调度（form） | kind/prompt/afterSeconds 保存 → 调度任务卡出现该行 | 调度面板（建端） |
| **待审批（list+决定）** | agent loop 触发 ask→行出现（工具/会话/原因）；允许→工具续跑；**拒绝弹确认**→行消失且工具拒执 | **审批弹窗主路径（关键路径）** |

## 2. 已知缺口（判定=不通过项，如实列）
- ~~审批交互~~ → **已落地（D-198/199 第十二卡：pending 列表 + 允许/拒绝(confirm)）**；
  审批策略编辑（approval/policy）未做（属设置域）。
- 动态插件 **define/activate**（表单参数写）未做。
- 设置编辑已 2 ns（ui-theme + **locale**，D-200 机械复制已兑现）；~~其余 ns 待点单~~
  （D-201 nsSelect 通用卡终结）；~~secrets 编辑不支持~~ → **已落地（D-204 write-only）**。
- 动态插件 define：~~缺口~~ → **非目标（D-205 终审）**：旧前端本无此浏览器能力
  （TS 全表面取证：该面只有事件与 invoke，代码定义是宿主/文件语义=动态插件目录）；
  canvas 做 define 需新造代码上传协议（RCE 面），超出改写边界。
- 聊天：审批线、附件未做（D-193 边界）；~~中断按钮~~ → **已落地（D-203 停止按钮）**。
- 全部卡片浏览器手测此前**未执行过**（无基建）——本清单即首次执行。

## 3. 判定规则
- 第 1 节全部 ✅ 且第 2 节缺口经用户接受（或补齐）→ 可下线旧前端路由。
- 任一 ❌ → 记录现象回对应面板切片修复（卡级 bug 不阻塞其他面板判定）。
- 下线动作本身 = 后续独立决策轮（保留 `serve` 旧前端路由至判定通过，回滚点=撤那一个提交）。

## 4. 回归基线（E2E 前先绿）
cargo test 全套（dsh-cli **259/0**，wasmrt m32–m44）、clippy 0、node **31/31**、
verify-diff ALL PASS。（D-195..200 后基线。）

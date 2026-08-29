# 需求结论：桌布 C6 —— 行动作（rowActions）+ 破坏性确认（写能力卡）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-189。
上游：canvas design §4.1（rowActions **形状早已定稿但渲染器未实现**）、D-188（写动作卡在
确认形态前止步的欠账）、宿主 `set` 面实证（remote_host.rs `dynamicStop/dynamicUndefine`
payload `{pluginId}`）。

## 1. 目标（第一性）

「前端全部由服务单元组成」要求面板能**管理**（不止展示）。展示型卡已批量（form/list/status）；
管理型卡的共同缺口 = **行作用域动作 + 破坏性操作的安全确认**。v2 契约的 list 视图
早已声明 `rowActions` 形状（§4.1），但：渲染器未实现、动作参数线形状未定义、破坏性确认
未定义。C6 补齐这三件，使第一张写能力卡（动态插件 stop/undefine）端到端可用。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 | 理由 |
|---|---|---|---|
| 1 | 行动作参数线形状 | body = `{ row: <该行完整对象> }` | 渲染器不发明身份语义、不挑选字段；**单元自己校验**（row.pluginId 非空字符串）——渲染器不是安全边界 |
| 2 | 确认机制 | 动作可选字段 **`confirm: true`** → 渲染器执行前须用户确认（v1 = `window.confirm`）；**无字段 = 直接执行**（向后兼容，save 类不打扰） | 契约不强制确认（语义因面板而异），只提供不静默的机制 |
| 3 | 动作结果 | 卡状态行显示 ✓/✗；成功 → 重放 dataRpc 刷新列表 | 行数据随动作变（running→defined），不刷新=说谎 |
| 4 | 声明校验扩展 | `rowActions` 若存在必须是数组，每项须有 `name` + `rpc` 二元组，否则 `view-malformed` | 画不画得出仍由声明校验回答 |
| 5 | 首个写卡 | panel-dynamic-plugins 升级：`stop`/`undefine` 端点（宿主 `dynamicStop`/`dynamicUndefine` 透传），rowActions 均 `confirm:true` | 破坏性动作从最高危场景起步验证确认机制 |

## 3. 验收判据

| # | 判据 |
|---|---|
| S1 | core：`rowActionBody(row)` == `{row}`（线形状钉死）；`needsConfirm` 只认严格 `true` |
| S2 | validateDeclaration：rowActions 非法形态（非数组/缺 name/rpc 非二元组）→ `view-malformed`；合法通过 |
| S3 | 单元 stop/undefine：row.pluginId 缺失/空/非串 → fail-loud 且**不触达宿主服务**；正常 → 透传 `{ok:true}`；服务失败 → 透传错误码 |
| S4 | 一份契约继续成立（ui.json 含 rowActions == describeUI） |
| S5 | 渲染：合法 rowActions 出「操作」列（DOM 层，诚实声明）；confirm 未确认**不发 RPC** |
| S6 | 回归 0 劣化 + clippy 0 + node 全绿（新增用例计） |

## 4. 边界
不做撤销/重试语义 · 不做操作审计流 · settings 动态 fields 与 chat 渲染器仍是独立契约演进 ·
`dynamicActivate`（define/run 表单）不在本卡（非行动作）。

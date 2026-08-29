# 取证反转：secrets 编辑可做——redact 仅读侧剥离，update 写路无闸（D-194 划界修正）

日期：2026-09-05 | 取证：dsh-settings redact.rs 全文 grep 实证（32 处 redact 全在
describe/view 构造路径；update 校验-合并路径零 secret 闸）。

## 结论
D-194「secrets 仅存在性、不可在桌布编辑」是 **UX 决定**（不给显示不出当前值的字段做
普通控件），不是宿主限制。旧前端即 write-only 密码框模式——桌布可等价实现：
**write-only 秘密字段**（值永不明文回显：describe 源剥 + 保存后 patch 不含空值）。

## 实现锚点（D-204，预计一轮内收口）
1. **core.js `schemaFields`**：SecretSlot 命中的顶层键（path 无点=顶层）→ 不再进
   readonly/缺席，改推 `{key, type:"text", secretWriteOnly:true, value:"", label:key}`；
   node 测先红（write-only 形状 + 非 secret 字段逐位不变）。
2. **防误清闸（关键）**：`collectValues` 或 renderActions 读值处——**secretWriteOnly
   字段值为空串时必须从 patch 剔除**（否则空值覆盖已存秘密=事故）；node 测钉死双向
   （空→不入 patch；有值→入 patch）。
3. **app.js fieldInput**：secretWriteOnly → `input.type="password"` + placeholder
   「写入即覆盖，永不明文回显」；值恒空起步。
4. **声明层零改动**（fieldsFrom 卡自动获益；panel-locale-edit/settings-edit 通用）。
5. 文档：D-204 + E2E §2 划账 + §1 设置编辑行加秘密写烟测点。

## 边界不变
secrets 的 set/未设存在性展示保留（schemaFields.secrets 侧不动）；嵌套内 secret（path
带点）维持 v1 只读（顶层标量纪律 D-194 不变）。

## 纪律注记
本轮只取证不实现（预算闸）；实现轮 TDD 红必跑（第 1/2 点均有纯函数落点，可证层完整）。

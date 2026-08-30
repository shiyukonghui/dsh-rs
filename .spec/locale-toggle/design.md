# 设计 · 中英文切换（含单元声明双语）

## 1. 契约：LocalizedText（panel-ui/v2 兼容扩展）
文案位允许两种形：`"字符串"`（单语，旧声明零破坏）或
`{"zh": "...", "en": "..."}`（双语；一侧缺→回退另一侧→回退任意串）。
适用位：卡 `title`/`description`、list `columns[].label`/`emptyText`、
form `fields[].label`、`actions[].label`/`rowActions[].label`。
解析器 `ltext(v, lang)` 落 shell values.rs（字符串直返；对象按 lang→zh→en→
首个值）。校验面 model.rs/schema.rs：这些位由「必须字符串」放宽为
「字符串或 {zh,en} 对象」。

## 2. shell 消费链
- **i18n.rs（新）**：chrome 字典 ~40 键 ×{zh,en}（header 标题/状态、侧栏「全部」、
  卡生命周期「载入体面…/载入当前值…/载入清单…/载入会话列表…/没有可选会话」、
  表单 act（✓已保存/✗保存失败/✓已发送/✗发送失败/发现 N 项…）、聊天（发消息…/
  发送/停止/取消）、摆位（重置摆位/本组卡片已全部关闭…）、确认弹窗模板
  「确认「{0}」？」双版）。
- **语言信号**：`lang: Signal<String>`；启动 `settings/describe` 读
  `locale.preference`（缺省 zh）→ 置信号。
- **顶栏钮**：header 右侧 `中 / EN`——点击 → 以当前 revision 发
  `settings/update {ns:"locale", patch:{preference: 目标}}`（同 FormSave 线形）
  → ok 后置 lang（即时重渲全壳+已载卡）。失败=act 显错（header 状态位）。
- **渲染穿透**：lang 信号进 card_body/卡头/侧栏/表单/聊天渲染点；
  所有 `scalar_text(title/label/...)` 取文案处改走 `ltext(v, lang)`。
- **locale-edit 卡保存联动**：FormSave settings-update 成功且 ns==locale →
  同步置 lang（本页即时）。

## 3. 单元迁移（13 张卡）
逐 ui.json：title/description/label/emptyText 包 `{zh,en}`；列名/动作名同步。
数据面不动（值多为英文机器态）。llm-deepseek lib.rs `ui_declaration` 同步
（m32 deep-equal 断言）；先 grep 其余单元是否有 ui_declaration 镜像，有则同步。
英文译文口径：对齐原版 harness 用语（Provider/Approval/Schedule/Sessions…）。

## 4. 构建与验证
构建链：canvas-shell（wasm+bindgen+assets）→ 13 单元组件全量
`cargo component build` → bin 重建（内嵌）→ serve 重启。
测试：model.rs 新用例（LocalizedText 过校验/坏形拒绝）；shell lib 测试 ltext。
CDP 验证（verify-locale.mjs）：断言中文（侧栏「全部」+卡标题中文）→ 点 EN →
断言「All」+卡标题英文 + 表单 act 英文 → 刷新断言持久 → locale-edit 卡存 zh
断言即时中文 → 复原 zh；console 零错；e2e-audit T0-T17 不劣于基线。

## 5. 风险与对策
- validate 白名单遗漏某位 → 校验放宽处集中一处工具函数，单元逐个过卡回归。
- m32/等价测试镜像漂移 → 构建前 grep ui_declaration 全量对齐。
- header 钮挤占布局 → 复用 header 既有 flex 尾部（.status 之前）。
- 回滚：i18n 层/钮/ltext 均为增量，revert 单提交链即可。

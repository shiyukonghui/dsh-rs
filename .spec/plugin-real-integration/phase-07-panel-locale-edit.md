# 阶段 7 · panel-locale-edit（设置编辑 · locale）— ✅ 通过

- 功能清单：表单卡；fieldsFrom `settings/describe` pick=locale（无 nsSelect）
  + save → `settings/update`；locale schema=`preference∈{zh,en}`（union const）。
- 静态链路：与阶段 6 同臂（settings.describe/update）；无缺口。
- 浏览器实测（verify-action-form.mjs --row-ns locale，**7/7 全绿**，console 零错）：
  field-found(zh/en select) → set en → save1「✓ 已保存」→ 概览精确行
  `locale preference en`（**写可见**）→ 回存 zh ✓ → 不重载再存
  `✗ … "locale" … (expected revision 3, now 4)（code=SETTINGS_CONFLICT）`
  → 重载概览 `locale preference zh` 复原。
- **疑点 #2 判定**：卡声明的功能面=「编辑并保存 locale 设置文档」——真实成立。
  消费端（i18n 驱动）属 JS 客户插件时代（`@deepseek-ai/dsh-client-locale`，
  D-212 后归档），Dioxus 壳当前无 locale 消费者；「设置值驱动壳界面语言」属
  未来特性（本目标非目标「不引入新能力」），如实记录不扩权。
- 留痕（R2）：locale ns 由未设置({}) → preference=zh 显式落档（语义中性，
  壳无消费者故零副作用；卡面无法「清空」字段，此为声明面边界非缺陷）。
- 过程诚实记录：首轮 `summary-shows-alt` 因 `en⊂preference` 子串误配错行——
  当场识破并给模板加 `--row-ns` 行首列精确锁后复跑取净证据（写入本身首轮即真，
  由 locale ns 冲突 revision 0→1→2 与终行 preference 存在性铁证）。

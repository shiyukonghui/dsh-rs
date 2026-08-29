# 设计结论：设置编辑卡（S 系列切片，D-194）

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-194**（本轮只出契约，编码轮逐片红→绿）。

## 1. 契约增量（canvas design §4.1 form 块的回写点）

```jsonc
"view": { "kind": "form",
  "fieldsFrom": { "rpc": ["settings", "describe"], "pick": "ui-theme" },  // 与 fields 二选一
  "saveRpc": ["settings", "update"],      // body：{ns, patch:{…}, expectedRevision}
  "actions": [ … ]                        // 既有卡级动作面不变
}
```
校验（core.validateDeclaration 扩展）：`form` 体须满足 `fields 数组` **或** `fieldsFrom 形`
（rpc 二元组 + pick 字符串）；两者皆缺或皆有 → `view-malformed`。

## 2. 分层落点

```
core.js（纯函数，node 钉死）
  export function schemaFields(nsView)
    → { fields:[{key,label,type:"text"|"number"|"checkbox"|"select",options?,value,current,locked?}],
        readonly:[{key,note}],           // 嵌套对象/数组：只读展示行
        secrets:[{path,set}], revision, applies }
    规则：schema.properties 顶层逐键；type 映射标量；enum→select；
    非标量→readonly（note 标注形态，不伪造输入控件）；
    value 取 nsView.value 同名键（缺 → 空串/0/false 诚实初值，不虚构默认）。
app.js renderForm（**扩展而非另造**——一个表单实现，杜绝双源）
  view.fieldsFrom 存在 → 先 rpc(rpc.join("/"),{}) 取 namespaces[] 找 pick（缺 → 诚实错误态）
  → schemaFields 投影出与静态同形的 fields → 既有 collectValues/保存路径复用；
  保存 body 带 ns + expectedRevision；SETTINGS_CONFLICT → 状态行显式 + ↻ 重读；
  applies==="restart" → 成功文案追加「需重启生效」。
宿主（薄，D-192 同型别名）
  "settings.describe" | "settings/describe" 同臂；"settings.update" | "settings/update" 同臂
  （两测：别名同臂响应一致）。
单元 panel-settings-edit（照 panel-chat 声明单元型，零自有数据端点）
  describeUI = v2 form 卡（fieldsFrom: describe/pick ui-theme，saveRpc settings·update），
  ui.json == describeUI（mNN 一份契约）。scan 挂载 → 清单第九卡（type config）。
```

## 3. 实现切片（各自红→绿、独立可撤）

| 片 | 内容 | 层 |
|---|---|---|
| S1 | core：schemaFields + form 校验扩展（node 先红） | 纯函数 |
| S2 | 宿主 describe/update slash 别名（同臂测试） | Rust |
| S3 | renderForm fieldsFrom 预载 + 冲突/重启文案（DOM 接线） | JS |
| S4 | panel-settings-edit 声明单元 + 清单第九卡 | 单元 |

## 4. 回滚点
纯设计轮：撤本两文档 + D-194 即回到 `664051c`；实现片 S1..4 各自独立可撤。

# 需求结论：设置编辑卡（panel-settings-edit）——form 动态 fields 契约演进（S 系列）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 **D-194**（契约定稿；实现切片 S1..4 排期）。
数据面实证（D-192 已钉）：`settings.describe → namespaces[] {ns, schema, value, base?, user?, applies, secrets[{path,set}], revision}`（describe_all 源头 redact）；写面 `settings.update {ns, patch, expectedRevision}`（冲突 → SETTINGS_CONFLICT；未注册/非法 → settings-rejected）。既有 form 档 = **静态** view.fields（llm-deepseek 试点）。

## 1. 目标（第一性）

设置面板的写半边。本质障碍：字段集合与校验是**运行时数据**（schema 由宿主注册表给出），
v2 form 档的 fields 是**声明期数据**。基本事实核对：
- 写 RPC 与冲突协议宿主**全部现成**（settings.update + expectedRevision 乐观锁）——无需新后端；
- 缺的只是渲染侧「从 schema+value 投影出表单」这一步（可证纯函数）；
- 设置域是宿主域（同会话域先例 D-193-B）→ 单元只拥有声明。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 契约扩展形态 | form 视图新增可选字段 **`fieldsFrom`**：`{rpc:[ns,method], pick:"<ns>"}`——fields 于渲染时从数据面投影；与静态 fields **二选一**（校验扩展） |
| 2 | 数据面 | 宿主**既表面 + slash 别名**（`settings/describe`、`settings/update`）——复用不另造（D-192/D-193 同纪律） |
| 3 | 声明归属 | `panel-settings-edit` 声明单元只拥有声明（D-193-B 复制）；零自有数据端点 |
| 4 | v1 字段面 | 仅顶层**标量**属性（string/number/boolean/enum）；嵌套对象/数组 → 只读展示行（**不伪造可编辑**） |
| 5 | secrets | 仅显示「已设/未设」存在性；**编辑不支持**（redact 源头 set-only，写密钥需专门安全形态）——诚实缺省 |
| 6 | applies=restart | 保存成功后状态行显式「需重启生效」（不假装即时） |
| 7 | 并发 | describe 带 revision → 保存携带 expectedRevision；SETTINGS_CONFLICT 显式呈现 + 引导重读 |

## 3. 验收判据（契约层面）
S1 core：`schemaFields(nsView)` 纯函数（标量→输入、enum→select、嵌套→只读、secrets→存在性）+ form 校验扩展（fields XOR fieldsFrom 形）——node 先红；
S2 宿主：describe/update slash 别名与点号形同臂；
S3 renderForm 扩展：fieldsFrom 预载 → 投影 → 编辑 → update{patch,expectedRevision} → 冲突/重启文案；
S4 声明单元 + 清单第九卡（type config）；回归 0 劣化。

## 4. 边界（非目标）
namespace 增删 · 嵌套结构编辑 · 密钥写入 · 撤销/草稿持久化 · 「恢复默认」按钮（base 值展示可留后续）。

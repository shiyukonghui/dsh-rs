# 设计：服务装配单元 Phase 9 — B4 config simplify 回写 unparse

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 9）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p9/requirements.md`（定稿）+ schemastery 源码实证。

---

## 1. 设计目标

dsh-schema 提供 `simplify(schema, value)`（schemastery `Schema.prototype.simplify` 语义移植）；
loader `write_back` 在入口插件声明 `config_schema` 时把 `e.options.config` 存为简化值（cordis
`Config['simplify'](config)` 同径）——内存=落盘形态一致。无 schema → 原样（零影响）。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| schemastery simplify | @deepseek-ai/schemastery src/index.ts:407-442 | 逐分支语义 |
| dsh-schema | `Meta.default`（lib.rs:26-43）/ `resolve`（415）/ `ResolveOptions{path,autofix,strict}`（375） | 基础设施（union try 用 resolve） |
| loader write_back | loader.rs:263-285（`e.options.config = cfg.clone()`） | 注入点（fiber_to_entry → options.name → st.plugins[name].plugin.config_schema） |
| persist | loader.rs:844-852（write → entry_options → sink） | 落盘读 e.options.config（简化生效） |
| m17_persist | 机制锁定（no-schema 插件） | 零回归基线 |

## 3. 设计分解

### S1（dsh-schema simplify，schemastery 逐字）

```text
pub fn simplify(schema: &SchemaRef, value: &Value) -> Value {
    // 1. 与 meta.default 深等 → Null（无默认 → false）
    // 2. isNullable（JSON null）→ 原值
    // 3. 按 kind 分派：
    //    Object(fields): 每键用对应子 schema simplify；item Null → **删键**；
    //                    结果与 default 深等 → Null
    //    Dict{inner,..}: 每键 simplify(inner, v)（**保留 Null 项**）
    //    Array(inner)/Tuple(items): 逐项 simplify（index 对齐）
    //    Intersect(list): 逐成员 simplify 后 Object.assign 合并
    //    Union(list): 逐个 try resolve(value, s, {path:[],autofix:false,strict:false}) 成功 → simplify(s, value)
    //    _: 原值
}
```

- `deepEqual` 用 serde_json `==`（DIV-9-1：dict 的 default 特判在 JSON 值域退化为常规深等）。

### S2（loader write_back 接入）

```text
// loader.rs write_back 内，替换 `e.options.config = cfg.clone();`：
let mut cfg = args.get(1).cloned();
let schema = st.entries.get(&entry_id)
    .and_then(|e| st.plugins.get(&e.options.name))
    .and_then(|r| r.plugin.config_schema());          // Arc<dyn Plugin>::config_schema()
if let Some(sc) = schema { if let Some(c) = cfg { cfg = Some(dsh_schema::simplify(&sc, &c)) } }
if let Some(c) = cfg { e.options.config = c; }
```

- 无 schema → cfg 原样（T2 行为不变）；`st.plugins` 借用与 `entries` 嵌套共享借用需在其后释放再改
  `e.options.config`（避借用冲突）。

### S3（m-series 红测，crates/dsh-loader/tests/m24_config_simplify.rs）

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | FnPlugin 带 `config_schema`（默认 `def=5`），config `{def:5, other:1}` → create/update | persist sink 收到 `{other:1}`；**不加 simplify 则收到 `{def:5,other:1}`（红）** |
| T2 | 无 schema 插件 config `{k:1}` | 写回 `{k:1}` 原样 |
| T3 | 嵌套 object 默认键删 + dict 保 null + array 逐项 | `simplify` 单测/写回断言 |

- 测试经 `set_persist` sink 捕获 EntryOptions.config（复用 m17 模式），或对 write_back 的
  `st.writes`…——用 sink 断言（权威列表含化简后 config）。

## 4. 实现顺序（TDD）

1. **S1**：`simplify` + 单测（dsh-schema）。
2. **S2**：write_back 接入。
3. **S3**：m24 T1-T3 红→绿。
4. **回归**：m17_persist + loader/diff（23 golden）+ workspace + clippy；**阶段 5**：serve 冒烟 +
   acceptance。

## 5. DIV / 让步清单

- **DIV-9-1**：`deepEqual` 的 dict-default 特判降级（serde_json 深等；JSON 无 undefined——dict
  默认值的 per-key 比较与 schemastery 差异仅影响极边缘 dict+default 场景）。
- **DIV-9-2**：简化在写回（internal/update）而非序列化前——内存=落盘一致（cordis 同径），避免
  下次 write 从未简化的内存重新导出。
- **DIV-9-3**：next-load 默认补回（validate_config）——简化不丢失语义。

## 6. 部署与回滚（阶段 5 预案）

- 部署：纯增量（dsh-schema 新函数 + write_back 分支）；无 schema 插件零行为变化。
- 回滚：`git revert` 本阶段提交（dsh-schema simplify + write_back + m24）。

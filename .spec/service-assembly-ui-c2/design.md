# 设计结论：桌布 C2 —— `uiManifest/list` 实时清单端点

日期：2026-09-04
阶段：系统设计（瀑布流阶段 2）——基于 `.spec/service-assembly-ui-c2/requirements.md`（已过闸：用户确认
范围=仅 C2、端点=(a)、disabled=同名 entry 全禁用才排除）。
决策记录：`docs/DECISIONS.md` **D-183**。
上游契约：`.spec/service-assembly-ui-canvas/design.md` §6（D-181 锁定）；本文把 §6 具体化到
函数签名/装配点/rev 语义/坏包语义/测试清单，并按已定端点形状**回写** §6.1 与 §1 架构图。

---

## 1. 架构决策

### 1.1 模块切分：纯函数核心 + 一臂接线（单一权威、可复用）

```
crates/dsh-cli/src/ui_manifest.rs   （新模块，核心逻辑，零 HTTP 依赖）
  build_manifest(packages, entries) -> UiManifest      ← 每请求实时计算，禁缓存
crates/dsh-cli/src/web.rs
  dispatch() 新增臂： "uiManifest/list" => ui_manifest_result(boot, payload)
```

理由（需求 §7.4）：聚合/校验/归一/哈希全部收进 `ui_manifest.rs`，HTTP 层只取数（
`boot.packages` + `boot.loader.entries()`）与序列化。**C3（桌布壳代理）与 C5（SSE 广播 rev）
复用同一核心**，规则不复制。`dispatch()` 里只加一臂——与 `commands/list` 同型，
`/api` trust fence（仅 loopback）与 RPC 信封（`type:"client-request"` + method 一致）
经 `handle_rpc_host` 免费复用。**不新增路由例外。**

### 1.2 端点形状（Q2 已定 = 选项 a）

- **请求**：`POST /api/uiManifest/list`，body = client-request 信封；参数在 `payload.args`
  （与 `dispatch_wasm_remote` 相同的解包纪律：`payload.args` 缺失 → 用 payload 本身）：
  `args.rev?: string`（客户端已持有的清单哈希）。
- **响应（成功）**：
  ```jsonc
  { "ok": true, "value": {
      "rev": "<64-hex sha256>",
      "cards": [
        { "pluginName": "llm-deepseek", "cardId": "llm-deepseek.settings",
          "type": "model", "title": "DeepSeek Provider",
          "size": { "w": 2, "h": 3 },
          "declPath": "/plugins/llm-deepseek/ui.json" },
        // 归一改动时附加诊断字段（未改动则缺席）：
        // "declaredType": "llm",  "declaredSize": { "w": 9, "h": 9 }
        // 坏声明条目（不静默丢）：
        { "pluginName": "broken", "declPath": "/plugins/broken/ui.json",
          "error": { "code": "schema-version-unsupported", "message": "..." } }
      ] } }
  ```
- **响应（协商短路）**：`args.rev` == 当前计算 rev → `{ "ok": true, "value": { "rev": …,
  "unchanged": true } }`（**无 cards 字段**，省带宽；C5 SSE 落地前的轮询优化）。
- 不加入 `is_long_rpc_method`（纯内存+小包读，accept 同步处理）。

### 1.3 `rev` 语义：内容哈希（非单调计数）

- `rev = SHA-256(canonical JSON of cards[])` 的**小写 64-hex 全量**。
- **为什么内容哈希**（需求 S3）：单调计数在进程重启后归零/漂移，客户端缓存的 rev 全部作废；
  内容哈希「同内容同 rev」跨重启成立——热更语义的前提。
- **error 条目计入 rev**：坏声明被修好 = 清单内容变化，rev 必须变。
- 依赖：`sha2 = "0.10"`——已在 Cargo.lock（`sha2 0.10.9`）与本地 registry 缓存，
  **零新增供应链**、离线可解析（方法论四：成熟依赖优先；hex 编码手写，不拉 hex crate）。
- cards 的 key 序由 serde_json Value 序列化决定（同一构建代码路径 → 确定性）；
  禁含绝对路径/时间戳等**不稳定源**进哈希（declPath 是 URL 不是文件系统路径 ✓）。

### 1.4 校验与归一（清单层 = 单一权威，渲染器只信清单）

对每个 `pkg in boot.packages` 按序处理（**cards 数组 = packages 声明序**，无 priority）：

| 步 | 规则 | 结果 |
|---|---|---|
| 1 | `pkg.web` 为 None，或 `web/ui.json` 文件不存在 | **跳过**（无 UI ≠ 坏 UI，安静） |
| 2 | 文件读失败 / 非 UTF-8 / 非 JSON / 非对象 | error 条目 `declaration-unparseable` |
| 3 | `$schema` ≠ `"dsh/plugin-ui/v2"`（缺失同罪） | error 条目 `schema-version-unsupported` |
| 4 | 顶层 `kind` ≠ `"card"` | error 条目 `card-kind-unknown` |
| 5 | `cardId` 缺失/非字符串/空 | error 条目 `card-id-missing`（卡身份不完整） |
| 6 | `type` ∈ 闭集 {model,config,capability,runtime,resource,session,misc} → 原样；否则 → `"misc"` + **`declaredType` 保留原值**（非字符串原值丢弃不记） | 归一 |
| 7 | `size`：对象且 w/h 为数字 → `w=clamp(1..4)`、`h=clamp(1..8)`，改动则记 **`declaredSize`**；`size` 缺失/非法 → 按 `view.kind` 默认：**status→2×2、list→4×4、其余（含 form/异常）→2×3** | 裁剪/默认（降级不是失败，不报错） |
| 8 | `title` 非空字符串 → 原样；否则回落 `cardId` | 诚实回落，不新增诊断字段 |
| 9 | `size.x/y` 等坐标键 | **不进清单**（六元组 + 诊断字段之外零输出，坐标永不外泄） |
| 10 | `view` 体 | **不校验、不下发**（view-malformed 属渲染器档，C3；清单只元数据） |

> `card-id-missing` 是清单层新增的第四个错误码（承 D-182 `card-kind-unknown` 的收敛方式：
> m32 已断言 cardId 非空 = 身份不完整，无法去重/聚焦，fail-loud 而非静默补造）。
> §5.1「默认尺寸按 type」的原文把 type 与 view.kind 混写，此处按语义裁定为**按 view.kind**
> （model/config→2×3 即 form 档默认）——契约收敛，记 D-183。

### 1.5 disabled 交叉（Q3 已定）

`entries = boot.loader.entries()` 过滤 `group=true` 后：
- 存在同名（`entry.name == pkg.name`）entry 且**全部** `disabled` → **排除该卡**；
- 同名 entry 中任一 enabled → 出卡；
- 无同名 entry → 出卡（试点未 entry 化的现状语义）。

### 1.6 函数签名

```rust
// ui_manifest.rs
pub struct UiManifest { pub rev: String, pub cards: Vec<serde_json::Value> }
/// 实时计算（每请求调用；无缓存）。packages 序 = 卡声明序。
/// entries 来自 loader.entries()（调用方已过滤 group）。
pub fn build_manifest(
    packages: &[crate::plugin_pkg::PluginPackage],
    entries: &[dsh_loader::EntrySnapshot],
) -> UiManifest;
pub fn ui_manifest_result(boot: &Boot, payload: &serde_json::Value) -> serde_json::Value;
// web.rs dispatch():  "uiManifest/list" => ui_manifest_result(boot, payload),
```

`ui_manifest_result` 取数：`boot.packages` + `boot.loader.as_ref().map(|l| l.entries())`
（None → 空 entries = 全生效，诚实：无 loader ≠ 全禁用）。

---

## 2. 测试清单（TDD 红→绿；每条先见红=因缺行为而失败，非编译错）

**`ui_manifest.rs` 单元（核心规则，构造 EntrySnapshot/临时包目录）**
1. `aggregates_two_good_packages_in_declaration_order` — 双包六元组正确 + 无坐标键
2. `skips_packages_without_ui_json` — 无 web 目录 / web 无 ui.json → 零条目（非 error）
3. `broken_declarations_become_error_entries` — 非 JSON / v1 $schema / kind:"form" / 缺 cardId
   → 四个 error 条目各带 code；同批好包照常出卡（坏不连坐）
4. `unknown_type_falls_to_misc_keeping_declared_type` + 缺失 type → misc 无 declaredType
5. `oversized_size_clamped_and_recorded` — 9×9→4×8 + declaredSize；w:0→1；x/y 不泄漏
6. `size_defaults_by_view_kind` — status→2×2、list→4×4、form/缺失→2×3
7. `rev_is_content_hash_stable_and_changes` — 同盘两算同 rev；加卡/改 title/删卡 → rev 变
8. `disabled_entry_excludes_card` — 全禁用同名 entry 排除；任一 enabled 出卡；无 entry 出卡；
   group entry 不参与匹配

**`web.rs` 集成（wire 形状 + 实时性，`boot_with_sessions` + `handle_rpc` + 临时包）**
9. `rpc_ui_manifest_list_shape` — ok + rev(64-hex) + cards 六元组；空 packages → ok + 空 cards
10. `rpc_ui_manifest_is_live_no_cache` — 同一 boot 两请求之间改 ui.json 文件 → 条目与 rev 变
11. `rpc_ui_manifest_unchanged_short_circuit` — args.rev=当前 → `{rev, unchanged:true}` 无 cards

**回归**：m32 8/8 不变；dsh-cli **230 全绿基线**（本机实测，见 requirements A6）；
clippy `-D warnings` 0；verify-diff 26/26。

---

## 3. 边界重申（不做）

不下发 `view` 内容 · 不校验 view 体 · 不做 `ui-manifest-changed` SSE（C5）· 不做桌布壳（C3）·
不试点 entry 化 · 不改 wasm 侧一行 · 不动 `dispatch_wasm_remote` 路由。

## 4. 回滚点

新增 = `ui_manifest.rs` + dispatch 一臂 + `Cargo.toml` sha2 一行 + 测试。撤销提交即回到 `44f9618`
后状态；既有 wire 面零改动。

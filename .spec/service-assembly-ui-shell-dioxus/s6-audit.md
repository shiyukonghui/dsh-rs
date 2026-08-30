# S6 交互级双壳对打审计（第 51 轮，2026-09-05）

脚本：`.spec/service-assembly-ui-panels/e2e-audit.mjs`（同序列打两壳）。对象：
`/canvas`（JS 壳）vs `/canvas/rust`（Rust 壳，内嵌 release wasm）。

## 结果矩阵

| 测试 | JS 壳 /canvas | Rust 壳 /canvas/rust | 判定 |
|---|---|---|---|
| T0 基线 | cards=13 | cards=13 | ✅ 一致 |
| T2 ✕ 关闭 | 12 卡+1 灰显+ls=1 | 12 卡+1 灰显+ls=1 | ✅ 一致（含 localStorage 同键互通） |
| T3 侧栏重开 | 13 卡+0 灰显 | 13 卡+0 灰显 | ✅ 一致 |
| T4 nsSelect 切 llm→字段现 provider 域 | ✅ true/true | ❌ **false/false**（合成 change 事件未触发 NsPick 重投影） | ❌ Rust 缺陷 A |
| T5 表单动作→宿主诚实错误 | NO-INPUT（审计选择器 `input[name=prompt]` 在 JS 壳不成立——JS 读值机制不同；旧轮 e2e-interact 已证 JS 该流程绿） | ❌ honest=false——**页面已死于 panic** | ❌ Rust 缺陷 B |
| T6 chat 乐观气泡 | bubbles=1 ✅ | **页面死亡**（"Inspected target navigated or closed"） | ❌ 随 B |
| T7 热插拔 rename→DOM 即时降 | ❌ **旧 bug 复现**：m1=12 但 dom1=13、dom2=13 | ✅ **dom1=12（SSE→重渲染工作！）**；⚠️ dom2=12（restore 后 10s 内未回 13——remount 帧迟到或漏拍，需复测） | Rust 反超 JS 已知缺陷 |
| Console | 零错误 | **`RuntimeError: unreachable`（Rust panic→abort）×5** | ❌ **Rust 缺陷 C（最重）** |

## 扩展关卡（第 56 轮追加：T9 reload 持久化 + T10 几何零重叠）

审计基建同轮治理：全局超时 150→240s、preflight 自愈（共享 profile 污染→全量重开灰显卡）、
T7 基线动态化、超时也输出 PARTIAL（挂点定位能力）。

| 项 | JS 壳 /canvas | Rust 壳 /canvas/rust | 判定 |
|---|---|---|---|
| T10 几何（13 卡两两矩形） | ✅ overlaps=0 | ✅ overlaps=0 | 双壳实测布局硬不变式成立 |
| T9 关闭→reload→仍闭→重开复原 | ⚠️ **harness 挂点**：reload 后 CDP eval 在旧壳永久挂起（PARTIAL 锁定于 T9；等价能力 D-209 当时已活体认证，旧壳退役在即不追加投资） | ✅ {closedCnt:1, afterCards:12, shutCnt:1, backCards:13} 完整闭环 | Rust 壳切默认前最后一道持久化关卡过 |
| 全套复跑（Rust） | — | T2~T10 + T7 闭环 13→12→13 全绿，consoleErrs=[] | 持续保持 |

## 结论与决定

### 缺陷 C 定位补记（debug 复现成功，第 51 轮追加）
debug wasm 复现拿到 panic 原文（两处 dioxus-core 0.7.10 内部）：
- `dioxus-core-0.7.10/src/runtime.rs:223:51: called Option::unwrap() on a None value`
- `dioxus-core-0.7.10/src/runtime.rs:280:26: RefCell ... already mutably borrowed`

时序：T4（NsPick 合成 change→写 body→spawn 历史加载）后触发；此后 wasm 死亡但 DOM
只读仍活（T5/T7 读到的是残骸）。区分实验（下轮）：
- T2/T3（`el.click()` 同步写信号）**不炸**；T4（select change→写+spawn）**炸**
  → 嫌疑聚焦「**事件/异步回调里的 signal 写触发 runtime 重入或脱域调度**」。
- 修复候选（按侵入度升序）：
  ① 审计改用 CDP `Input.dispatchMouseEvent`（可信事件）——先排除合成事件因素；
  ② 外部回调（setInterval/SSE/spawn-after-await）统一经「运行时队列」再写信号
     （dioxus 官方外部事件模式），壳内所有 `body.write()` 收口到一个 scope 内的泵；
  ③ 升级 dioxus 0.7.10→最新 patch（223/280 可能已修，查 changelog）。

### 缺陷 C 修复回执（第 52 轮，S6a 完成）
**根因**：JS 回调上下文（setInterval/SSE onmessage）无 dioxus 作用域，其内调用
`dioxus::spawn` → `current_owner()` → `scope_stack.last().unwrap()` → runtime.rs:223
panic（abort 级）。四处收口 `wasm_bindgen_futures::spawn_local`（chat bootstrap、
聊天历史轮询、清单轮询、SSE manifest）；事件处理器内的 spawn 保留（delegated
handler 自带作用域）。
**复验（真路由 /canvas/rust，release 内嵌）**：T2/T3/T4/T5/T6/T7 **全过**
（含 dom1=12→dom2=13 热插拔完整闭环），**consoleErrs=[]** 零 panic。
缺陷 A 同灭（wasm 死因即 C；合成 change 事件 dioxus 本可正常处理）。
**判定更新**：Rust 壳 = 全项与 JS 壳一致且 T7 反超（JS 壳三次复现 DOM 冻结）。
S6d（切默认）技术阻塞解除——剩产品决策：旧 /canvas 退役节奏（按设计文档留用户拍板）。

### 决定
1. ~~切默认冻结~~ → **S6a 已修复（第 52 轮）**，S6b/S6c 随复验自动完成。
   剩余 S6d=切默认/退役节奏=用户拍板项（新旧并存即回滚态，随时可切）。
2. **热插拔定案**：JS 壳 DOM 不更新（连续第 3 次复现，坐实）；Rust 壳 SSE→model→重渲染
   链路**通**（dom1=12）。旧壳修复优先级进一步降低（倾向由 Rust 壳直接取代）。
3. **panic 复现路径（下轮起点）**：审计序列 T4→T5 之间触发（T4 返回后、T5 期间死亡）。
   嫌疑排序：① NsPick synthetic change 处理链 ② FormSave onclick→collect→spawn 的
   signal 借用 ③ 1500ms 测量脉冲与并写交错。**复现法**：起 `node .spec/service-assembly-ui-shell-dioxus/dev-proxy.mjs`
   （debug wasm 带 panic 文案与行号）→ `node e2e-audit.mjs http://127.0.0.1:60700/rust.html`
   → console 里的 "panicked at src\app.rs:LINE" 直接定位。
4. 缺陷 A（合成 change 不触发 dioxus onchange）独立于 panic 存在——修复时一并查
   dioxus 事件委托对 `new Event('change',{bubbles:true})` 的信任位问题
   （若真不接，审计脚本改用真实输入路径而非 dispatch）。

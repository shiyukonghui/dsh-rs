# 用 Rust + WebAssembly 达成「一切皆插件」——规划调研方案

> 目标：在 Rust 侧移植/重设计 DeepSeek Harness 所基于的 [Cordis](deepseek-harness/docs/cordis-primer.md)「一切皆插件」运行时，使得**模型适配器、工具注册表、会话日志、甚至是 agent 主循环本身都以插件形式存在、可从配置替换**。
>
> 本方案从第一性原理出发：先抽取 Cordis 真正依赖的最小原语集合，再论证 Rust+WASM 下每种原语的等价物与取舍，最后给出分层架构、接口定义、工具链与落地路线。

---

## 1. 「一切皆插件」到底依赖什么？（第一性原理）

先看 Cordis 的真实机制（取自 `vendor/cordis/src/` 与 DSH 上层封装），剥掉 TypeScript 语法糖后，它只依赖以下**最小原语集合**。任何 Rust+WASM 移植都必须具备这些，缺一不可：

### 1.1 插件 = 纯「应用」函数，不是庞大的类体系

Cordis 里插件三种形状最终都归一为**一个处理函数**：

```ts
type Plugin = (ctx: Context, config: T) => void | Disposable
```

- 普通函数插件：`function (ctx, config) { ... }`
- 类插件：构造后可能调用 `init` 钩子
- 对象插件：`{ apply(ctx, config) }`

**关键推论**：插件本身是「过程式副作用安装器」，几乎所有状态都放在共享的 `ctx`（Context / Service store）里，而非插件私有字段。这让 80% 的插件可以写成 **无状态入口函数**——对 WASM 极其友好。

### 1.2 上下文（Context）= 按名字解析的服务注册表

Cordis 的 `ctx` 是一个 Proxy，属性读取走 `ReflectService` 的解析器：

- `ctx.tools`、`ctx.llm`、`ctx.sessions` 都不是字段，而是**按字符串键查表**
- `provide(name, value)` 注册一个实现；`get(name)` 读取
- 依赖声明 `inject = ['systemPrompt', 'tools']` 表达「我需要的服务」，加载顺序由**依赖可达性**决定，而非手动排 boot 顺序

**关键原语**：一个**全局/分层的服务注册表**（key → 值 + 生命周期 owner），加上**依赖图驱动激活**。

### 1.3 Fiber = 插件的生命周期 + 可逆副作用（effects）

`Fiber` 是单次插件加载的实例，状态机为：

```
PENDING → LOADING → ACTIVE ⇄ (dispose → DISPOSED)
              ↘ FAILED
      UNLOADING
```

核心是 **`ctx.effect(fn)`**：`fn` 立即执行，返回一个 disposer；disposer 在插件卸载时**按逆序**运行。所有注册（`on`/`provide`/`register`）都通过 `effect()` 变成「可回滚的副作用」。

**关键原语**：**arbitrary 副作用 → 单一可逆 disposer 的归一化**。这是 HMR / reload / 插件卸载一致性的根基。

### 1.4 服务变更驱动的重载（通知机制）

`ReflectService.notify(names)`：当服务被注册/卸载后，遍历**所有 runtime 的所有 fiber**，凡 `inject` 含该服务的 fiber 重新 `_refresh()`——依赖消失则卸载，依赖齐备则加载。这就是「加载顺序通过服务需求表达」的实现机制。

**关键原语**：**服务注册表变更 → 对依赖它的插件批量重新求值（卸载/重载）**。

### 1.5 事件分发（四种模式）

`EventsService` 提供 `on`/`once` + 四种 dispatch：

| 模式 | 语义 | Rust 中可映射为 |
|---|---|---|
| `emit` | 即发，顺序观察，忽略返回值 | 同步 `Vec<Listener>` 顺序调用 |
| `parallel` | 全部并发，等全部 settle | `join_all`（async） |
| `serial` | 按序 await，直到某个返回非空即停 | 顺序 async + early-return |
| `waterfall` | 洋葱中间件：`next()` 委托 | 中间件栈（服务端中间件惯用） |

每个监听器既是副作用（用 `effect()` 注册、随 fiber 卸载）。

**关键原语**：**一组 observable + 四种调度策略**，统一承载「拦截 / 策略 / 包装」。

### 1.6 隔离与作用域（isolate / intercept / extend）

- `extend`：创建子 Context，原型继承父属性
- `isolate(name, label)`：把某个服务隔离到独立作用域（同一 label 共享）
- `intercept(name, config)`：为某服务注入每插件的 config 合并

**关键原语**：**上下文链（child scope）+ 服务可见性遮蔽**。

### 1.7 配置校验（标准 schema）

`resolveConfig` 用 `@standard-schema/spec` 校验插件 config，校验失败则 fiber 进 `FAILED` 并大声报错。元素据 `Config`、`inject`、`provide` 是插件「声明」部分。

**关键原语**：**声明性元数据（name / config schema / inject / provide）+ 强 config 校验**。

### 1.8 类型层：declaration merging（编译期）

DSH 大量用 `declare module '@deepseek-ai/cordis' { interface Context { tools: ... } }` 做**编译期类型合并**：新增一个服务 = 往 `ctx` 类型上加一个键。这是 TS 提供的「开放式上下文类型」。

**关键原语（仅编译期，不进入运行时）**：需要对应的**编译期类型注册机制**。

---

## 2. 为什么 Rust + WASM 是「正确」但「有摩擦」的选择

### 2.1 优势
- **真正的沙箱隔离 + 可外包执行**：插件是 `.wasm` 组件，宿主可用 wasmtime 加载，天然内存隔离、能力受限（WASI preview2 / 组件模型），比 DSH 现有的 Landlock/ACL 子进程更像「软件层面的沙箱」。
- **性能与确定性**：Rust 插件编译后热路径快，内存布局确定。
- **「一切皆插件」的终极形态**：连模型适配器、工具执行的 IO 能力都能以 WASI capabilities 形式授予/撤销，做到「界面驱动授权」。
- **跨平台交付**：同一 `.wasm` 组件可在桌面/服务器/浏览器（wasmtime-js / wasmer-js）复用。

### 2.2 六个核心阻抗（必须逐一有对策）
1. **动态类型 → 静态类型**：`ctx.<anything>` 与任意 config 对象在 Rust 里没有运行时的「开放对象」。→ 用 JSON Value 作为边界 + 类型化插口（trait）。
2. **Proxy/原型链 → Rust 继承**：`extend`/`isolate` 需要手写「上下文分层」数据结构，不能用语言继承。
3. **JS 单线程事件环 + 动态作用域 → Rust 并发模型**：`fiber`、`ctx` 是协程/作用域敏感的对象；Rust 需要 `&mut Context` 或 interior mutability，异步化要与 tokio 结合。
4. **Symbol 键 → 无 Symbol**：用字符串/整数键 + 命名空间。
5. **反射/Proxy 透传 → 显式 trait object**：服务解析要基于 `TypeId`/`dyn Trait`/枚举，而不是属性访问。
6. **reload / HMR 的「任意时刻替换实现」**：需要可替换 trait object 与正确的 drop 时序。

---

## 3. 推荐架构：三段式

```
┌──────────────────────────────────────────────────────────────┐
│  ① 核心（Rust crate：dsh-core）                                │
│  Context / ServiceRegistry / Fiber / Events / Config / Scope   │
│  —— 宿主侧纯 Rust，非 WASM，进程内运行                           │
├──────────────────────────────────────────────────────────────┤
│  ② 插件边界（WIT 接口：dsh-plugin.wit）                        │
│  world dsh-plugin: export meta/start/stop; import host/...     │
│  —— 编译期契约，wasmtime 组件模型                              │
├──────────────────────────────────────────────────────────────┤
│  ③ 插件宿主执行（dsh-wasmrt）                                  │
│  wasmtime + wasi-preview2 + 组件模型 + 每插件一实例             │
└──────────────────────────────────────────────────────────────┘
```

- **① 核心**在宿主进程内（如 Cordis 核心也是进程内的 TS）。它提供「一切皆插件」的编排能力，但核心自身不必是 WASM（类似 Cordis 核心也是 JS）。
- **② 契约**让任意第三方 Rust 插件只需 `cargo component` 编译为组件，实现 WIT 暴露的入口。
- **③ 执行**把插件声明/事件/副作用翻译到 WASM 调用，并把 host 服务桥接回插件。

---

## 4. 从第一性原理定义「最小原语集」（Rust 版）

这些是第 1 节每个 Cordis 原语的 Rust 直接映射，构成核心 crate 的公开 API。

### 4.1 `Context` 与 `ServiceRegistry`

```rust
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// 一个服务实现：类型化的值 + 拥有它的 fiber + 可选可用性谓词
pub struct ServiceEntry {
    pub value: Arc<dyn Any + Send + Sync>,   // 服务值（trait object）
    pub owner: FiberHandle,                   // 生命周期 owner
    pub check: Option<fn() -> bool>,          // 依赖可用性
}

/// 全局服务仓库，按 (scope_label, name) 索引
#[derive(Default)]
pub struct ServiceStore {
    by_label: HashMap<ScopeId, HashMap<String, ServiceEntry>>,
}
```

**要点**：`Arc<dyn Any>` 是「ctx.<name>」在 Rust 里的等价物——按字符串名查表、取回时用 `downcast_ref::<T>()` 转成具体类型。这是对第 1.2 / 1.6 的直接实现。

### 4.2 `Fiber` 与可逆副作用

```rust
pub type Disposer = Box<dyn FnOnce() + Send + Sync>;
pub type EffectFn = Box<dyn FnOnce(&mut Context) -> EffectResult>;

pub enum EffectResult {
    Disposer(Disposer),                       // 单个 disposer
    Iter(Vec<Disposer>),                      // 生成器产出多个
    None,
}

pub struct Fiber {
    pub state: FiberState,        // Pending/Loading/Active/Failed/Unloading/Disposed
    pub inject: Vec<String>,      // 依赖服务名
    pub disposers: Vec<Disposer>, // 逆序运行
    pub handle: FiberHandle,
}

impl Fiber {
    /// 副作用立即执行，返回的 disposer 逆序回滚
    pub fn effect(&mut self, f: EffectFn) -> Disposer;
    pub fn unload(&mut self);     // 逆序跑 disposers
}
```

**要点**：`effect()` 是「注册即副作用、卸载即逆序回滚」的核心，对应 1.3。**所有** `on()`/`provide()`/`register()` 内部都 `effect()`。

### 4.3 事件总线（四种模式）

```rust
pub enum DispatchMode { Emit, Parallel, Serial, Waterfall }

pub struct EventBus {
    listeners: HashMap<&'static str, Vec<(FiberHandle, Box<dyn Listener>)>>,
}

impl EventBus {
    pub fn on(&mut self, name: &'static str, f: ...) -> Disposer;
    pub fn emit(&mut self, name: &'static str, payload: EventPayload);
    pub async fn parallel(&mut self, name: &'static str, payload: EventPayload);
    pub async fn serial(&mut self, name: &'static str, payload: EventPayload) -> Option<EventPayload>;
    pub fn waterfall(&mut self, name: &'static str, args: Vec<EventPayload>, next: NextFn);
}
```

**要点**：对应 1.5。`emit` 同步顺序、`serial` 首个非空短路、`waterfall` 洋葱中间件——与 DSH 里 `tools/pre-execute` 这些策略拦截点一一对应。

### 4.4 服务变更 → 批量重求值（notify）

```rust
impl ServiceStore {
    /// 服务变更后，重算所有依赖它的 fiber：缺则卸载，齐则加载
    pub fn notify(&mut self, changed: &[&str], registry: &mut Registry);
}
```

**要点**：对应 1.4 的 `ReflectService.notify`，是「插件加载顺序由依赖决定」的执行者。

### 4.5 上下文作用域

```rust
pub struct Context {
    pub services: ServiceStore,
    pub events: EventBus,
    pub fiber: FiberHandle,
    parent: Option<ContextHandle>,   // extend 链
    isolate: HashMap<String, ScopeId>, // isolate 遮蔽
}
```

**要点**：对应 1.6。`extend` = 挂 `parent`；`isolate` = 在 `isolate` 表里为某服务给新 `ScopeId`。

### 4.6 配置

```rust
pub trait Plugin {
    const NAME: &'static str;
    const INJECT: &'static [&'static str];
    const PROVIDE: &'static [&'static str];
    type Config: DeserializeOwned + Default;   // 强校验
    fn apply(&self, ctx: &mut Context, config: Self::Config)
        -> Result<Box<dyn FnOnce(&mut Context) + Send>, PluginError>;
}
```

**要点**：对应 1.7。用 `serde` + `serde_json` 做 JSON config 校验（等价 `standard-schema` 的最简子集）。

---

## 5. 宿主侧插件 vs WASM 插件的「统一模型」

「一切皆插件」的核心目标是：**新增一个插件 = 挂一个入口，不改核心**。Rust 侧要支持两类插件，且让核心以**同一套心智**对待它们：

### 5.1 类 1：进程内 Rust 插件（`dyn Plugin`）
- 编译进宿主，或经 `dlopen` 动态链接。
- 优点：开发快、能共享类型、调试友好。
- 适用：核心自带插件、需要紧耦合类型 / 高性能热路径的插件。

### 5.2 类 2：WASM 组件插件（`.wasm`，wasmtime）
- `cargo component build` 成 `dsh-plugin` 世界里的组件。
- **隔离**：每插件独立线性内存，宿主经 WIT 进出口调用。
- **能力授予**：插件想碰文件/网络/进程，必须 import 对应的 host 函数；宿主可按配置决定「这个插件能不能调 fs」。

### 5.3 统一抽象：`PluginBackend`

把两类插件收敛到一个 trait：

```rust
pub trait PluginHost {
    fn load(&mut self, manifest: &PluginManifest) -> Result<LoadedPlugin, PluginError>;
    fn call(&mut self, id: PluginId, entry: EntryPoint, payload: &[u8]) -> Result<Vec<u8>, PluginError>;
    fn unload(&mut self, id: PluginId);
}
```

- 进程内实现：直接把 `dyn Plugin::apply` 接到 `call`。
- WASM 实现：`call` 走 wasmtime 的组件调用（序列化成字节进出）。

核心（Context/Fiber/Registry/Events）只依赖 `PluginHost` 抽象，**不关心插件是编译进去还是 WASM 加载**。这就是把「一切皆插件」从口号落到接口：替换任一插件 = 换一个 manifest 指向不同的 `.wasm` 或 native 实现。

---

## 6. WIT 契约草案（`dsh-plugin.wit`）

用 wasmtime 组件模型定义插件 ↔ 宿主边界。这是第 3 节“② 插件边界”的具体化。

```wit
// 插件世界：插件导出的入口，以及它 import 的宿主能力
package dsh:plugin;

interface plugin-api {
  // 插件生命周期入口（插件 export 给宿主）
  record config {
    bytes: list<u8>,   // 序列化 config（json）
  }
  record context-op {
    // 宿主 API 调用，如 emit 事件、get 服务
    op: string,
    payload: list<u8>,
  }
  // 插件实现 "everything is a plugin" 的入口
  apply: func(config: config) -> result<list<u8>, string>;  // 返回序列化 disposer 描述
  dispose: func(disposer: u64) -> result<_, string>;
  // 事件/指令入口（宿主 → 插件）
  handle-event: func(name: string, payload: list<u8>) -> result<list<u8>, string>;
}

interface host-api {
  // 宿主能力（插件 import）：可被配置授予/撤销
  emit-event: func(name: string, payload: list<u8>);
  get-service: func(name: string) -> result<list<u8>, string>;
  provide-service: func(name: string, value: list<u8>);
  log: func(message: string);
  fs-read: func(path: string) -> result<list<u8>, string>;  // 条件授予
  subprocess-spawn: func(argv: list<string>) -> result<u64, string>; // 条件授予
}

world dsh-plugin {
  export plugin-api;
  import host-api;
  import wasi:cli/command@0.2.0;   // wasi-preview2 基础能力
}
```

**要点**：
- 插件**被动**：宿主调 `apply` 让其安装副作用，随后宿主调 `handle-event` 驱动它。插件回宿主则经 import 的 `host-api`——**权限在此授予/撤销**。
- 只要允许，第三方可用 `cargo component` 直接生成适配代码，无需手写 ABI。

---

## 7. Event / 副作用在 WASM 边界上的序列化策略

Cordis 的事件载荷与副作用在 JS 里是「引用传递」，Rust 边界必须改为**值传递（序列化）**：

- **载荷类型**：事件参数 / 服务值 / config 统一编码为 **JSON 字节**（对应 DSH「lossless JSON」的纪律）。重型二进制走 WASI 能力而非穿过边界。
- **副作用抽象化**：插件 `apply` 返回的不是闭包，而是**一组结构化的 disposer 描述**（如 `[{kind:"unlisten", event:"tools/pre-execute"}, ...]`）。宿主在插件侧「模拟」 disposer——因为闭包不能跨 WASM 线性内存传递。
- **事件回调桥**：插件注册监听 = 告诉宿主「把这个事件转发回来」，宿主经 `handle-event` 回调，插件内部再调度。

这一层是「WASM 一切皆插件」与「进程内 Cordis」**最大的差异**，值得单独立项攻坚。

---

## 8. 目录/workspace 规划（`F:\RustProjects\dsh-rs`）

```
dsh-rs/
├── Cargo.toml                  # workspace
├── crates/
│   ├── dsh-core/               # ① 核心：Context/Fiber/Registry/Events/Config/Scope
│   ├── dsh-plugin-wit/         # ② WIT 定义 + wit-bindgen 生成
│   ├── dsh-wasmrt/             # ③ wasmtime 宿主执行
│   ├── dsh-native-plugin/      # 进程内 PluginHost 实现（dyn Plugin）
│   └── dsh-loader/             # 从对话式 manifest（等效 cordis.yml + patch）组装
├── plugins/
│   └── hello/                  # 示例 WASM 插件（cargo component）
├── wit/
│   └── dsh-plugin.wit
└── examples/
    └── mini-agent/             # 最小 agent 循环，全由插件组装
```

---

## 9. 落地路线（里程碑）

### M0 —— 进程内核心（先验证原语，不碰 WASM）
- [ ] `dsh-core`：`Context`（containing service store + fiber）、`effect()` 逆序回滚、`EventBus`（四模式）、`notify` 依赖驱动重载、config 校验。
- [ ] 验收：用 `dyn Plugin` 写出「模型适配器」「工具注册」「会话日志」三个插件，再加一个「agent loop」插件，**它能正常运行最小对话**——证明「loop 本身可替换」。

### M1 —— 统一 `PluginHost` 抽象 + native backend
- [ ] 抽出 `PluginHost`，把 M0 的插件全部改走该抽象。
- [ ] 验收：换掉「会话日志」插件（native 换实现）不触及其他插件。

### M2 —— WASM 后端（wasmtime + WIT）
- [x] **已交付（2026，轻量 core-wasm FFI 路线，见 `crates/dsh-wasmrt` + `wasm-plugins/hello`）**：wasmtime 加载 wasm32 插件（导出 `plugin_apply`/`plugin_handle_event`/`plugin_dispose` + `alloc`/`dealloc`，导入 `host_log`/`host_emit`/`host_on`/`host_provide`/`host_get`），适配为 `dsh-core::Plugin`。
- [x] hello.wasm 插件：apply 时提供服务 + 注册监听；handle_event 时回读服务并 host_emit。
- [x] 验收通过：WASM 插件注册服务、触发事件（双向）、随卸载回滚副作用（`m6_wasm.rs`）。
- [ ] 组件模型（cargo-component + WIT world）与 WASI preview2 能力留作后续升级路线。

### M3 —— 能力授予 + 沙箱
- [x] ABI 能力位（PROVIDE/EMIT/GET）在 host import 侧检查，被拒返回错误码并记日志（`Capabilities`）。
- [x] 验收通过：禁用 provide 的 wasm 插件 apply 失败（fiber FAILED + 拒绝日志）。
- [ ] WASI fs/subprocess 能力（组件模型 import 按实例授予）留作后续。

### M4 —— manifest 组装（等效 cordis.yml）
- [ ] `dsh-loader`：从 JSON/TOML manifest 描述 `{services:[], events:[], plugins:[]}`，等价 Cordis 的 bundle/patch 组合；支持依赖排序与 overlays。
- [ ] 验收：改 manifest 一行启用/停用/替换插件，重启即生效。

### M5 —— 热重载（HMR）
- [ ] 运行时重载单个插件：unload 旧 fiber（逆序 disposers）→ load 新实现 → `notify` 依赖方。
- [ ] 验收：替换插件后事件流/工具集无缝切换。

---

## 10. 验证策略

- **单元**：`fiber.unload()` 逆序、`EventBus` 四模式、`notify` 依赖重算——用 `#[cfg(test)]` 全覆盖。
- **组合/快照**：M0 的「插件组装 mini-agent」跑一条 key-less 的固定 prompt，输出为快照（对标 DSH 的 `test:snapshot`）。fixtures 要在 CI 可复现。
- **HMR 安全**：dispose fiber → 断言事件/工具已被移除（对标 DSH 的 registry-disposal 测试）。
- **沙箱**：M3 权限矩阵单测 + 一条拒绝路径测试。
- **契约**：WIT 定稿后用 `wit-bindgen` 生成两侧，编译期强约束 ABI 稳定。

---

## 11. 结论与取舍

- **可行性**：高。关键洞察是 **Cordis 的 80% 插件可以降到「无状态入口函数 + 副作用 effect」**，这正好是 WASM 友好的形状；真正难的是 1.3/1.4 的「可逆副作用」与「依赖驱动重载」在跨线性内存边界的表达，已用「结构化 disposer 描述」对策。
- **核心不必 WASM**：如同 Cordis 核心是进程内 JS，`dsh-core` 进程内 Rust 即可，WASM 用于**第三方插件**这一层，「一切皆插件」体现在「任意环节可替换/可挂载」，而非「内核也跑在 WASM」。
- **推荐工具链**：`wasmtime` + `wasmtime-wasi`(preview2) + `cargo component` + `wit-bindgen`；配置文件用 `serde_json`；异步用 `tokio`。
- **主要风险**：① 跨边界的闭包/disposer 语义（M2 攻坚点）；② reload/HMR 与 drop 时序；③ WASI preview2 的成熟度（注意 pin 版本）。

参考：
- Cordis primer / DSH architecture（本仓库 `docs/`）
- [wasm_plugin_system_example（wit-bindgen + wasmtime 插件宿主示例）](https://github.com/alez-dev/wasm_plugin_system_example)
- [wasmtime 组件模型 / WASI preview2 架构讨论](https://bytecodealliance.github.io/zulip-archive/stream/206238-general/topic/Architecture.20for.20Wasip2.20Host.2FPlugin.html)

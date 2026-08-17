# 将 Cordis 等效迁移到 Rust —— 细化方案

> 目标：把 vendored Cordis（`vendor/cordis`、`vendor/loader`、`vendor/include`、`vendor/schemastery`）作为**行为规范**，等效移植为一组 Rust crate，使 DSH 的「一切皆插件」内核可以脱离 Node/TypeScript 运行。
>
> 本方案是 [PLAN-rust-wasm-everything-is-plugin.md](PLAN-rust-wasm-everything-is-plugin.md) 的细化与修正：上一版回答「如何用 Rust+WASM 达成一切皆插件」，这一版回答「**如何把 Cordis 本身等效迁移到 Rust**」——先给出逐机制的**行为规格（可差分验证的等效性基线）**，再给出 Rust 类型/所有权/异步设计，最后给出差分验证与里程碑。
>
> 阅读前提：已通读 `deepseek-harness/vendor/cordis/src/*`（context / registry / fiber / events / reflect / service / logger / utils）、`vendor/loader/src/*`（index / entry / group / tree / isolate / utils）、`vendor/include/src/index.ts`、`vendor/schemastery/src/index.ts`。

---

## 0. 等效性的操作性定义

「等效」不是逐行翻译，而是**可观测行为一致**。定义如下（差分测试按此断言）：

给定相同的插件集合、相同的配置、相同的事件注入序列，两个运行时必须产生一致的：

| 观测面 | 内容 |
|---|---|
| 事件轨迹 | 每个 `internal/dispatch`：事件名、分派模式、监听器收到的参数、是否调用了 `next()`、短路/委托结果 |
| 服务注册表终态 | 每个服务名 → 实现者 fiber、isolate 标签、`check()` 可用性 |
| fiber 生命周期轨迹 | 每个 fiber 的 state 转换序列（`internal/status` 事件序）、disposer **逆序执行**序列、`uid` 生命周期 |
| 加载器事务 | entry 的 init/update/dispose/rollback 顺序、`loader/*` 事件、config 写回内容 |
| 日志序列 | 每条 `Message` 的 `{sn, ts 归一化, name, type, level, args 归一化}` |

**明确不移植的项**（记录为差异，差分时忽略）：
- `traceable` / `withProps` / `createShadow` / `composeError` / `buildOuterStack` —— 全部服务于 JS 错误堆栈修饰与 receiver 透传，Rust 用 `std::backtrace` 与 `anyhow` context 代替。
- `symbols.*` 的「跨 realm 全局 symbol 身份」—— 用编译期 `u64` id / 类型常量代替。
- `WeakRef`（logger Message.fiber）—— 用弱句柄（`Weak`）或直接省略（该字段仅诊断用途）。
- Proxy 的 `isSpecialProperty`（`_` 前缀、数字串、`prototype`/`then` 保留名）—— Rust 无属性代理，直接访问器 + 显式方法。
- `!!js` 任意 JavaScript 表达式求值 —— 迁移为**结构化表达式子集**（见 §4.6）。
- `process.env.CORDIS_SHARED`、`Math.random` 的 id 生成 —— 可注入的确定性来源（测试需要）。

---

## 1. 语义规格（行为规范）—— 逐机制

> 每节先给 TS 侧**精确规则**（作为移植基准），再给 Rust 映射要点。Rust 细节集中在 §2。

### 1.1 插件形状与 Registry（`registry.ts`）

- `resolve(plugin)`：函数 → 自身；`{ apply }` 对象 → `apply`；否则无效 → `plugin()` 抛 `'invalid plugin, expect function or object with an "apply" method'`。
- `plugin(plugin, config?, getOuterStack?)`：
  1. `resolve` → `callback`；`ctx.fiber.assertActive()`（fiber 已 dispose 则抛 `INACTIVE_EFFECT`）。
  2. runtime 复用：`_internal.get(callback)` 存在则复用；否则建 `Runtime{ name, callback, fibers: DisposableList, Config: plugin.Config }`（`name` 取自 `plugin.name`，若为 `'apply'` 则为 undefined）。
  3. `new Fiber(ctx, config, Inject.resolve(plugin.inject), runtime, getOuterStack)`。
  4. 返回 `Fiber & PromiseLike<Fiber>`：`then` → `fiber.await().then(...)`。
- `Inject.resolve(inject, result)`：`null` → 空；数组 → 每个名字 `result[name]=null`；`checkProto` 标记的对象 → 递归合并原型链再合并自身；普通对象 → 直接键拷贝。
- `delete(plugin)`：dispose 该 runtime 所有 fiber，删除 runtime。
- **类插件**：`isConstructor(callback)`（有 `prototype` 且非 generator/async generator）→ `new callback(ctx, config)` + 运行 `[initHooks]` + 调用 `[init]`（async generator，yield 的 disposer 由 effect 收集）。
- 元数据：`Plugin.Base` = `{ name?, Config?, inject?, provide?, intercept? }`。

### 1.2 Context 与服务解析（`context.ts` + `reflect.ts` 的 proxy handler）

Context 是一个 Proxy，读 `ctx.<name>` 走解析器。规则：

- `Context` 接口成员：`[isolate]`、`[intercept]`、`root`、`baseUrl?`、`events`、`logger`、`reflect`、`registry`、`fiber`（由 fiber 注入）。
- 根构造：root context 自身即 proxy；`fiber` = 根 fiber（runtime=null，uid=0，state=ACTIVE，store={}，dispose=restart）。
- `extend(meta)`：子 context，原型链指向父（traceable），meta 的 own props 覆盖。
- `isolate(name, label?)`：子 context，isolate 表副本 + `name → label ?? Symbol(name)`。
- `intercept(name, config)`：子 context，intercept 表副本 + `name → config`。
- **proxy get（非特殊属性）**：
  1. `Reflect.has(target, prop)` → 返回 `getTraceable(ctx, value)`（直接成员）。
  2. `props[prop]` 为 accessor → 调 get hook。
  3. `!ctx.fiber.runtime`（根 context）→ `reflect.get(prop, false)`（非严格读全局 store）。
  4. 否则 `waterfall('internal/get', ctx, prop, error, next)`：沿 fiber 链向上——`fiber.store[prop]` 命中返回；`prop in fiber.inject` 且未命中 → 抛 `cannot get required service "<prop>" in inactive context`；`!fiber.runtime` → 抛；父 isolate 标签不同 → 抛；否则 `fiber = fiber.parent.fiber` 继续。
- **proxy set**：`isSpecialProperty` → 直接；无 prop 定义 → 根 context 直接 set，否则抛 `cannot set property ... without provide`；accessor → set hook；否则 `waterfall('internal/set', ...)` → `reflect.set`。
- `has`：特殊属性 / 自身 / `props`。
- `extend`/`isolate`/`intercept` 都不修改父 context（原型链 + 表副本）。

**Rust 映射要点**：无 Proxy。等价物是 `ctx.get(name)` / `ctx.get::<T>(name)` 显式方法 + 少量直接访问器（`ctx.root`/`ctx.events` 等）。fiber 链解析与 isolate 标签检查逻辑 1:1 保留。`internal/get`、`internal/set` waterfall 保留（loader/隔离插件依赖）。

### 1.3 Reflect：服务仓库（`reflect.ts`）

- `store: Dict<Impl, symbol>`：按 **isolate 标签 symbol** 索引；`Impl = { name, value, fiber, check? }`。
- `props: Dict<Property>`：`{type:'service'} | {type:'accessor', get, set?}`。
- `_getImpl(name, strict=true)`：`key = ctx.isolate[name]`；`store[key]`；strict 且 `impl.fiber.state !== ACTIVE` → 无。
- `get(name, strict=true)`：`getTraceable(ctx, _getImpl(name, strict)?.value)`。
- `set(name, value, error?)`：impl 不存在抛；`impl.fiber !== ctx.fiber` 抛 `cannot set property ... in multiple fibers`；写 `impl.value`。
- `provide(name, value?, check?)`：`ctx.fiber.effect` 内：
  - 声明 prop：未声明 → `{type:'service'}`；已是 accessor → 抛。
  - `ctx.root.isolate[name] ??= Symbol(name)`（根隔离标签全局注册）。
  - `key = ctx.isolate[name]`；`store[key]` 已存在 → 抛 `service "<name>" has been registered at <fiber.name>`。
  - 写入 `store[key]` 与 `ctx.fiber.store[name]`；若 fiber 已 ACTIVE → `notify([name])`。
  - disposer：`delete store[key]` → `notify([name])` → `await` 所有受影响 fiber → `delete ctx.fiber.store[name]`（**先依赖后自身**）。
- `accessor(name, options)`：effect；`name in props` 抛；写 `props[name]`；disposer 删除。
- `mixin(source, mixins)`：effect generator；每个 `(key, value)` → accessor：get = `Reflect.get(ctx[source], key, withProps(receiver, service))`（绑定）；set 同理。
- `notify(names, filter?)`：
  1. 遍历 `registry.values()` → 每个 `runtime.fibers` → 每 fiber：`hasUpdate` = 任一 name `in fiber.inject` 且 `filter(fiber.ctx, name)`（默认：isolate 标签匹配）。
  2. 对命中的 fiber：逐个 `_checkImpl(name)` → `_refresh()` → 收集。
  3. 对每个 name：以 `self = Object.create(ctx)` + `self[filter] = target => filter(target, name)` 为 thisArg `emit('internal/service', name, value)`。
  4. 返回 fibers。

**Rust 映射要点**：`ServiceStore` 用 `HashMap<ScopeId, HashMap<String, Impl>>`；`Impl.value: Arc<dyn Any + Send + Sync>`，取回时 downcast。`notify` 遍历逻辑 1:1。`provide` 的「先删 store → notify → await 依赖 → 再删自身」顺序是硬约束。

### 1.4 Fiber 状态机与 effect（`fiber.ts`）

状态：`PENDING → LOADING → ACTIVE ⇄ (dispose) → DISPOSED`，另有 `FAILED`、`UNLOADING`。

- 状态判定：`uid === null` → DISPOSED；`_error` → FAILED；`epoch !== INACTIVE` → ACTIVE；否则 PENDING。
- **根 fiber**：uid=0，state=ACTIVE，store={}，runner epoch=''，`execute` 空操作，`dispose = restart`。
- **插件 fiber 构造**：
  1. `uid = ++counter`；`ctx = parent.extend({ fiber: this })`。
  2. `inject` 的 intercept config 拷贝进 `ctx[intercept]`（null 跳过）。
  3. runner：`execute` = 类插件（构造 + initHooks + `[init]`）或函数插件 `callback(ctx, config)`。
  4. `dispose = parent.fiber.effect(...)`：disposer 内 `uid=null` → `emitPluginDisposed` → 若 `registry.has(callback)`：从 runtime.fibers 移除，空了删 runtime → `_setEpoch(INACTIVE)` → 无 inertia 则 `_updateState(UNLOADING + inertia=_unload())` → `while inertia: await`。
  5. 发布 `internal/plugin`（此时 loader 可扩展 `fiber.inject`）。
  6. 对每个 inject 名 `_checkImpl` → `_refresh()`。
- **`effect(execute, label)`**：
  - `assertActive()`（uid !== null）；`state === UNLOADING` 抛 `INACTIVE_EFFECT`。
  - 执行 body，收集 disposer（函数 / 可迭代（生成器逐个收集）/ promise / async iterable；其它形状抛 `TypeError('Invalid effect')`）。
  - 返回 wrapper disposer：`disposables.splice(0).reverse()` 逐个运行；幂等（第二次调用 no-op）；async 链式；`EffectMeta{label, children}` 诊断树（`symbols.effect`）。
  - 支持「setup 中 unload」的重入（`setupBarrier`、`finalizeDisposal`、`inFlight` 追踪）——**这是最精细的部分，务必 1:1 移植**。
- `_checkImpl(name)`：`_getImpl(name, true)` 无 → 删 `store[name]`；`check && !check()` → 删；否则写 `store[name] = impl`。
- `_refresh()`：`epoch = ''`，对每个 inject 名：无 impl → epoch=INACTIVE 停止；否则 `epoch += ':' + impl.fiber.uid`。→ `_setEpoch(epoch)`。
- `_setEpoch(epoch)`：相等或 `inertia` 在途则返回；否则 `_updateState`：INACTIVE→ACTIVE 则 `inertia=_reload()`（LOADING），否则 `inertia=_unload()`（UNLOADING）。
- `_reload()`：
  1. `store = {..._store}`；记 `oldEpoch`。
  2. `await Promise.resolve()` —— **必须让出一次**（等价于异步边界）。
  3. epoch 未变：`config = _resolveConfig(_config)`（`internal/config` waterfall → `resolveConfig(runtime, config)` 同步 schema 校验）→ `await _execute(runner)` → `_error = undefined`。
  4. 出错：`ctx.logger.error(reason)`；`_error = reason`；`epoch = INACTIVE`。
  5. `_updateState`：epoch 仍等于 oldEpoch → `inertia = undefined`；否则 `inertia = _unload()`（UNLOADING）。
- `_unload()`：`Promise.all(disposables.clear().map(每个 disposer 用 composeError 包裹))`（**并行清理、错误含化**）→ `store = undefined` → `_updateState`：INACTIVE → `inertia=undefined`；否则 `inertia=_reload()`（LOADING）。
- `await()`：`while inertia: await`；`_error` → throw；返回 this。
- `restart()`：`assertActive` → `_setEpoch(INACTIVE)` → `_refresh()` → `await()`。
- `update(config, noSave=false)`：
  - `assertActive`；`_config = config`。
  - 非 ACTIVE：清 `_error` → `_setEpoch(INACTIVE)` → `_refresh()` → 返回。
  - ACTIVE：`config = _resolveConfig(config)` → `waterfall('internal/update', this, config, noSave, next => { this.config = config; this._error = undefined; return this.restart() })`。
- `resolveConfig(runtime, config)`：有 `Config` → 同步 standard-schema 校验（async 校验直接抛不支持）；失败 → `ValidationError`（聚合 issue 消息）。
- `emitPluginDisposed`：dispatch `internal/plugin`（emit 模式，错误含化）。

### 1.5 Events：四模式分派 + 内置事件（`events.ts`）

- `dispatch(type, args)`：
  1. `thisArg` = 首个参数为 object/function 则 shift（事件过滤用）；`name` = shift。
  2. 非 `internal/` 事件先 `emit('internal/dispatch', type, name, args, thisArg)`（诊断/轨迹钩子）。
  3. `filter`：`hook.global || !filter || filter.call(thisArg, hook.ctx)`（`thisArg[Context.filter]` 存在时）。
  4. 返回 callback 列表（bind 到 thisArg）。
- 四种模式：
  | 模式 | 实现 | 返回值 |
  |---|---|---|
  | `emit` | 同步顺序调用全部 | 无 |
  | `parallel` | `Promise.allSettled(map(async cb))`，有 reject → `AggregateError` | Promise<void> |
  | `serial` | 顺序 `await`，`isBailed(result)`（`!== null && !== false && !== undefined`）即停 | 首个 bail 值 |
  | `bail` | 同步顺序，同上 | 首个 bail 值 |
  | `waterfall` | `cbs=dispatch`；`inner=args.pop()`；`next=()=>{cb=cbs.shift()??inner; return cb(...args)}`；`args.push(next)`；`next()` | 最外层返回值 |
- `on(name, listener, options?)`：
  1. `fiber.assertActive()`。
  2. `listener = ctx.reflect.bind(listener)`（traceable 包装）。
  3. `bail(this.ctx, 'internal/listener', name, listener, options)` —— 非空结果替换注册（`internal/update` 的专用钩子走这里）。
  4. `hooks = this._hooks[name] ||= []`；`fiber.effect(() => { hooks[prepend?'unshift':'push']({ctx, callback, ...options}); return () => unregister(hooks, callback) }, label)`。
- 内置 `internal/*` 事件（Events 接口 + EventsService 构造器注册的两个）：
  - `internal/plugin(fiber)`、`internal/status(fiber, oldValue)`、`internal/config(this=Fiber, config, next)`（waterfall）、`internal/service(this=Context, name, value)`、`internal/update(this=Fiber, config, noSave, next)`（waterfall）、`internal/get(ctx, name, error, next)`（waterfall）、`internal/set(...)`（waterfall）、`internal/listener(this=Context, name, listener, prepend)`（bail）、`internal/dispatch(mode, name, args, thisArg)`。
  - EventsService 构造器：`on('internal/listener')` 把非 global 的 `internal/update` 监听存入 `fiber._hooks['internal/update']`（DisposableList）；再以 `{global:true, prepend:true}` 注册 `internal/update` waterfall：先迭代 `fiber._hooks['internal/update']` 再 `next`。
- `once`：包装器首次调用后 self-dispose。
- 监听器是**副作用**：随 fiber 卸载自动移除（经 `fiber.effect`）。

### 1.6 Service 基类语义（`service.ts`）

- 构造：`name ??= ctor['provide']`；tracker `{associate, property:'ctx'}`；有 `[invoke]` → callable（函数-对象二象性）；`reflect.provide(name, self, this[check])`（**构造即注册、随 fiber 卸载**）。
- `[filter](ctx)`：`ctx.isolate[name] === this.ctx.isolate[name]`（isolate 过滤）。
- `[extend](props)`：callable/普通对象 + props。
- `[resolveConfig](base?, head?)`：沿 intercept 原型链收集本服务名的 own config（unshift）→ unshift base → push head → `Config.merge` 或 `Object.assign`。**这是 intercept 合并的权威实现**。
- `static [hasInstance]`：沿 constructor 链（代理安全）。

### 1.7 Logger（`logger.ts`）

- `LoggerService` 是可调用服务：`ctx.logger(name?)` 返回 `Logger`；`ctx.logger.info(...)` 直接用当前 fiber 名。
- `[invoke](name?)`：`_resolveConfig()`（沿 intercept 取 `logger` 配置）→ `fiber = (shadow ctx ?? ctx).fiber` → `name ??= config.name ?? hyphenate(fiber.name)` → `new Logger({name, level: config.level, meta:{fiber: WeakRef}})`。
- `Logger._method(type, level)`：
  1. 单 Error 参数特殊处理：有 `cause` → 递归记录 cause；`AggregateError` → 逐个记录。
  2. `sn = ++_snMessage`；`ts = Date.now()`。
  3. 遍历 exporters：`targetLevel = exporter.levels?.[name] ?? exporter.levels?.default ?? this.level ?? INFO`；`targetLevel < level` 跳过；否则 `exporter.export({sn, ts, type, level, name, ...meta, args})`。
- `Logger.format`：printf 风格 `%s %d %i %f %o %O %c %C`，`%%` → `%`；每行 `maxLength=10240` 截断 + `...`。
- `exporter(exporter)`：`ctx.effect` 注册/移除。
- 内置 buffer exporter（1000 条）。
- 等级：ERROR=0 < INFO=1 < WARN=2 < DEBUG=3。

### 1.8 Loader（`loader/src/*`）

**Loader 服务**（index.ts）：
- `provide('loader', this, check)`；`check`：`config.await && getTasks().length` → 不可用（依赖方保持 PENDING）。
- `internal/config`（global 监听）：无 entry 或父 fiber entry 相同 → 原样；插件是树载体（Group/Include，`[EntryGroup.key]`）→ 原样（config 保持字面）；否则 `interpolate(ctx, config)`（递归替换 `!!js`）。
- `internal/update`（global, prepend）：无 entry/noSave/父相同 → `next()`；否则 `await next()` 后 `entry.options.config = runtime.Config?.simplify ? unparse(config) : config`；`entry.parent.tree.write()`（**config 写回持久化**）。
- `internal/update`（global）：showLog 'reload' → `next()`。
- `internal/plugin`（global）：7 个 case 的**自处置检测**（fiber 创建时设 `fiber.entry` 并合并 entry 的 inject；判定「该 fiber 是否被 loader 之外的机制 dispose」→ 是则把 entry 标 `disabled` 并写回）。
- 构造时 `ctx.plugin(isolate)`。

**Entry**（entry.ts）：
- `ctx = loader.ctx.extend({[Entry.key]: this})`；构造即 `emit('loader/entry-init')`。
- `id`：父树 entry id + `EntryTree.sep(':' )` + 自身 id。
- `disabled`：group 恒 false；`disabledOf(options)`（`!!js` 表达式对 loader ctx 求值或布尔）；沿父 entry 链检查。
- `update(options, create, force)`：
  - 合并 → `sortKeys`（id/name 前置、config 后置、其余字母序）→ `deepEqual` 求 diff；无 diff 且非 force → 返回。
  - **无活动 fiber**：`fiber=undefined; options=candidate`；`!_disabled(candidate)` → `init()`；失败回滚 options；commit。
  - **candidate disabled**：options=candidate → `_dispose(previous)` → commit → `emit('loader/partial-dispose', entry, legacy, true)`。
  - **replace（diff 含 name/inject/group）**：import 新插件（name 变）或复用 `previous.runtime.callback` → options=candidate → `_dispose(previous)` → `_start(plugin)`；失败：options 回滚 → `_start(previousPlugin)` 回滚（再失败 → `AggregateError('rollback')`）→ emit partial-dispose → throw。
  - **仅 config 变**：options=candidate → `_patchContext(diff)`；失败回滚 `_patchContext(diff)`（rollback 语义）；commit → emit partial-dispose。
- `_patchContext(diff)`：`waterfall('loader/patch-context', entry, next)`：`Object.setPrototypeOf(this.ctx, this.parent.ctx)`；`fiber?.uid && (diff 含 config || group)` → `fiber.update(options.config, true)`。
- `_start(plugin)`：`_patchContext([])` → showLog 'apply' → `fiber = ctx.registry.plugin(plugin, options.config, getOuterStack)` → `await fiber.await()`；失败 `_dispose(fiber)` 再 throw。
- `init`：`_initTask ??= _init()`；finally 清空；`!getTasks().length` → `notify(['loader'])`；`_await()`。

**EntryTree**（tree.ts）：`entries()` 含嵌套 subtree；`getTasks()`（`_initTask || fiber.inertia`）；`await()`（轮询 allSettled 直到无任务，再 `_await` 全部，单失败抛、多失败 AggregateError，`notify(['loader'])`）；`ensureId`（随机）；`resolve`（`:` 分隔路径）；`create/remove/update`（move 带回滚）；`import`（`cordis:` 内置 / internal.import / 动态 import）；`write()` 抽象。

**EntryGroup**（group.ts）：`create`（复用或新建 Entry、parent 更新、`update(options, true, true)`、失败回滚）；`update(config[])` **事务**（allSettled 全量 create；失败 → 逆序 remove 新增 + 重建旧配置 → AggregateError）；`remove`；`stop`；`Group` 插件（嵌套 group，`internal/update` → `update(config)`，`[init]` async generator：yield stop → update）。

**isolate**（isolate.ts）：`Realm/LocalRealm/GlobalRealm`（symbol 仓库，`#entry-id` / `@label` 后缀）；`loader/entry-init` 建子 intercept/isolate 表；`loader/patch-context` 的 **7 步 realm 转移**（新 isolate 表 → 服务 diff（delim symbol）→ 设原型 → swap 表 → `next()` → 迁移 store 项 → `notify(diff 键, 定制 filter)` → 清理 delim）；`loader/partial-dispose` 的 realm GC。

### 1.9 Include（`vendor/include/src/index.ts`）

- `entryListSchema` = YAML JSON_SCHEMA + `!!js` Type（round-trip `{__jsExpr}`）。
- `applyEntryPatches(data, patches, warn)`：
  1. `structuredClone(data)`；`buildMap` 递归收集（含 group 内嵌）。
  2. 逐 patch：`insert` → 有 id 则插入目标 group（非 group 警告跳过）/ 无 id 则 push 顶层；**插入后立即 re-index**（同列表后续 patch 可命中）；`insert` 后 continue。
  3. 非 insert：无 id 警告跳过；target 不存在警告跳过；`name && name !== target.name` 警告跳过；其余键覆盖（`id` 除外）。
- 读/解析/校验（ENOENT → `initial` 写入或报错）；`writeFile`：`.tmp` + `rename` + 重试（10 次，50ms，`EACCES/EBUSY/EPERM`）；`write` 防抖（`setTimeout 0`）+ `writeQueue` 串行化；`refresh` 走 `applyQueue`（**非重入**，失败不 gate 后续任务）。
- `internal/update` 监听：`config.path` 变化 → enqueue `root.update(data)`。
- `[init]` async generator：`read(true)` → yield stop → `apply`。

### 1.10 Schemastery 语义子集（`vendor/schemastery/src/index.ts`）

- `Schema` 值 + `resolve(data, schema, options, strict)` → `[output, adapted?]`。
- 组合子（DSH 实际用到的）：`any / never / const / string / number / natural / percent / boolean / date / regExp / arrayBuffer / bitset / function / is / array / dict / tuple / object / union / intersect / transform / lazy`。
- `Options`：`autofix`（无效属性移除而非抛）、`ignore`、`path`（错误路径）。
- `Meta`：`default / required / disabled / collapse / badges / hidden / loose / role`（UI 元数据，随 config 目录导出）。
- `ValidationError`（聚合 issue，`path.join('.')` 消息）。
- standard-schema 互操作：`Schema['~standard'].validate(config)` → `{ value } | { issues }`。

---

## 2. Rust 架构与所有权设计

### 2.1 运行时布局

核心决策：**单线程 + 句柄（arena）+ 短借用门面**，不引入多线程。理由：Cordis 语义（同步 emit、动态作用域、任意重入）本质是单线程的；等效移植应保持该模型，多线程留给宿主层。

```rust
// dsh-core/src/runtime.rs
pub struct Runtime {
    pub contexts: Arena<ContextData>,   // extend 链节点
    pub fibers: Arena<FiberData>,       // 状态机节点（SlotMap 语义）
    pub services: ServiceStore,         // HashMap<ScopeId, HashMap<String, Impl>>
    pub props: HashMap<String, Property>,
    pub hooks: HashMap<&'static str, Vec<Hook>>,
    pub registry: HashMap<PluginKey, RuntimeRecord>,
    pub log: LoggerService,
    pub counter: u64,
}

pub struct Cordis { rt: Rc<RefCell<Runtime>> }   // 插件可见门面，可 Clone
```

- **内部算法**用 `&mut Runtime` 直写（`runtime.fiber_mut(id)` 经句柄取回），借用安全、可测。
- **插件回调**拿到 `Cordis`（`Rc<RefCell<Runtime>>` clone），每次方法调用是**短借用**（`borrow_mut()` 在方法内获取/释放），绝不跨 `await` 持有——与 JS 的「单线程 + 任意可变共享 + 动态作用域」对齐。
- **重入纪律**：任何「会调用用户代码（插件 body、监听器、disposer、check）」的公开方法，必须**在调用用户代码前释放借用**（`RefMut` drop 后再调），用户代码内可再入 `plugin()`/`emit()` 等（重新 borrow）。用代码结构（helper 拆分 setup/body/finish）+ review 规则保证。这是本设计唯一的「软约束」，也是最大风险点，需专门测试（见 §5 重入用例）。

### 2.2 句柄与借用边界

```rust
pub type FiberId = u64;
pub type ScopeId = u64;          // 替代 symbol 隔离标签
pub type PluginKey = usize;      // 插件回调指针身份（native）/ manifest hash（wasm）
pub type Disposer = Box<dyn FnOnce(&mut Runtime) + 'static>;

pub enum EffectOutcome {
    None,
    One(Disposer),
    Many(Vec<Disposer>),
    Async(BoxFuture<'static, EffectOutcome>),   // 异步 effect
}

impl FiberData {
    pub fn effect(&mut self, body: EffectBody, label: &'static str) -> Disposer { ... }
    // 1:1 移植 fiber.ts 的 effect()：assertActive、逆序、幂等、inertia、EffectMeta 树
}
```

- `Impl.value: Arc<dyn Any + Send + Sync>`；`ctx.get::<T>(name) -> Option<Arc<T>>` 经 downcast。
- `Hook` = `{ owner: FiberId, global: bool, prepend: bool, cb: Box<dyn Fn(&mut Runtime, &[Value]) -> HookResult> }`。
- **waterfall** 在 Rust 侧签名：`waterfall(rt, name, args: Vec<Value>, next: NextFn)`，`NextFn = Box<dyn FnOnce(&mut Runtime, Vec<Value>) -> HookResult>`；`HookResult` 区分 `Delegated(Value)` / `ShortCircuit(Value)` / `Veto`——对应 JS 的「调用 next / 返回 / 不调用 next」。

### 2.3 异步与并发

- 运行时用 **tokio `current_thread` + `LocalSet`**（future 非 Send 可跑）。
- `parallel`/`serial`/`_reload`/`_unload`/loader 事务照搬 `Promise.allSettled` 语义：Rust 侧 `futures::future::join_all` + 逐项错误聚合为 `AggregateError`。
- `_reload` 的 `await Promise.resolve()` 让出 = `tokio::task::yield_now().await`。
- 取消：Cordis 无 AbortSignal 概念（DSH 层有），核心不引入。

### 2.4 错误模型

```rust
pub enum CordisError {
    InactiveEffect,                       // INACTIVE_EFFECT
    InvalidPlugin,                        // invalid plugin ...
    MissingService(String),               // cannot get required service ...
    NotProvided(String),                  // cannot set property ... without provide
    MultipleFibers(String),               // ... in multiple fibers
    AlreadyRegistered(String),            // service ... has been registered at ...
    Validation(Vec<String>),              // schemastery issues
    LoaderStage { stage: LoaderStage, id: String, name: String, cause: Box<dyn Error> },
}
pub struct AggregateError { errors: Vec<Box<dyn Error>> }   // parallel/事务回滚
```

- 错误**消息字符串**与 TS 保持一致的风格（差分测试比对），但以结构化 code 为主。

### 2.5 值表示

- 配置与通用事件载荷：`serde_json::Value`（对应 `lossless JSON` 纪律）。YAML 解析用 `serde_yaml`（`!!js` 自定义 tag 见 §4.6）。
- 框架内类型化事件：`trait Event { const NAME; type Payload; const MODE: DispatchMode; }`（见 §2.6）。
- DSH 的「lossless JSON / deep-freeze」语义由宿主层负责；核心只保证 `Value` 的 clone 语义。

### 2.6 类型系统等效（TS declaration merging 的 Rust 等价）

TS 的 `declare module '@deepseek-ai/cordis' { interface Context { tools: ... } }` 是**编译期**开放类型。Rust 等价物：

```rust
// dsh-core/src/events.rs
pub trait Event: 'static {
    const NAME: &'static str;
    const MODE: DispatchMode;
    type Payload: Clone + serde::Serialize + serde::de::DeserializeOwned;
}
// 事件名是运行时字符串键；trait 只是编译期糖，跨 crate 无需中心注册表
// 与 Cordis 完全一致：hook 表按字符串键，TS merging 只影响类型层。

// 服务类型增补：每个 crate 声明自己的服务类型 + 名称常量
pub trait ServiceMarker { const NAME: &'static str; }
// ctx.get::<T>() 用 T: ServiceMarker + Any 解析
```

宏（可选）：`dsh-macros::define_events! { "tools/pre-execute" => struct ToolsPreExecute, Waterfall, payload = ... }`。

### 2.7 模块 ↔ crate 映射

| TS 源（vendor/） | Rust crate / 模块 | 说明 |
|---|---|---|
| `cordis/src/context.ts` | `dsh-core/src/context.rs` | Context 门面 + extend/isolate/intercept |
| `cordis/src/registry.ts` | `dsh-core/src/registry.rs` | 插件形状、Runtime record、Inject.resolve |
| `cordis/src/fiber.ts` | `dsh-core/src/fiber.rs` | 状态机、effect、reload/unload（最难，先做） |
| `cordis/src/events.ts` | `dsh-core/src/events.rs` | 四模式 + internal/* 事件 |
| `cordis/src/reflect.ts` | `dsh-core/src/reflect.rs` | ServiceStore、provide/get/set/accessor/mixin/notify |
| `cordis/src/service.ts` | `dsh-core/src/service.rs` | Service trait、intercept 合并 |
| `cordis/src/logger.ts` | `dsh-core/src/logger.rs` | Logger、Message、Exporter、printf format |
| `cordis/src/utils.ts` | `dsh-core/src/util.rs` | DisposableList、EffectMeta（其余不移植） |
| `cordis/src/fiber.ts` 的 `resolveConfig` | `dsh-schema` | schema 校验 |
| `loader/src/index.ts` | `dsh-loader/src/loader.rs` | Loader 服务 + 7-case 自处置检测 |
| `loader/src/config/entry.ts` | `dsh-loader/src/entry.rs` | Entry + update 事务 |
| `loader/src/config/group.ts` | `dsh-loader/src/group.rs` | EntryGroup + 事务 + Group 插件 |
| `loader/src/config/tree.ts` | `dsh-loader/src/tree.rs` | EntryTree |
| `loader/src/config/isolate.ts` | `dsh-loader/src/isolate.rs` | Realm + 7 步转移 |
| `loader/src/config/utils.ts` | `dsh-eval` | `!!js` 表达式子集求值 |
| `include/src/index.ts` | `dsh-loader/src/include.rs` | YAML/JSON + patch + 写回 + 队列 |
| `schemastery/src/index.ts` | `dsh-schema` | SchemaNode 值 + resolve + 校验错误 |
| —（新增） | `dsh-host` | `PluginHost` trait + native 后端（对接旧方案） |
| —（新增） | `dsh-wasmrt` | wasmtime 后端（对接旧方案 M2+） |
| —（新增） | `dsh-diff` | 差分测试工具（§5） |

---

## 3. 关键差异与决策表

| # | 差异 | TS 侧 | Rust 侧 | 影响 |
|---|---|---|---|---|
| D1 | 服务访问 | `ctx.tools`（proxy） | `ctx.get::<ToolRuntime>("tools")` | 语法差异，语义等价；宏可生成 `ctx.tools()` 快捷 |
| D2 | 可调用服务 | `ctx.logger("x")` / `ctx.logger.info()` 函数-对象二象性 | `ctx.logger("x")` 显式方法；`Logger` 门面 struct | 语法差异；log 输出一致 |
| D3 | traceable/堆栈修饰 | `composeError`/`buildOuterStack`/traceable proxy | 不移植；`backtrace` + `anyhow` | 差分忽略 stack 字段 |
| D4 | 表达式 | `!!js` 任意 JS eval（`with(ctx){eval}`） | 结构化子集（§4.6） | **语义缺口**：不支持的表达式迁移时需改写 |
| D5 | symbol 身份 | `Symbol.for` 全局注册 | `ScopeId: u64`（进程内全局计数器） | 等价 |
| D6 | 深拷贝 | `structuredClone` / `deepEqual` | `serde_json` round-trip / `serde_json::Value ==` | 等价 |
| D7 | 随机 id | `Math.random().toString(16)` | 注入 `Rng`（测试固定种子） | 测试可控 |
| D8 | 环境 | `process.env.CORDIS_SHARED`、Node fs/timers | `std::env`、tokio fs/time | 等价 |
| D9 | 泛型事件 | TS merging 合并 EventMap | `trait Event` + 字符串键（§2.6） | 编译期糖，运行时一致 |
| D10 | 并发 | 单线程事件环 | tokio current_thread | 语义一致；宿主可另起线程池隔离插件 |

---

## 4. 各机制的 Rust 落地要点

### 4.1 effect（先做，最核心）

```rust
impl FiberData {
    pub fn effect(
        &mut self,
        label: &'static str,
        body: impl FnOnce(&mut Runtime) -> Result<EffectOutcome, CordisError>,
    ) -> Result<Disposer, CordisError> {
        if self.uid.is_none() || self.state == FiberState::Unloading {
            return Err(CordisError::InactiveEffect);
        }
        let mut disposers: Vec<Disposer> = Vec::new();
        // 1:1 移植 execute/collect/dispose-wrapped 逻辑（含 setupBarrier/inFlight）
        // 返回幂等 wrapper
        Ok(wrapper)
    }
}
```

### 4.2 notify 与依赖驱动重载

```rust
impl Runtime {
    pub fn notify(&mut self, names: &[&str], filter: Option<ScopeFilter>) -> Vec<FiberId> {
        let mut affected = Vec::new();
        for (_, record) in self.registry.iter() {
            for fiber_id in record.fibers.clone() {   // 句柄拷贝，避免借用冲突
                let has = names.iter().any(|n| fiber_data(self, fiber_id).inject.contains(n)
                    && filter_matches(filter, fiber_ctx(fiber_id), n));
                if !has { continue }
                for n in names { self.fiber_mut(fiber_id).check_impl(n) }
                self.fiber_mut(fiber_id).refresh();
                affected.push(fiber_id);
            }
        }
        // emit internal/service（构造过滤 self）
        affected
    }
}
```

### 4.3 事件分派

```rust
impl Cordis {
    pub fn emit(&self, name: &str, args: Vec<Value>) {
        self.rt.borrow_mut().dispatch_emit(name, args);
    }
    pub async fn parallel(&self, name: &str, args: Vec<Value>) -> Result<(), AggregateError> { ... }
    pub fn waterfall(&self, name: &str, args: Vec<Value>, next: NextFn) -> HookResult { ... }
}
// dispatch 的 filter 逻辑（global || !filter || filter(hook.ctx)）原样保留
```

### 4.4 Service / intercept 合并

```rust
pub trait Service: Any {
    const NAME: &'static str;
    fn check(&self) -> bool { true }
    fn resolve_config(&self, rt: &Runtime, base: Option<Value>, head: Option<Value>) -> Value {
        // 沿 ctx 的 intercept 链收集本服务名 config，unshift base，push head
    }
}
```

### 4.5 Loader 事务与 7-step isolate 转移

- `Entry::update` 的四分支（无 fiber / disabled / replace / config-only）+ 回滚，按 §1.8 逐条移植。
- 事务用 `async fn` + 显式错误聚合；`loader/partial-dispose` 事件在正确时点 emit。
- isolate 转移：realm 表 `HashMap<String, ScopeId>`，`#entry-id`/`@label` 后缀；7 步算法照搬（diff 用 delim 标记转移归属）。

### 4.6 `!!js` 表达式子集（`dsh-eval`）

`evaluate(ctx, expr)` 在 JS 侧是 `with(ctx){ eval(expr) }`。Rust 侧实现一个**受限表达式语言**，覆盖 DSH 实际用法：

- 支持：标识符读取（`ctx.*`、`config.*`、`env.*`、常量）、成员访问、算术/比较/逻辑、三元、模板字面量、布尔/字符串/数字字面量。
- 不支持：函数调用（除白名单 `String/Number/Boolean/Array.isArray/Object.keys`）、赋值、语句、闭包。
- 求值作用域：loader context 的 `{ ctx, config, env }` 绑定。
- 解析用 `winnow`/`nom`（或 `evalexpr` 起步）；**未知标识符/语法 → 显式错误**（fail loud，对齐「misconfiguration fails loud」）。
- 迁移工具：给出常见 `!!js` 模式 → 子集 DSL 的改写表（如 `!!js config.env === 'prod'` → `config.env == "prod"`）。

### 4.7 Schema（`dsh-schema`）

`SchemaNode` 枚举直接建模 schemastery 组合子；`resolve(value, node, Options) -> Result<Value, Vec<Issue>>` 输出仍是 `Value`（不强行类型化，保持与 TS 一致的运行时语义）；类型化输出由调用方 `serde` 派生转换。`autofix`/`default`/`transform`/`union`/`intersect`/`lazy` 的精确行为照 schemastery 移植（它有大量边界行为：default 合并、autofix 删属性、transform 的 preserve 标志等）。UI 元数据（badges/role 等）原样保留供 config 目录生成。

---

## 5. 差分验证策略（`dsh-diff`）

### 5.1 场景 DSL

用 JSON 描述「框架行为剧本」，两侧各自实现同一剧本：

```json
{
  "scenario": "loader-config-hot-reload",
  "plugins": {
    "a": { "apply": "ctx.effect(dispose_fn)", "inject": [], "config": {} }
  },
  "steps": [
    { "op": "create-entry", "id": "a", "plugin": "a", "config": { "k": 1 } },
    { "op": "emit", "event": "e1", "args": [1, 2] },
    { "op": "update-entry", "id": "a", "config": { "k": 2 } },
    { "op": "dispose-entry", "id": "a" }
  ],
  "trace": "expected-normalized-trace"
}
```

### 5.2 规范化 trace

两个运行时都输出**规范化事件日志**（同一格式）：

```
emit      e1             [1,2]
fiber     a              PENDING→LOADING→ACTIVE
disposer  a#1            run (reverse order)
loader    a              apply
internal  update         a {k:1}→{k:2}
...
```

归一化规则：`sn`/`ts` 替换为序号；stack 字段剔除；`Value` 按 canonical JSON 排序比较。`dsh-diff` 逐行 diff，CI 门禁。

### 5.3 必测场景清单

1. **effect 语义**：多 disposer 逆序、async effect、effect 内再注册、effect body 抛错、卸载时 dispose 幂等、effect 内 dispose 自身 fiber（重入）。
2. **依赖重载**：A inject B；B 注册/卸载 → A 的 `_reload`/`_unload` 顺序；B 实现替换（provide 后 notify）。
3. **事件**：四模式各一组；waterfall 短路/委托/包装；prepend/global/filter（isolate 过滤）。
4. **intercept**：多层 intercept 合并顺序（`resolveConfig`）。
5. **loader 事务**：entry 增删改、config-only 热更、name 替换（dispose→start→回滚）、disabled 表达式、group 嵌套、include 文件热更（patch insert/override）、写回内容。
6. **isolate**：LocalRealm/GlobalRealm、7 步转移、realm GC。
7. **重入**：`internal/plugin` 监听器 dispose 父 fiber；loader 7-case 自处置。
8. **logger**：levels 过滤、Error/AggregateError 展开、format 占位符。

### 5.4 双运行时执行

- JS 侧：在 `deepseek-harness` checkout 内建一个测试宿主（tsx），执行场景 DSL 并输出 trace（复用现有 vendored Cordis）。
- Rust 侧：`dsh-diff` 读同一 DSL，驱动 `dsh-core`/`dsh-loader`，输出同一格式 trace。
- CI：`pnpm run test:diff`（TS 侧）与 `cargo test --package dsh-diff`（Rust 侧）对同一 fixture 目录比对。
- 也可先做「Rust 侧 trace 快照」：用 TS 侧输出录制成 `*.golden`，Rust 侧回归比对（与 DSH 的 `test:snapshot` 同思路）。

---

## 6. 里程碑与验收

| 里程碑 | 内容 | 验收标准 |
|---|---|---|
| **M0 核心原语** | `dsh-core`：fiber 状态机 + effect、四模式事件、ServiceStore + notify、Context 门面 | 通过 §5.3 的 1/2/3 类场景（Rust 单测 + 与 TS 差分） |
| **M1 服务与拦截** | Service trait、intercept 合并、accessor/mixin、logger | 4/8 类场景通过；`ctx.logger("x").info` 输出与 TS 一致 |
| **M2 Loader** | Entry/Group/Tree、update 事务、Loader 服务、7-case 检测 | 5/7 类场景通过（含回滚路径） |
| **M3 isolate + 表达式** | realm 转移、`dsh-eval` 子集、include 文件热更 | 6 类场景 + 改写表样例通过 |
| **M4 Schema** | `dsh-schema` 组合子全量（含 autofix/transform/lazy） | schemastery 边界用例差分通过 |
| **M5 差分基建** | `dsh-diff` + 双运行时 CI 门禁 + golden 快照 | 全场景双绿；新场景两侧同步加 |
| **M6 对接 WASM 层** | `dsh-host`（PluginHost）+ `dsh-wasmrt` 后端，核心跑通 WASM 插件 | 旧方案 M2-M3 验收在此复跑；核心不依赖 WASM |

顺序依赖：M0→M1→M2→M3→M4（核心线性），M5 从 M0 起并行铺（场景 DSL 先定义），M6 最后。

---

## 7. 风险与对策

| 风险 | 说明 | 对策 |
|---|---|---|
| **重入借用**（最高） | `Rc<RefCell>` 长借用会 panic；用户代码重入（监听器 dispose 父、effect 内 reload） | §2.1 纪律 + 专门重入测试（5.3-7）；核心算法用 `&mut` 直写减少 RefCell 面 |
| **effect 精细节** | setup 中 unload、inertia、async disposer 链的时序极细 | 1:1 移植 + 差分覆盖；先于其它机制完成 |
| **`!!js` 缺口** | 任意 JS 表达式无法迁移 | 子集 + 改写表；迁移前扫描真实 cordis.yml 的 `!!js` 用法清单 |
| **schemastery 边界** | autofix/transform/lazy 行为多 | 以 schemastery 测试用例为输入做差分 |
| **parallel/事务错误聚合** | AggregateError 语义差异 | 统一 `AggregateError` 类型，差分比对消息序 |
| **范围蔓延** | DSH 层（tools/session/loop）不在本次 | 明确边界：本方案交付框架 + 一个示例插件集（hello/logger/tool），DSH 层后续单独立项 |

---

## 8. 结论

- **等效迁移可行**：Cordis 的运行时机制可完整移植，关键是把「proxy/符号/动态作用域」译为「显式方法 + 句柄 + 短借用」，把「TS 类型合并」译为「trait + 宏糖」，把「JS eval」译为「受限表达式子集」。
- **最难的三块**：① `fiber.effect` 的完整重入/异步时序；② loader 事务与 7-case 自处置检测；③ 隔离 realm 的 7 步转移。三者都是纯算法移植，适合先用差分测试锁行为。
- **验证第一**：先搭 M0 + 差分工具，用 §5.3 场景锁住语义，再扩展机制——「行为规格 + 差分门禁」就是等效性的工程化定义。
- 与旧方案的衔接：旧方案（WASM 一切皆插件）的 `PluginHost`/`dsh-wasmrt` 作为 M6 的部署形态，构建在本迁移产出的 `dsh-core` 之上；核心自身保持 native 等效迁移。

参考（本仓库）：`vendor/cordis/src/*`、`vendor/loader/src/*`、`vendor/include/src/index.ts`、`vendor/schemastery/src/index.ts`、`docs/cordis-primer.md`、`docs/architecture.md`。

---

## 9. M0 交付记录（2026）

**状态：M0 已交付**（`cargo test` 22 项全绿，`cargo clippy --all-targets` 零警告）。

已落地（`crates/dsh-core/`，workspace 根 `Cargo.toml`）：
- `fiber.rs`：FiberState 状态机、`FiberData`（uid/epoch/store/disposers）、`collect_effect` 逆序+幂等 wrapper、`make_disposer`（共享一次性）。
- `events.rs`：DispatchMode / HookResult（`isBailed` 语义）/ Listener（`NextRef` 兼容 waterfall）/ Hook。
- `reflect.rs`：`Impl`（`Arc<dyn Any + Send + Sync>` 服务值、check 谓词）、`CheckFn`。
- `registry.rs`：`Plugin` trait（实例化 name/inject，dyn 兼容）、`RuntimeRecord`。
- `runtime.rs`：`Runtime` 竞技场（fibers/impls/services/hooks/registry）、作用域表、`register_plugin` / `notify` / `check_impls` / `refresh_fiber`（epoch 由 provider uid 编码）、`begin_load` / `finish_load` / `fail_fiber` / `begin_unload` / `finish_unload` / `dispose_fiber`、规范化 trace。
- `context.rs`：`Cordis` 门面——**收集-再执行纪律**（borrow 内只做数据变更，用户代码在无借用上下文运行，保证重入安全）；`plugin` / `effect` / `on` / `provide` / `get` / `get_typed` / `emit` / `bail` / `serial` / `parallel` / `waterfall` / `unload` 与 fiber 查询 API。
- `error.rs`：`CordisError`（INACTIVE_EFFECT 等）+ `AggregateError`。

测试（§5.3 场景 1/2/3 类）：
- `m0_effect.rs`（6）：逆序、幂等、嵌套、body 错误、inactive、dispose 后 effect。
- `m0_events.rs`（10）：emit 顺序、prepend、serial/bail 短路（null/false 不算 bail）、parallel、waterfall 委托/包装/短路/next 多次调用、监听器内重入 emit。
- `m0_reload.rs`（6）：inject 门控（Pending→Active）、撤销依赖→卸载、重提供→重载、无依赖立即加载、`get_typed` 读取、重复 provide 报错。

已知 M0 限制（M1 补齐，见 §2.3/§3 决策表）：
- 监听器/effect 为同步实现；async effect、`parallel`/`serial` 的 await 语义、`_reload` 的 `yield_now` 让出在 M1 引入。
- isolate / intercept / accessor / mixin 未实现（`scope` 字段已预留）。
- `internal/status`、`internal/plugin` 等内部事件仅写入 trace，未派发到钩子（M2 loader 需要时补齐）。
- `Cordis::set`（覆盖服务值）、`fiber.await()` 的异步形态未实现。

---

## 10. M1 交付记录（2026）

**状态：M1 已交付**（`cargo test` 39 项全绿，`cargo clippy --all-targets` 零警告）。

M0 之上新增（`crates/dsh-core/`）：
- `service.rs`：`Service` trait（`service_name` / `check`）。
- `logger.rs`：`LoggerState`（exporters + 默认 buffer 1000 条）、`Message`、`Exporter`/`ExporterConfig`、`Logger` 门面（error/info/warn/debug + `log_err`/`log_aggregate`）、`format_message`（printf 占位符 %s/%d/%i/%f/%o/%O/%c/%C、`%%` 转义、每行 10240 截断）、`hyphenate`。
- `reflect.rs`：`Property`（Service | Accessor）、`AccessorGet`/`AccessorSet`、`CheckFn` 无参化。
- `runtime.rs`：`props` 表（provide 声明 service 属性、accessor 冲突检查）、`pending_internal` 队列、`set_impl_value`（所有者校验）、`check_impls` 求值 check 谓词、`resolve_impl` 改为**全局 store 按 isolate 标签查询**（对应 Cordis `reflect.get`，非 proxy fiber 链）。
- `context.rs` 门面新增：`provide_with`（带 check）、`provide_service`、`set`（accessor 优先 + 所有者校验）、`get_value`、`intercept`（fiber 级，effect 管理）、`resolve_config`（base→根→…→当前→head 浅合并）、`accessor`、`mixin`（JSON 值成员转发）、`logger`/`logger_auto`/`exporter`/`logger_buffer`、`drain_internal`（internal/status、internal/plugin 派发到钩子，internal/plugin 先于加载转换，与 Cordis fiber 构造顺序一致）。

新增测试（§5.3 场景 4/8 + M0 缺口项）：
- `m1_intercept.rs`（3）：父/子层合并顺序、同层后者覆盖、base/head 优先级、intercept 随 fiber 卸载。
- `m1_logger.rs`（7）：占位符格式、级别过滤（**阈值 = 最高显示级别**，默认 INFO 下 warn/debug 过滤——实测 vendored Cordis 源码确认）、自动命名 hyphenate + intercept 覆盖、AggregateError 展开、buffer、exporter 随卸载移除。
- `m1_service.rs`（7）：provide_service 按名注册、check 门控依赖、set 所有者校验（MultipleFibers）、accessor get/set/生命周期/冲突、mixin 转发、internal 事件派发。

关键语义修正（差分思维发现）：
- `ctx.get` 是**全局 store 查询**（Cordis `reflect.get` 按 isolate 标签），不是 fiber 链——M0 实现已修正。
- Logger 阈值语义 `targetLevel < level → skip` 反直觉但忠实 Cordis：阈值是「最高显示级别」。

已知 M1 限制（M2 补齐）：
- 监听器/effect 仍为同步；async 语义、`_reload` 的 `yield_now` 让出在 M2（loader 事务需要 async）引入。
- isolate 作用域（`ScopeId` 已预留，`Service[filter]` 过滤未实现）、`fiber.await()` 异步形态。
- `Config.merge`（schema 驱动合并，M4 dsh-schema 引入）。

---

## 11. M2 交付记录（2026）

**状态：M2 已交付**（`cargo test` 48 项全绿，`cargo clippy --all-targets` 零警告）。

dsh-core 新增（loader 支撑）：
- `FiberData.entry: Option<String>`（loader entry 关联，沿 parent 链继承）+ `Runtime.pending_entry`（挂载入口时赋给下一个新 fiber）。
- `internal/plugin` 事件区分 `create` / `dispose` 两种载荷；dispose 在 `dispose_fiber` 排队（对齐 Cordis `emitPluginDisposed`）。
- `Cordis::plugin_arc`（以 `Arc<dyn Plugin>` 挂载，loader 按名复用）、`Cordis::update`（`internal/update` waterfall + 默认 inner=restart，loader 写回监听器拦截）、fiber 查询 accessor（`fiber_entry`/`fiber_uid`/`fiber_error`）。

新建 `crates/dsh-loader`（依赖 dsh-core）：
- `entry.rs`：`EntryOptions`（serde 可序列化，group config 数组复用）、`Entry`（fiber/subgroup/disposing）。
- `group.rs`：`EntryGroup`（有序子入口 id）。
- `loader.rs`：`LoaderState`（entry 树 + 插件仓库 + fiber 反查 + 写回记录）、`LoaderPlugin`（**7-case 自处置检测** + internal/update 写回）、`Loader` API（`new`/`register_plugin`/`create`/`update`/`remove`/`fiber`/`is_disabled`/`entries`/`take_writes`）、update 四分支事务（未启动/禁用卸载/config-only 热更/name 替换）+ 回滚、group 嵌套（挂载/递归卸载/子项 diff 同步）。

新增测试 `m2_loader.rs`（9 项，§5.3 场景 5/7）：
- 创建加载 / disabled 不启动 / 重复 id 报错
- config-only 热更（fiber 重启 + internal/update 写回 entry.options.config）
- disabled 更新 → 卸载
- name 替换（dispose 旧 + start 新）
- 替换失败回滚（旧插件重新启动、选项还原）
- group 嵌套挂载与递归卸载、子项同步（移除/新增/更新）
- **7-case**：绕过 loader 直接 dispose entry fiber → 入口被标 disabled 并写回

已知 M2 差异（方案 §1.8 的同步化处理，M3/M4 补齐）：
- 事务为同步实现（Cordis 用 async + `Promise.allSettled` 并行 create → 降为顺序，最终状态与事件顺序一致）；`yield_now` 让出、`EntryTree.await()` 轮询未实现。
- `!!js` disabled/config 表达式（M3 dsh-eval）；M2 仅布尔。
- isolate / intercept entry 选项（M3）；Group 以 loader 层展开实现（无独立 Group 插件 fiber）。
- 插件按名从 `Loader::register_plugin` 仓库解析（Rust 无动态 import；Cordis 按模块 specifier import）。
- `internal/config` 插值监听器（M3）；`Entry._patchContext` 的 isolate 7 步转移（M3）。

---

## 12. M3 交付记录（2026）

**状态：M3 已交付**（`cargo test` 65 项全绿，`cargo clippy --all-targets` 零警告）。

dsh-core 新增（isolate 作用域）：
- `FiberData.isolate: HashMap<String, ScopeId>`（服务名 → 作用域；继承 parent + 覆盖）。
- `Runtime.pending_isolate` / `pending_intercept`（loader 挂载入口时注入新 fiber）。
- `Runtime::alloc_scope()`（realm 独立作用域）、`resolve_scope(fiber, name)`（沿 fiber 链查 isolate，否则根作用域）。
- `insert_impl` / `remove_impl` / `set_impl_value` / `resolve_impl` / `check_impls` 全部改为**按调用方作用域解析**；`notify` 因作用域感知的 check_impls 而自然过滤（不同 realm 的提供者互不可见）。
- `run_load` 增加 **`internal/config` waterfall**（`!!js` 配置插值点）。

新建 `crates/dsh-eval`（§4.6 受限表达式子集）：
- 手写 tokenizer + 递归下降解析器 + 求值器；支持字面量/标识符/成员/索引/一元/算术/比较（`===`/`==` 宽松相等）/逻辑短路/三元/白名单调用（`String`/`Number`/`Boolean`/`Array.isArray`/`Object.keys`）、数组 `.length`、数字归一化（`7.0`→`7`）。
- `truthy`（JS 语义）、`interpolate`（递归替换 `{"__jsExpr": "..."}` 节点）。
- 不支持：赋值/语句/闭包/模板字符串/任意函数（fail loud）。

dsh-loader 新增（M3）：
- `EntryOptions` 增加 `disabled_expr` / `isolate` / `intercept`（serde 可序列化）。
- `load_plugin` 注入 LocalRealm（`{entry}:{service}` → 新作用域）/ GlobalRealm（label → 共享作用域）+ intercept 条目；`remove` 时 realm GC（本地清理 + 无引用全局 label 清理）。
- `entry_disabled` 支持 `!!js` 表达式（fail-closed）；`internal/config` 监听器做插值。
- `Loader::sync`（整树收敛，include 用）；`update` 改为**部分更新语义**（None/空 = 保留现值，对应 Cordis「仅合并传入键」）。
- 新增 `include.rs`：YAML/JSON 读取（serde_yaml）、`apply_entry_patches`（override/insert，name mismatch 跳过）、`write_back`（JSON/YAML 落盘）、`refresh`（手动热更）、`initial` 首建。

新增测试（§5.3 场景 6 + 改写表样例）：
- `m3_eval.rs`（6）：算术/比较/逻辑/三元/成员/白名单/插值/语法错误 fail loud。
- `m3_expr.rs`（3）：disabled_expr 门控 + config 热切换（prod/dev/prod）、求值失败 fail-closed、internal/config 插值（`{"__jsExpr": "config.k * 2"}` → 42）。
- `m3_isolate.rs`（4）：LocalRealm 隔离（不同 realm 提供者互不可见、根提供后加载）、GlobalRealm 共享、realm GC、intercept entry 选项合并。
- `m3_include.rs`（4）：读取挂载、patch override/insert、写回落盘、refresh 增删与 config 热更（同一 fiber 重启）。

已知 M3 差异（M4 补齐）：
- isolate 更新走 replace 分支（重建 fiber；Cordis 的 7 步 store 迁移在 patch-context 中不重建——行为等价、时序不同）；`Service[filter]` 事件级过滤未实现（服务级隔离已完整）。
- include 无文件 watcher（手动 `refresh()`）；patch 仅作用于根层；YAML `!!js` tag 用 `{"__jsExpr": ...}` 代替。
- async（`yield_now`、并行 allSettled、`EntryTree.await()`）仍未引入（M4 或专门 async 里程碑）。
- disabled_expr 求值失败 fail-closed（Cordis 抛错）；internal/config 插值失败保留原配置并记录。

---

## 13. M4 交付记录（2026）

**状态：M4 已交付**（`cargo test` 80 项全绿，`cargo clippy --all-targets` 零警告）。

新建 `crates/dsh-schema`（§1.10 Schemastery 移植）：
- `Schema`/`SchemaRef`/`SchemaKind`（Any/Never/Const/String/Number/Boolean/Function/Is/Bitset/Array/Dict/Tuple/Object/Union/Intersect/Transform/Lazy）+ `Meta`（required/default/loose/min/max/step/pattern/role/hidden/collapse/disabled/badges/description/comment/link/extra）。
- 组合子：`any/never/const/string/number/natural(=number.step(1).min(0))/percent(=..step(0.01).min(0).max(1).role(slider))/boolean/function/is/bitset/array/dict/tuple/object/union/intersect/transform/lazy` + meta 链方法（`required/loose/default/min/max/step/pattern/role/...`）。
- `resolve(data, schema, opts)`：nullable + required/default（intersect 链取首个非空 default）→ 类型 resolver；`loose` 回退默认；object/array/tuple/dict 逐项校验（`property`，`autofix` 删无效项并回退默认）；union 逐个尝试聚合错误；intersect strict 合并对象；transform 先校验再回调；lazy 惰性展开；路径前缀错误消息（`$.a[1]`）；`schema_to_string` 紧凑类型串。

dsh-core 接入：
- `Plugin::config_schema() -> Option<SchemaRef>`（Cordis `Plugin.Base.Config`）；`CordisError::Validation(String)`。
- `run_load` / `update` 经 `validate_config` 校验：失败 → `CordisError::Validation` → fiber FAILED（加载）/ 返回 Err（更新）；合法配置按 default 填充后传给 `apply` / 写回。

新增测试：
- `m4_schema.rs`（11）：object default 填充/required/路径消息、autofix 删无效+回退默认、string pattern/长度、number 范围/natural step/percent、array/tuple 长度与逐项、dict 键 schema、union 聚合错误消息、intersect 合并、transform、const/never/any、lazy 递归、meta 链 + toString。
- `m4_config.rs`（4）：合法配置 default 填充传给 apply、非法配置 → FAILED + Validation 错误、update 非法返回 Err / 合法热更填充、无 schema 原样通过。

已知 M4 差异：
- `function` 与 `is(Class)` 在 Value-land 不可表达（`is` 按 JSON 类型名映射）；bitset adapted 键数组不写回；`clone(default)` 用 serde_json 深拷贝。
- regex flags 仅支持 `i/m/s` 前缀；`toString` 的对象键排序为字母序（schemastery 为插入序）。
- `strict` 标志未贯穿（dict 坏键按跳过处理；Cordis 非 strict 抛错）。
- async 基建（`yield_now`、并行 allSettled、`EntryTree.await()`）仍待专门 async 里程碑。

---

## 14. M5 交付记录（2026）

**状态：M5 已交付**（`cargo test` 全绿 + clippy 零警告；8/8 场景 Rust↔TS trace 逐行一致）。

### 场景 DSL + dsh-diff（§5.1/5.2）

- 场景 DSL（JSON）：`plugins`（`apply` 为微型 DSL 操作序列：`log`/`log-config`/`effect`/`dispose-effect`/`on`/`on-prepend`/`on-return`/`on-waterfall`/`on-short`/`provide`/`intercept`/`resolve-config`/`plugin`）+ `steps`（`plugin`/`plugin-with-config`/`emit`/`serial`/`bail`/`waterfall`/`unload`/`update`）。
- 新建 `crates/dsh-diff`：Rust 侧场景解释器（用 dsh-core 执行 DSL），trace 全部写入 `Runtime.trace`（与 Cordis 事件/状态顺序自然交错）；CLI 支持打印 / `--golden` 校验 / `--record`。
- 规范化 trace 行格式（两侧一致）：框架层 `plugin:`/`status:`/`emit:`/`serial:`/`bail:`/`waterfall:`，解释器层 `apply:`/`log:`/`effect-reg:`/`dispose:`/`on:`/`provide:`/`intercept:`/`resolve-config:`，宿主层 `*-result:`。

### TS 侧宿主（diff/ts-host）

- 独立 npm 工程依赖 npm 原版 `cordis@4.0.0-rc.8`（vendored 为 4.0.1 的 rescope），`scenario-host.mjs` 解释同一 DSL 输出同格式 trace（`internal/plugin`/`internal/status` 监听 + apply 解释器）。
- `generate-goldens.mjs` / `verify-diff.mjs`：一键生成 golden + Rust 校验。

### 8 个核心场景（§5.3 类 1/2/3/4）

effect 逆序 / effect 幂等 / emit 顺序+prepend / serial-bail（null/false 不算 bail）/ waterfall 包装+短路 / 依赖门控（提供/撤销/重载）/ intercept 合并（父→子覆盖）/ update 热更。

### 差分发现并修复的三个 Cordis 语义（保真提升）

1. **notify 时机**：Cordis 只在提供者 fiber **ACTIVE** 时 notify 依赖方（apply 期间 provide 不通知，Active 转换时通知）——`finish_load` 现返回依赖方转换。
2. **微任务让出**：Cordis 插件加载/嵌套挂载有 `await Promise.resolve()` 让出——实现**两阶段延迟加载**（apply 期间触发的嵌套加载：Loading 状态同步、apply 在父 Active 前、Active 在父 Active 后）。
3. **`fiber.update()` 不返回 restart promise**（cordis 4.x）——TS 宿主补 `await fiber.await()`。

### 已知 M5 差异

- 同步核心（无真实微任务）；两阶段延迟覆盖嵌套/依赖顺序，但 3 层以上深嵌套的微任务交错不完全等价。
- 场景集聚焦 §5.3 类 1/2/3/4（effect/事件/依赖/intercept）；loader 事务与 logger 场景因 TS 侧需 `@koishijs/loader` 等额外依赖暂未纳入（Rust 侧已由 m2/m1 单测覆盖）。
- npm cordis 为 4.0.0-rc.8（vendored 4.0.1 fork），核心 API 一致。

---

## 15. M6 交付记录（2026）

**状态：M6 已交付**（`cargo test` 85 项全绿 + clippy 零警告；wasm 插件经 wasmtime 跑通）。

新建 `crates/dsh-wasmrt`（旧方案 M2-M3 验收落地，核心不依赖 WASM）：
- **C ABI**（wasm32-unknown-unknown，纯 `env` 导入，无 WASI）：插件导出 `alloc`/`dealloc`/`plugin_apply`/`plugin_handle_event`/`plugin_dispose`；宿主导入 `host_log`/`host_emit`/`host_on`/`host_provide`/`host_get`。
- `WasmPlugin`：把 wasm 插件适配为 dsh-core `Plugin`——配置 JSON 字节双向桥接（wasmtime 线性内存 + typed funcs）；`host_on` 记录事件名、apply 返回后统一注册转发监听器（规避 wasmtime 34 `func_wrap` 需 Send+Sync 闭包）；卸载时经 effect disposer 调用 `plugin_dispose`；provide/on 经 fiber 机制**随卸载自动回滚**。
- `PluginHost` 抽象：`PluginManifest`（native / wasm 字节 + `Capabilities`），`NativeHost::load` 统一加载两种来源。
- **能力授予**（M3 验收）：`Capabilities` 位集（PROVIDE/EMIT/GET），host import 侧检查，被拒返回错误码并记日志。
- 插件模板 `wasm-plugins/hello`（Rust cdylib → wasm32），测试按需 `cargo build` 生成。

新增测试 `m6_wasm.rs`（5 项，旧方案 M2-M3 验收）：
- wasm 插件注册服务 → `get_value("greeting")` 可读
- 事件双向：`emit("ping")` → wasm `plugin_handle_event` → `host_emit("pong")` → 宿主监听收到；wasm 侧 `host_get` 回读服务成功
- 卸载回滚：服务/监听消失，`plugin_dispose` 被调用（host_log 可证）
- 能力拒绝：禁用 provide → `plugin_apply` 返回 -1 → fiber FAILED + 拒绝日志
- `PluginHost` 统一加载 native + wasm

已知 M6 差异：
- 轻量 core-wasm FFI（非组件模型/cargo-component）；wasmtime 34 的 `func_wrap` 需 Send+Sync 闭包，故监听器注册由宿主 apply 侧统一完成；同一 `WasmPlugin` 实例多 fiber 并发挂载共享同一 wasm 实例（M6 单挂载）。
- `host_get` 返回 JSON 值；WASI 能力（fs/网络）未授予（M6 仅 ABI 能力位，WASI preview2 组件模型留待后续）。

---

## 16. M7 交付记录（2026）—— async 基建

**状态：M7 已交付**（`cargo test` 96 项全绿 + clippy 零警告；9 个差分场景：8 同步 + 1 async 深嵌套逐行一致）。

### 目标（HANDOFF §7 方向 1）

引入真实异步语义，替代两阶段延迟近似，使核心可承载 DSH 层（async 模型调用/工具执行）：
`tokio current_thread` 执行环境、async listener/effect、`yield_now` 微任务让出、
`parallel`/`serial` 真并发、`fiber.await()` 与 loader `EntryTree.await()`。

### dsh-core 新增（`crates/dsh-core/`）

- **依赖**：`futures-util`（默认特性关闭，只用 `join_all`/`LocalBoxFuture`；不引入
  `futures-macro`，规避离线构建问题）；dev 依赖 `tokio`（rt + macros，测试用
  `#[tokio::test]`）。
- `events.rs`：`AsyncListener` 类型（`(ctx, args) -> LocalBoxFuture<Result<HookResult, CordisError>>`）、
  `HookCallback` 枚举（`Sync(Listener) | Async(AsyncListener)`）；`Hook.cb` 统一为枚举，
  同步分派只处理 `Sync` 变体（`Async` 跳过并记录差异），顺序（prepend）语义保留。
- `fiber.rs`：`EffectOutcome::Async(LocalBoxFuture<EffectOutcome>)`（异步 disposer，
  支持嵌套 resolve）；`FiberData.async_disposers` + `take_async_disposers`；
  `collect_effect` 对 `Async` 存入异步列表（同步 wrapper 不含该部分）。
- `runtime.rs`：`AsyncTask` 枚举（`Apply(FiberId) | Finish(FiberId)`）+ FIFO
  `pending_async_loads` 微任务队列 + `async_mode` 标志（嵌套注册改走 async 入队）。
- `context.rs`：
  - `on_async` / `on_cb`（统一注册）；`parallel_async`（join_all + `AggregateError`，
    allSettled 语义：全部执行、错误聚合）、`serial_async`（顺序 await + bail，错误传播）。
  - `plugin_arc_async`（async 注册）：`run_transitions_async`（Load → Loading 同步 +
    Apply 入队；Unload 同步）+ `drive_async_loads`（FIFO：`Apply` = yield → apply →
    排入 `Finish`；`Finish` = yield → finish_load → notify 依赖方）。
  - `yield_now`（自实现 ready-再-pending future，不依赖 `futures_util::task`）。
  - `fiber_await`（轮询直到离开 Loading/Unloading；FAILED 传播）。
  - `unload_async`（同步 disposer 逆序 + 异步 disposer join_all 并行，错误含化）。

### 关键设计：`_reload` 的两个让出点（差分思维发现）

TS 侧 `fiber.ts` 的 `_reload` 有**两次** `await` 让出：apply 前 `await Promise.resolve()`、
apply 后 `await this._execute(...)`（即使同步值也让出一次）。嵌套 `ctx.plugin()` 在父
apply 内**同步**注册（Loading 同步）并入队。因此 3 层嵌套的交错是：
「b 的 apply 在 a Active 前、c 的 apply 在 a Active 后」。M5 的两阶段延迟只覆盖前两层，
第 3 层顺序与 TS 偏差（HANDOFF §6 记录）。M7 用 FIFO 微任务队列 + 真实 `yield_now`
精确复刻，**3 层嵌套与 TS 逐行一致**。

### loader / diff

- `Loader::await_idle`（等价 `EntryTree.await()`：轮询直到无 fiber 在 Loading/Unloading）。
- `dsh-diff --async`：CLI 支持 async 编排（tokio current_thread + `LocalSet`）；
  `Runner::run_async`；`verify-diff.mjs` 对深嵌套场景自动加 `--async`（`ASYNC_SCENARIOS`）。

### 新增测试（11 项）

- `m7_async.rs`（8）：parallel_async 全跑、错误聚合但 allSettled、serial_async bail、
  错误传播、yield_now 交错、fiber_await、unload_async 异步 disposer、async listener
  随卸载移除。
- `m7_await.rs`（2）：loader await_idle 稳定返回、依赖门控收敛。
- `m7_async_diff.rs`（1）：async 路径深嵌套（3 层）trace 与 TS golden 逐行一致。

### 已知 M7 差异（M8 补齐）

- 同步分派（emit/bail/serial/waterfall）跳过 async listener（Cordis 为 fire-and-forget）；
  `unload`（同步）跳过 async disposer（需 `unload_async`）。
- loader 事务仍同步（`Promise.allSettled` 并行 create 降顺序）；`EntryTree.await()`
  轮询由 `Loader::await_idle` 提供但未接入 loader 内部事务。
- async 加载路径（`plugin_arc_async`）与同步路径（两阶段延迟）并存：新场景用 async，
  既有 8 个同步场景 golden 不变（向后兼容）。

---

## 17. M8 交付记录（2026）—— 组件模型 + DSH 层缝 WIT 化

**状态：M8 已交付**（`cargo test` 101 项全绿 + clippy 零警告；C ABI 与组件模型双路径共存）。

### 目标（方向 B：组件模型优先）

按用户决策，先 cargo-component + WIT 定义 DSH 层 world，示例直接以 WASM 插件验证
「loop 本身可替换」——替代手写 C ABI 的升级路径，并落地 WASI preview2 能力授予。

### 工具链

- `cargo component` 0.21.1（`cargo install cargo-component --locked`）+ wit-bindgen 0.44
  （cargo-component 自带）+ `wasm32-wasip1` target。
- workspace `rust-version` 1.75 → **1.85**（wit-bindgen/wasm-tools 组件模型 MSRV 下限；
  HANDOFF 工具链声明本为 Rust 1.94+，1.75 是过期值）。

### dsh-wasmrt 新增（`crates/dsh-wasmrt/`）

- **WIT 契约**：
  - `wit/plugin.wit`（`package dsh:plugin`）：插件载体——导出 `apply`（配置 bytes→s32）、
    `handle-event`（事件名+payload→s32）、`dispose`；导入 host `log`/`emit`/`on`/`provide`/`get`
    （能力位沿用 `Capabilities`）。
  - `wit-dsh/dsh-loop.wit`（`package dsh:dsh`）：**DSH 层缝**——`session`（turn/step 边界 +
    user/assistant/tool 消息 + append/derive-messages）、`tools`（execute/register）、
    `llm`（generate）、`agent-loop`（run-turn）；world `dsh-loop` 导出 agent-loop、导入三缝。
- **`component.rs`**：`WasmComponentPlugin`（适配 `Plugin`）——`wasmtime::component::bindgen!`
  编译期 host 绑定（`DshPlugin` + `host_api::Host` trait）；`ComponentHostState` 实现
  `Host` 与 `WasiView`/`IoView`；`wasmtime_wasi::p2::add_to_linker_sync` 注册 WASI preview2。
- **Send 纪律**：wasmtime `IoView: Send` vs Cordis `Rc<RefCell>`——`ComponentHostState`
  不含 Cordis，apply 时经 `thread_local CURRENT_CTX` 桥接（单线程内安全）。
- `load_wasm_component_plugin`（宿主 API 入口）。

### 组件插件（`wasm-plugins/`）

- `hello-component`（`dsh:hello-component`）：等价 M6 hello——apply 提供 `greeting` 服务
  + 注册 `ping` 监听；handle-event 回读服务并 host_emit；dispose 无操作。
- `echo-loop`（`dsh:echo-loop`）：实现 `agent-loop` 缝——`run_turn` 在 WASM 插件内完成
  turn/step 驱动 + session 回写（turn/start → step/start → user/message →
  assistant/message → step/end → turn/end），**不依赖 LLM**，直接回显——证明
  「loop 本身可替换」：宿主只提供缝，loop 实现与替换发生在插件层。

### 新增测试（5 项）

- `m8_component.rs`（4）：组件插件注册服务、事件双向（ping→pong）、卸载回滚、
  能力拒绝（无 PROVIDE → apply -1 → FAILED）。
- `m8_dsh_loop.rs`（1）：WASM echo-loop 组件实现 agent-loop——`run_turn` 返回
  `{reason: completed, echo}` 且 session 缝被写入完整 turn/step 事件序列。

### 关键陷阱（HANDOFF §5 已记录）

- cargo-component path 依赖：一个 wit 目录只含一个 package；依赖放
  `[package.metadata.component.target.dependencies]`。
- wasip1 组件自动 import WASI → 宿主必须 `add_to_linker_sync`。
- WIT 保留字：`stream` 非法（改 `generate`）；`_` 非法（用 kebab-case `out-len-ptr`）。

### 已知 M8 差异（M9 补齐）

- 组件路径 host `get` 为占位（无线性内存句柄；bytes 版待扩展）；WASI 授予为默认
  `WasiCtxBuilder`（fs/网络按 caps 精细授予待做）；loader 未接入 `PluginHost`
  （manifest 挂载组件插件待做）。
- DSH 层仅 WIT 缝 + WASM echo-loop 验证；native 参考实现（tools/session/llm 服务）
  未移植（下一步）。

---

## 18. M8 补充交付记录（2026）—— WASM DSH 层闭环

**状态：补充交付**（`cargo test` 103 项全绿 + clippy 零警告）。

### 背景修正

上一轮 HANDOFF §7 曾写「DSH 层参考实现（native）」。经用户指正：B 方向的第一性原理
是「缝的权威契约 = WIT，loop 以 WASM 为第一公民；宿主只承载缝」。据此修正：**不做
native 参考插件**，把 `LoopHost` 提升为 dsh-wasmrt 正式宿主组件。

### 新增（`crates/dsh-wasmrt/src/loop.rs`）

- `LoopHost`：session/tools/llm 缝的 **Host 实现**（宿主职责，如同 WASI Host）——
  session `append`/`derive-messages`（含宿主侧模型历史投影：user/assistant/tool 消息
  序列）、tools `execute`（最小工具集：`add` 计算 a+b）/`register`、llm `generate`
  （回显）；含 WASI preview2 上下文。
- `WasmLoopPlugin`（适配 `Plugin`）：apply 懒实例化 dsh-loop 组件（三缝 Host + WASI
  注册），返回 disposer（卸载清理）；`run_turn`（驱动 WASM loop）、`event_kinds`/
  `derive_messages`（宿主读取 session 缝）。
- `load_wasm_loop_plugin`（宿主 API 入口）。

### 组件插件（`wasm-plugins/`）

- `echo-loop`（`dsh:echo-loop`）：run_turn 回显输入 + 写完整 turn/step 事件序列。
- `tool-loop`（`dsh:tool-loop`）：run_turn 调 `tools::execute("add", {a,b})` → 写
  tool/call + tool/result → assistant/message 引用结果。

### 新增测试（3 项，m8_dsh_loop.rs）

- `wasm_loop_mounts_as_plugin_and_runs_turn`：WASM loop 经 `plugin_arc` 挂进 Cordis
  （fiber Active）→ run_turn → session 事件序列 + 模型历史投影 → 卸载 Disposed。
- `wasm_loop_runs_multiple_turns`：多轮累计。
- `wasm_loop_calls_host_tool`：tools 缝双向桥接——WASM loop 调宿主 add 工具 → 结果
  5 回 session → 模型历史含 tool 消息。

### 结论

「loop 本身可替换」以 **WASM 形态闭环**：loop 驱动与工具编排全在插件层（echo-loop/
tool-loop），宿主只承载缝（LoopHost 的 session/tools/llm Host 实现）——与 B 方向
一致，无 native 参考插件。下一轮：缝的承载实质化（桥接 Cordis 服务仓库）。

---

## 19. M8 补充交付记录（2026）—— 缝的承载实质化

**状态：补充交付**（`cargo test` 108 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1）

`LoopHost` 的 session/tools 缝桥接 Cordis 服务仓库，使 WASM loop 的 session 输出
经 Cordis 可查、工具注册经 Cordis 可扩展——「WASM DSH 层」与「dsh-core 运行时」
深度整合。

### dsh-core 新增（`crates/dsh-core/`）

- `session.rs`：`SessionLog`（append-only 事件 + `derive_messages` 模型历史投影）、
  `SessionEvent`（seq/kind/payload）、`SessionHandle`/`new_session`。
- `tools.rs`：`ToolRegistry`（注册/执行/未注册错误）、`ToolRegistryHandle`/
  `new_tool_registry`。
- 句柄用 `Arc<Mutex<>>`（非 `Rc<RefCell<>>`）：服务仓库 `Impl.value: Arc<dyn Any +
  Send + Sync>` 要求 Send+Sync；运行时单线程，Mutex 仅满足类型约束。

### dsh-wasmrt（`crates/dsh-wasmrt/src/loop.rs`）

- `LoopHost` 桥接：`append_session` 优先写 `ctx.sessions`（`SessionLog`）、
  `execute_tool` 优先执行 `ctx.tools`（`ToolRegistry`），未提供时内存回退
  （appends + add 工具）；`derive_messages` 优先取 Cordis sessions 投影。
- `run_turn(ctx, input)`：显式注入当前 Cordis 至 `thread_local CURRENT_CTX`
  （Send 约束），调用 WASM loop 后清理。

### 新增测试

- `m9_session_tools.rs`（4，dsh-core）：SessionLog append/投影、tool/result 投影、
  ToolRegistry 注册/执行/未注册、句柄 Send+Sync 可作服务值。
- `m8_dsh_loop.rs` 扩展 `wasm_loop_seam_bridges_cordis_services`：宿主 provide
  sessions/tools 服务（含自定义 multiply + 覆盖 add 为 +100）→ WASM echo-loop 的
  session 事件/模型历史经 `ctx.sessions` 可读；WASM tool-loop 调 add 得宿主实现
  （2+3+100=105）→ tool/result 落入 `ctx.sessions`。

### 已知差异（M9 补齐）

- llm 缝未桥接 `ctx.llm`（LoopHost 回显）；完整 turn 流（pre-step → llm → 工具
  循环 → post-step）未组合；loader 未接入 PluginHost。

---

## 20. M8 补充交付记录（2026）—— 完整 turn 流（llm 缝桥接）

**状态：补充交付**（`cargo test` 112 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

桥接 llm 缝到 `ctx.llm`，组合完整 turn（pre-step → llm → 工具循环 → post-step），
使「模型调用 → 工具执行 → 会话记录」全链路可配置替换。

### dsh-core 新增（`crates/dsh-core/src/llm.rs`）

- `LlmService`：默认适配器 + 按 provider 适配器表 + `generate(provider, messages, tools)`
  → 助手响应 JSON；无适配器 → 错误 JSON。
- `LlmHandle`/`new_llm`（`Arc<Mutex<>>`，满足服务仓库 Send+Sync）。

### dsh-wasmrt（`crates/dsh-wasmrt/src/loop.rs`）

- `LoopHost` 的 llm Host 桥接 `ctx.llm`：`generate` 优先调 `LlmService`（解析
  messages/tools 为 JSON），未提供时内存回显。

### 组件插件（`wasm-plugins/llm-loop`）

- `llm-loop`（`dsh:llm-loop`）：**完整 turn 驱动**——pre-step（user/message）→
  llm 缝（模型返回 add 工具调用）→ tools 缝（宿主执行 add）→ 写 tool/call +
  tool/result → 再调 llm 缝（含工具结果，模型返回最终回答）→ assistant/message →
  step/end → turn/end。

### 新增测试

- `m8_dsh_loop.rs` 扩展 `wasm_loop_full_turn_with_llm`：宿主 provide sessions/tools/
  llm 三服务（llm 适配器：首轮返回 add 工具调用、含工具结果后返回 "sum is 5"）→
  WASM llm-loop 驱动完整 turn → session 服务含完整事件序列
  （user → tool/call → tool/result → assistant）+ 模型历史
  （user → tool{sum:5} → assistant"sum is 5"）。
- `m9_session_tools.rs` 扩展 LlmService（3）：默认/provider 适配器、未知 provider
  回退默认、无适配器错误、句柄 Send+Sync。

### 结论

三缝（session/tools/llm）承载全部桥接 Cordis 服务，WASM loop 插件驱动完整 turn：
「模型调用 → 工具执行 → 会话记录」全链路在插件层，宿主只承载缝——与 B 方向一致。
下一轮：DSH 层配置化组装（loader 挂载 WASM loop + 服务）。

---

## 21. M9 交付记录（2026）—— DSH 层配置化组装

**状态：M9 已交付**（`cargo test` 116 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

经 `dsh-loader` 从配置挂载服务插件 + WASM loop 插件——「loop 可替换」从代码级
验证升级为**配置级验证**（换 entry `name` 即换 loop 行为，宿主不改代码）。

### dsh-wasmrt 新增（`crates/dsh-wasmrt/src/services.rs`）

- `DshServicesPlugin`（适配 `Plugin`）：apply 时按配置 `config.services`（默认全
  注册）provide `sessions`/`tools`/`llm` 服务（`SessionLog`/`ToolRegistry`/
  `LlmService` 句柄）。
- `DshServicesConfig`（配置类型）。

### 配置化组装（`crates/dsh-wasmrt/tests/m9_loader_assemble.rs`，4 项）

- `loader_assemble_echo_loop`：`dsh:services` entry + `echo-loop` entry → run_turn
  正常 + session 记录 6 事件。
- `loader_assemble_tool_loop`：换 loop entry 为 `tool-loop` → 经 tools 缝调宿主
  add 工具（2+3=5）+ tool/result。
- `loader_assemble_llm_loop`：换为 `llm-loop` → 完整 turn（user → tool/call →
  tool/result → assistant）+ 模型历史完整。
- `loader_assemble_services_subset`：`config.services: ["sessions"]` → loop 仍可跑
  （tools/llm 缝回退内存）。

### 结论

「loop 可替换」的配置级形态成立：`dsh:services`（缝的承载）+ WASM loop 插件
（缝的消费）经 loader entry 组装，换 entry `name` 即换 loop 行为——对应
deepseek-harness cordis.yml 的 agent-loop 行。下一轮：YAML 配置端到端
（Include 从 cordis.yml 形态挂载）。

---

## 22. M9 补充交付记录（2026）—— YAML 配置端到端

**状态：补充交付**（`cargo test` 120 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

用 `dsh-loader` 的 Include（YAML）从 **cordis.yml 形态**的配置挂载服务 + loop
插件——贴近真实 dsh 启动方式（配置驱动、宿主不改代码）。

### 测试（`crates/dsh-wasmrt/tests/m9_yaml_assemble.rs`，4 项）

- `yaml_assemble_echo_loop`：写 cordis.yml 形态 YAML（services + echo-loop entries）
  → `Include::load` → run_turn 正常 + session 记录 6 事件。
- `yaml_assemble_tool_loop`：换 YAML 的 loop name 为 tool-loop → 经 tools 缝调
  宿主 add 工具（2+3=5）+ tool/result。
- `yaml_assemble_llm_loop`：换为 llm-loop → 完整 turn（user → tool/call →
  tool/result → assistant）。
- `yaml_patch_overrides_loop_config`：patch 覆盖 loop entry 的 config → 仍正常
  挂载（patch 机制在 DSH 组装中生效）。

### 结论

DSH 层组装已具备 cordis.yml 形态的配置驱动：Include 读 YAML → loader 按名挂载
服务 + WASM loop——换 YAML 的 loop name 即换 loop 行为。对应 deepseek-harness
的 bundle/patch 组装。下一轮：DSH 层启动器（app-boot 等价，可执行入口）。

---

## 23. M9 补充交付记录（2026）—— DSH 层启动器（app-boot 等价）

**状态：补充交付**（`cargo test` 122 项全绿 + clippy 零警告；9 差分场景不变；
CLI 端到端实跑通过）。

### 目标（HANDOFF §7 方向 1 续）

可执行入口：读 cordis.yml → 注册插件仓库（native 服务 + WASM loop manifest）→
Include 挂载 → 一次性 run_turn——对应 deepseek-harness 的 `dsh` CLI 最小形态。

### 新建 `crates/dsh-cli`（bin `dsh`）

- `src/lib.rs`：`boot(config_path, wasm_base)`——读 YAML 入口列表 → 注册
  `dsh:services` + 每个非 services entry 按 `config.wasm`（组件目录）构建
  `WasmLoopPlugin` → `Include::load` → 返回 `Boot{ctx, loop_plugin, sessions}`；
  `run_turn(boot, input)` 驱动 WASM loop。缺 loop entry 报错（fail loud）。
- `src/main.rs`：`dsh <cordis.yml> [wasm-base]`，stdin 读一行 JSON → run_turn →
  打印响应。
- `WasmLoopPlugin::new_owned`（运行时名字，Box::leak 换 `&'static str`）。

### 新增测试

- `crates/dsh-cli/tests/m9_boot.rs`（2）：boot 端到端（cordis.yml → echo-loop
  run_turn → session 6 事件）；缺 loop entry 报错。
- CLI 实跑：`echo '{"content":"hello from cli"}' | dsh cordis.yml wasm-plugins`
  → `{"echo": "echo: hello from cli", "reason": "completed"}`。

### 结论

DSH 层具备可执行启动器：`dsh` CLI 从 cordis.yml 配置驱动 WASM loop（配置驱动、
宿主不改代码）——app-boot 的最小形态。下一轮：交互式多轮 + profile 叠加层。

---

## 24. M9 补充交付记录（2026）—— 交互式运行（多轮 + profile 叠加 + manifest）

**状态：补充交付**（`cargo test` 125 项全绿 + clippy 零警告；9 差分场景不变；
CLI 多轮实跑通过）。

### 目标（HANDOFF §7 方向 1 续）

`dsh` CLI 支持多轮（stdin 循环）、`--overlay` profile 叠加层（bundle 语义）、
loop manifest（`config.wasm` 目录或 `.wasm` 路径）。

### dsh-cli（`crates/dsh-cli/`）

- `boot(config_path, overlays, wasm_base)`：
  - 读主配置 + 各 overlay（YAML 入口列表），`merge_entries` 同 id 覆盖
    （bundle/patch 语义）；
  - `config.wasm` 两种形态：目录名（构建目录）或 `.wasm` 路径（相对/绝对）；
  - 合并后写唯一临时 YAML 供 Include 挂载。
- `main.rs`：`dsh <cordis.yml> [--overlay <file>]... [--wasm-base <dir>]`，stdin
  逐行 JSON → run_turn → 打印响应（多轮会话）。

### 新增测试（`m9_boot.rs` 5 项）

- `boot_loads_and_runs_turn`：端到端。
- `boot_runs_multiple_turns`：同一 boot 连续 run_turn，session 累计 12 事件。
- `boot_profile_overlay_swaps_loop`：overlay 把 loop 从 echo-loop 换成 tool-loop
  （bundle 语义）。
- `boot_manifest_wasm_path`：`config.wasm` 指向 `.wasm` 文件路径。
- `boot_requires_loop_entry`：缺 loop 报错（fail loud）。

### 结论

`dsh` CLI 具备交互式多轮运行：cordis.yml + overlay 叠加 → 插件仓库 → Include →
逐行 run_turn——对应 deepseek-harness 的 profile/bundle 组装与 `dsh` CLI。
下一轮：完整 loop 语义（多轮共享上下文、工具/llm 配置化）。

---

## 25. M9 补充交付记录（2026）—— 完整 loop 语义（声明式配置 + 多轮上下文）

**状态：补充交付**（`cargo test` 127 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

工具/llm 配置化（cordis.yml 声明，非代码注册）+ 多轮会话共享上下文
（session 历史进 llm 缝输入）。

### dsh-wasmrt（`crates/dsh-wasmrt/src/services.rs`）

- `DshServicesPlugin` 声明式配置：
  - `config.tools: [{name, op}]`——op ∈ add/multiply/echo，按配置注册（不再代码
    注册）；
  - `config.llm: {provider, behavior}`——behavior ∈ tool-first/echo，注册为
    **默认适配器**（loop 的 llm 缝不带 provider 参数，走 default）。
- `wasm-plugins/llm-loop`：run_turn 从 session 缝 `derive-messages` 取历史
  （前轮 user/assistant/tool 消息）作为 llm 缝输入——多轮共享上下文在插件层。

### 新增测试（`m9_boot.rs` 2 项）

- `boot_declared_tools_and_llm`：cordis.yml 声明 add 工具 + tool-first llm →
  tool-loop 经 tools 缝调声明式 add（2+3=5），无需代码注册。
- `boot_multi_turn_shared_context`：llm-loop 两轮——第一轮 turn=1；第二轮 llm
  缝输入含前轮历史（tool-first 回答 `sum is 5 (ctx=N)`），session 服务含两轮
  16 事件。

### 结论

完整 loop 语义落地：声明式工具/llm 配置（cordis.yml 驱动）+ 多轮共享上下文
（session 历史投影进 llm 缝）——DSH 层 turn 流可配置、可替换、带记忆。
下一轮：组件模型完善（host get bytes 版、WASI 精细授予、loader 接入
PluginHost）。

---

## 26. M10 交付记录（2026）—— 组件模型完善（WASI 精细授予 + llm provider）

**状态：M10 已交付**（`cargo test` 129 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

WASI preview2 精细授予（按 caps 而非全量）+ llm 缝 provider 参数（按 provider
选适配器）。

### dsh-wasmrt（`crates/dsh-wasmrt/`）

- `abi.rs`：`Capabilities` 增加 WASI 位——`CAPS_WASI_ENV`/`CAPS_WASI_FS`/
  `CAPS_WASI_NET`；`build_wasi_ctx()` 按位构建 `WasiCtxBuilder`（env 继承、
  fs 预打开根目录只读、net 继承 + TCP/UDP/域名解析）；`abi_only()` 无 WASI。
- `loop.rs`/`component.rs`：`LoopHost`/`ComponentHostState` 用 `caps.build_wasi_ctx()`
  （不再全量默认）。
- WIT `dsh-loop.wit`：llm 缝 `generate(provider, messages, tools)`；`LoopHost`
  llm Host 桥接按 provider 选 `LlmService` 适配器（空 → default）。

### 组件重建 + 测试（`m9_boot.rs` 9 项）

- `boot_works_without_wasi_caps`：`Capabilities::abi_only()`（无 WASI）loop 仍跑
  （组件不依赖 WASI 功能）。
- `llm_provider_selection`：多 provider（tool-first/echo）按名选择；未知 provider
  回退 error。
- 既有 7 项（多轮/overlay/manifest/声明式/上下文）全绿——WIT 变更向后兼容。

### 结论

组件模型路径具备**能力精细授予**（ABI 位 + WASI preview2 位按插件配置）与
**provider 路由**（llm 缝带 provider，适配器按名选择）。下一轮：host `get`
bytes 版 + loader 接入 PluginHost。

---

## 27. M10 补充交付记录（2026）—— host get bytes 版 + PluginHost 统一加载

**状态：补充交付**（`cargo test` 132 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 续）

host `get` 接口的 bytes 版（组件模型下返回值经 WIT `list<u8>`，无需线性内存
句柄）+ `PluginHost` 统一加载组件插件（manifest 形态）。

### dsh-wasmrt（`crates/dsh-wasmrt/`）

- WIT `wit/plugin.wit`：host-api `get(service) -> list<u8>`（bytes 版，去掉
  out-ptr/out-len-ptr 的 C ABI 形态）。
- `component.rs`：`ComponentHostState::get` 返回服务值 JSON 字节（能力位检查，
  被拒/缺失返回空）。
- `host.rs`：`PluginKind::ComponentBytes(Vec<u8>)` + `NativeHost` 统一分派
  （native / WasmBytes C ABI / ComponentBytes 组件）。
- `wasm-plugins/hello-component`：handle_event 经 `host.get("greeting")` 回读服务
  （bytes 版），emit 载荷含回读值。

### 新增测试（`m10_plugin_host.rs`，3 项）

- `plugin_host_loads_component`：PluginHost（ComponentBytes manifest）加载组件 →
  Plugin trait 可用 + 提供服务。
- `component_provides_and_rolls_back`：服务提供 + 卸载回滚。
- `component_host_get_bytes_roundtrip`：ping → 组件内 host.get 回读 greeting →
  emit 载荷含回读值（bytes 版双向）。

### 结论

组件模型路径补全：host `get` bytes 版（组件可回读服务）+ PluginHost 统一加载
三形态插件（native / C ABI / 组件）。下一轮：async 收尾 / WASI 能力按 entry
配置 / 真实 llm 接入。

---

## 28. M10 补充交付记录（2026）—— 能力按 entry 配置（界面驱动授权到配置层）

**状态：补充交付**（`cargo test` 134 项全绿 + clippy 零警告；9 差分场景不变；
CLI 受限 caps 实跑通过）。

### 目标（HANDOFF §7 方向 1 续）

cordis.yml 的 loop entry 声明 `caps`（能力位数组），启动器按配置授予——界面
驱动授权落地到配置层（对应 WASI preview2 的按实例授予）。

### dsh-wasmrt（`crates/dsh-wasmrt/src/abi.rs`）

- `Capabilities::from_json(Option<&Value>)`：解析 `caps` 名称数组
  （provide/emit/get/wasi-env/wasi-fs/wasi-net；`all`；缺省/空 → `abi_only`）。

### dsh-cli（`crates/dsh-cli/src/lib.rs`）

- `boot` 的 loop entry：`Capabilities::from_json(config.get("caps"))` 授予
  （此前硬编码 `Capabilities::all()`）。

### 新增测试（2 项）

- `m10_plugin_host.rs` `capabilities_from_json`：缺省 abi_only、指定位、all。
- `m9_boot.rs` `boot_caps_from_entry_config`：`caps: [provide, emit, get]` 的
  loop entry 正常 run_turn。

### 结论

能力授予从硬编码变为**配置驱动**：cordis.yml 的 `caps` 字段控制插件能力
（ABI + WASI 位），启动器按配置构建 WASI 上下文——界面驱动授权的配置层落地。
下一轮：async 收尾 / 真实 llm 接入。

## 29. M13 交付记录（2026）—— async 剩余收尾（emit fire-and-forget async listener + spawn 钩子）

**状态：补充交付**（`cargo test` 135 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1）

async 剩余收尾第一半：`emit` 对 async listener 的 fire-and-forget。Cordis 语义中
`ctx.emit` 同步返回、async listener 在宿主事件循环上异步执行；Rust 侧同步宿主无
事件循环，故经「spawn 钩子」把异步任务交给宿主驱动——无钩子时跳过并记 trace。

### dsh-core（`crates/dsh-core/src/runtime.rs` / `context.rs`）

- `Runtime.spawn: Option<Box<dyn Fn(LocalBoxFuture<'static, ()>)>>` 字段（宿主
  注入的任务驱动钩子；默认 None）。
- `Cordis::set_spawn`：注入钩子（同步宿主可空转，async 宿主如 tokio LocalSet
  用它 spawn_local 驱动）。
- `Cordis::fire_async_listener`：`emit` 分派遇 `HookCallback::Async` 时把
  `listener(&ctx, args)` 包成 `LocalBoxFuture` 交给钩子（fire-and-forget）；
  无钩子则 `trace_push("async-listener-skipped")` 并跳过。

### 新增测试（1 项）

- `m7_async.rs` `emit_fire_and_forgets_async_listener`：宿主注入 tokio
  LocalSet spawn 钩子，`emit` 同步返回后经 `run_until` + 多次 yield 驱动，
  async listener 已执行（log 出现 "async-fired"）。

### 结论

`emit` 的异步监听器语义闭环：同步宿主可安全跳过（trace 可见）、async 宿主可
经钩子驱动，行为对齐 Cordis 的 fire-and-forget。剩余：loader 事务 allSettled
（同步收尾另一半）。

## 30. M14 交付记录（2026）—— async 剩余收尾（loader 事务 allSettled）

**状态：补充交付**（`cargo test` 142 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1 另一半）

loader 事务 allSettled：复刻 Cordis `EntryGroup.update(config)` 事务语义——
全部入口都尝试 create/update（一个失败不阻断其他）、错误聚合（1 个失败 = 原
错误；多个 = AggregateError）、任一失败则**整事务回滚**（逆序移除新建 + 重建
旧配置），回滚错误并入 AggregateError。语义从 vendored
`@deepseek-ai/cordis-plugin-loader` 逐行确认。

### dsh-loader（`crates/dsh-loader/src/loader.rs`）

- `Loader::create_async` / `update_async` / `remove_async`：async 生命周期变体
  （`plugin_arc_async` / `unload_async`），四分支事务与同步版一致。
- `Loader::sync_async`：**allSettled 事务**——重复 id 校验 → 全部入口
  create/update（收集每个结果，不中断）→ 全成功才移除缺席旧入口 → 任一失败
  回滚（逆序移除新建 + 重建旧配置）→ `AggregateError`。
- async 内部生命周期避免 async 递归（编译器 E0733）：
  - `start_entry_async`：显式栈迭代（组结构先挂载、子入口按序启动），中途
    失败逆序清理已启动入口（`rollback_started`）。
  - `dispose_entry_async`：后序收集子树 + 逐个 `unload_async`（子先父后）。
  - `update_async`：队列迭代（group 子入口「更新既有」入队循环处理）。
- `Include::load_async` / `refresh_async`：配置装载走 async 事务（Cordis
  Include 插件的 `internal/update → EntryGroup.update(config)` 路径）。

### 新增测试（7 项，`m14_loader_async.rs`）

1. `sync_async_partial_failure_keeps_others`：create 阶段 allSettled（e1/e3 都
   apply，不阻断）→ 整事务回滚（新建全部移除）；单失败 = 原错误。
2. `sync_async_multiple_failures_aggregate`：多失败聚合（2 个都保留）。
3. `sync_async_rollback_restores_old_config`：旧配置在运行 → sync 含失败 →
   新建移除 + e1 恢复旧配置。
4. `sync_async_success_removes_absent`：全成功（热更既有 + 新增 + 移除缺席）。
5. `async_entry_lifecycle`：create/update/remove_async 基本路径。
6. `sync_async_duplicate_id_fails`：重复 id 报错。
7. `include_load_async_transaction`：YAML → async 事务装载；部分失败回滚；
   修复后重载成功。

### 结论

async 剩余收尾闭环：`emit` fire-and-forget（M13）+ loader 事务 allSettled
（M14）。loader 侧 Cordis `EntryGroup.update(config)` 语义完整落地，剩余仅为
同步变体（`serial`/`parallel` 不调 async listener、同步 `unload` 跳过 async
disposer）的既有差异。下一轮：真实 llm 接入 / HMR / loader 差分集。

## 31. M15 交付记录（2026）—— HMR 热重载（文件 watcher + refresh 自动化）

**状态：补充交付**（`cargo test` 147 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 3）

HMR 热重载：对应 Cordis `cordis-plugin-hmr` 的 `registerConfig(filename, refresh)`
——监听 add/change/unlink → refresh 串行执行；失败 emit `hmr/error` 事件。
Rust 侧用**内容指纹轮询**（无 chokidar 依赖、无后台线程，符合单线程纪律）。

### dsh-loader（`crates/dsh-loader/src/hmr.rs`）

- `Hmr::register_config(path, refresh)`：注册监视（立即建快照——首次 `poll`
  不触发，对应 chokidar `ready`）；`unregister` 取消。
- `Hmr::poll() -> Vec<String>`：内容指纹（存在性 + std hash）检测变化 →
  **串行**调用 refresh；返回本次触发路径列表。
- `Hmr::take_errors() -> Vec<(String, CordisError)>`：refresh 失败记录
  （对应 Cordis `hmr/error` 事件；Rust 侧查询式）。
- `Include` 增加 `#[derive(Clone)]`（回调捕获用）。

### dsh-cli（`crates/dsh-cli/src/lib.rs` / `main.rs`）

- `Boot` 新增 `refresh: Rc<dyn Fn() -> Result<(), CordisError>>`：重读主配置 +
  overlays → 重新挂载（`Include::load`；async 宿主可用 M14 `load_async`）。
- `dsh --watch`：监视主配置 + overlays（`Hmr` 注册 → `boot.refresh`）；stdin
  后台线程（mpsc）逐行发主循环，主循环 select stdin + HMR 轮询（50ms）。

### 新增测试（5 项）

- `m15_hmr.rs`（4）：首 poll 不触发 + 内容变化触发；删除/重建（unlink/add）
  触发；多文件独立检测 + 失败记录；Include 集成热更（config k=1 → k=2）。
- `m9_boot.rs` `boot_refresh_hot_reloads_llm_behavior`（1）：端到端——修改
  cordis.yml 的 llm behavior（echo → tool-first）→ `boot.refresh()` → 新 turn
  走新适配器（回答从回显变 `ctx=N`）。

### 结论

HMR 热重载闭环：配置变化 → `poll` 检测 → refresh 重挂载 → 运行中生效
（llm 行为热更端到端验证）。剩余差异：事件驱动 vs 轮询（行为等价、
实现无依赖）；`boot.refresh` 为同步路径（async 事务供 async 宿主）。
下一轮：真实 llm 接入 / loader 差分集 / C ABI caps。

## 32. M16 交付记录（2026）—— 能力按 entry 配置统一入口（C ABI + 组件）

**状态：补充交付**（`cargo test` 151 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 3）

C ABI 路径能力配置：此前仅组件路径（boot 的 loop entry）经
`Capabilities::from_json(config.caps)` 接入配置；C ABI 路径（`WasmPlugin`）
调用点全部硬编码 caps。统一为按 entry 配置解析的入口，两路径一致。

### dsh-wasmrt（`crates/dsh-wasmrt/src/host.rs`）

- `PluginManifest::from_config(name, kind, config)`：从 entry 配置构造清单——
  `config.caps` 数组 → `Capabilities::from_json`（缺省 abi_only / `all` 全量）。
  C ABI（`WasmBytes`）与组件（`ComponentBytes`）共用；native 直通（caps 无
  host 侧检查）。

### 新增测试（4 项，`m16_caps_config.rs`）

1. `manifest_from_config_defaults_abi_only`：缺省 = ABI 能力（无 WASI）。
2. `manifest_from_config_parses_caps_array`：位名映射（含 wasi-env/wasi-net）。
3. `c_abi_caps_from_entry_config_enforced`：`caps: [provide]` → apply 成功
   （provide 允许）、事件处理中 host_get 被拒（`host_get denied` 入插件日志）。
4. `component_caps_from_entry_config_enforced`：`caps: [emit, get]`（无
   provide）→ 组件 apply 提供服务被拒 → fiber Failed。

### 结论

能力授予的配置驱动在两条 WASM 路径闭环：`PluginManifest::from_config` 是统一
入口（宿主/loader 组装任意形态插件时按 entry 配置授权），host import 侧检查
生效并记录拒绝。剩余：C ABI 路径无 WASI 上下文（仅 ABI 位生效，WASI 位待接入
`WasmPlugin`）。下一轮：真实 llm 接入 / loader 差分集 / HMR 完善。

## 33. M17 交付记录（2026）—— 真实 HTTP llm 接入

**状态：补充交付**（`cargo test` 157 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 1）

真实 HTTP llm 适配器（非回显）替换声明式 mock：OpenAI 兼容 `/chat/completions`。
无真实 API key / 外网依赖——用**本地 TCP mock 服务器**验证完整语义（请求形状、
响应解析、错误路径），真实端点（https）仅需换 base URL。

### dsh-core（`crates/dsh-core/src/llm_http.rs`）

- `chat_completions(base, api_key, model, messages, tools)`：手写 HTTP/1.1
  （`std::net::TcpStream`，零外部依赖；单线程纪律）——POST `{base}/chat/completions`，
  body `{model, messages, tools}`，Bearer 认证可选；解析 `choices[0].message`
  → `{content, tool_calls?}`；非 2xx / 形状不符 / 连接失败 → error JSON（fail
  loud，不 panic）。Content-Length 提前截断读取。
- `LlmService::register_http(provider, base, api_key, model)` /
  `register_http_default(...)`：HTTP 适配器注册（默认或按 provider）。

### dsh-wasmrt（`crates/dsh-wasmrt/src/services.rs`）

- 声明式 `llm: {provider, http: {base, api_key, model}}`：`DshServicesPlugin`
  按配置注册 HTTP 适配器（provider == "default" → 默认，否则按名）。

### 新增测试（6 项）

- `m17_http_llm.rs`（5，dsh-core）：mock 服务器请求形状（POST 路径 / Bearer /
  model / messages）+ 响应解析；provider 作默认；非 2xx → error 含状态码；
  连接拒绝 → error；响应无 choices → error。
- `m9_yaml_assemble.rs` `yaml_declared_http_llm`（1，dsh-wasmrt）：YAML 声明
  `llm.http` → llm-loop 完整 turn 经真实 HTTP（mock）→ 回答来自 HTTP 响应；
  两步模型请求（step1+step2）各含 Bearer + model。

### 结论

真实模型接入的最小契约落地：声明式配置 → HTTP 请求 → 响应解析 → turn 流。
剩余：https（TLS）待扩展（当前仅 http://；可后续 native-tls/rustls 或 ureq）。
下一轮：loader 差分集 / HMR 完善 / C ABI WASI。

## 34. M18 交付记录（2026）—— 同步分派对 async listener fire-and-forget 补全

**状态：补充交付**（`cargo test` 159 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 3 之一）

M13 已实现 `emit` 对 async listener fire-and-forget；`bail`/`serial`（同步
`run_serialish`）与 `waterfall`（同步 `run_chain`）仍**跳过** async listener
（记录为差异）。Cordis 语义：同步分派 `Reflect.apply` 直接调用 async listener
（返回 Promise 被丢弃，不 await），bail 值不可同步判定 → 链继续。补全三处。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `run_serialish`（`bail`/`serial`）：`HookCallback::Async` → `fire_async_listener`
  （不再 `continue` 跳过）。
- `run_chain`（`waterfall`）：`HookCallback::Async` → `fire_async_listener` +
  继续 next 链（inner 仍执行）。
- 无 spawn 钩子（同步宿主）时仍跳过并 trace "async-listener-skipped"（同 M13）。

### 新增测试（2 项，`m7_async.rs`）

1. `bail_and_serial_fire_and_forget_async_listener`：bail + serial 各触发一次
   async listener（副作用执行）；async 返回值不参与 bail 判定。
2. `waterfall_fire_and_forgets_async_listener`：waterfall 中 async listener
   被调用（副作用执行）、inner 同步返回（链继续）。

### 结论

同步分派（emit/bail/serial/waterfall）对 async listener 的语义全部对齐
Cordis（调用但不 await）。剩余 async 差异仅：同步 `unload` 跳过 async
disposer（需 `unload_async`）。下一轮：真实模型 https / loader 差分集 /
HMR 完善。

## 35. M19 交付记录（2026）—— C ABI 路径 WASI 能力授予（preview1）

**状态：补充交付**（`cargo test` 162 项全绿 + clippy 零警告；9 差分场景不变）。

### 目标（HANDOFF §7 方向 4）

C ABI 路径（`WasmPlugin`）此前只有 ABI 能力位（provide/emit/get）生效，WASI
位（env/fs/net）无上下文。接入 WASI **preview1**（core-module 形态）——wasip1
构建的 C ABI 插件按 caps 注入环境变量/文件系统/网络。

### 关键技术约束

`wasmtime_wasi::preview1::add_to_linker_sync<T: Send>` 要求 store data 是
Send；`WasmHostState` 此前含非 Send 的 `Option<Cordis>`。改造为**组件路径同款
thread_local 桥接**（`CURRENT_CTX` + `mounted`），`WasmHostState` 变 Send。

### dsh-wasmrt

- `abi.rs`：`Capabilities::build_wasi_p1_ctx()`——按位构建 `WasiP1Ctx`
  （`build_p1`）；**无任何 WASI 位 → None**（不注册，wasip1 插件 import 解析
  失败 = 能力拒绝）。
- `plugin.rs`：`WasmHostState` 移出 `ctx`（thread_local `CURRENT_CTX` +
  `mounted: Cell<bool>`），新增 `wasi: Option<WasiP1Ctx>`；instantiate 时有
  WASI ctx 则 `preview1::add_to_linker_sync` 注册；host import 闭包改经
  `ctx()`（mounted + CURRENT_CTX）。
- `wasm-plugins/hello-wasi`（新）：wasm32-wasip1 C ABI 插件，`plugin_apply`
  读环境变量 `DSH_TEST` → `host_log`。

### 新增测试（3 项，`m19_wasi_cabi.rs`）

1. `wasi_env_cap_allows_env_read`：`caps: [provide, wasi-env]` → 插件读到
   `DSH_TEST`（host_log 含值）。
2. `no_wasi_cap_fails_instantiation`：`abi_only` → apply（懒实例化）时 wasi
   import 无法解析 → fiber Failed（能力拒绝）。
3. `wasi_fs_cap_builds_ctx_for_env_plugin`：纯 env ABI 插件（hello）+ WASI 位
   注册无碍（实例化成功、服务正常）。

### 结论

C ABI 路径 WASI 精细授予闭环：wasip1 插件按 caps 注入（env 已验证端到端；
fs/net 位同构构建，待端到端验证）。两条 WASM 路径（C ABI preview1 / 组件
preview2）能力授予一致。下一轮：真实模型 https / loader 差分集 / HMR 完善。

## 36. M20 交付记录（2026）—— loader 场景纳入差分集

**状态：补充交付**（`cargo test` 162 项全绿 + clippy 零警告；**11 个差分场景**
全部逐行一致——9 核心 + 2 loader 事务）。

### 目标（HANDOFF §7 方向 2）

loader/include 场景纳入差分集。TS 参照改用 **vendored
`@deepseek-ai/cordis-plugin-loader`**（DSH 实际使用的 4.0.1+1.0.2 配对；无需
npm 装 `@koishijs/loader`）——`diff/ts-host/loader-host.mjs`。

### 关键发现：真并行 create

Rust `Loader::sync_async` 原为**串行** create（PLAN §10 记录「并行降为顺序」），
与 TS `Promise.allSettled(config.map(create))` 的 plugin/status/apply **交错
顺序**不一致（差分逐行比对暴露）。改为 **join_all 真并行**：
- `futures-util` 加入 dsh-loader 依赖；`sync_async` 的 allSettled 循环改
  `join_all`（`LocalBoxFuture` 显式类型）。
- 竞争分析：`pending_entry`/`pending_isolate`/`pending_intercept` 在
  `register_plugin` 的**同步段**被 take（不跨 await），并行 create 各自
  register 时无覆盖竞争。
- 回滚路径保持串行（逆序移除新建 + 重建旧配置），语义不变。

### dsh-diff

- `Step` 增加 `LoaderSync`/`LoaderCreate`/`LoaderUpdate`/`LoaderRemove`
  （serde kebab-case：`loader-sync` 等）；`LoaderEntry = serde_json::Value`
  （保序——TS 侧 canonical 排序对齐 Rust BTreeMap 默认键序）。
- `Runner` 持有懒初始化 `Loader`（`ensure_loader`：挂载 Loader 插件 + 注册场景
  插件；挂载产生的 `plugin:loader` trace 丢弃——TS 在框架监听前挂载）。
- loader 步骤必须走 `--async`（同步路径报错提示）；错误 trace 只输出**失败
  数量**（两边错误消息文本不同，数量语义一致）。
- `verify-diff.mjs`：loader 场景用 `loader-host.mjs` 生成 golden + `--async`
  校验；golden 去 BOM。

### 新增场景（2 个）

1. `loader-01-sync-success`：sync 两入口（并行交错）→ update 热更 → remove。
2. `loader-02-partial-failure-rollback`：sync 三入口含未知插件 → 部分失败
   整事务回滚（新建移除 + 旧配置重建）+ `loader-error:1` → 恢复 sync。

### 结论

loader 事务语义（allSettled 并行交错 + 失败回滚）与 TS **逐行一致**（20/26
行 PASS）。剩余差异：group 入口 Rust 无 Group 插件 fiber 形态（TS 有
`plugin:Group`/`status:Group`）——group 场景待实现后纳入。下一轮：loader
group 对齐 / 真实模型 https / HMR 完善。

## 37. M21 交付记录（2026）—— C ABI 路径 WASI fs 授予端到端验证

**状态：补充交付**（`cargo test` 164 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §7 方向 4 之一）

M19 已实现 C ABI 路径 WASI preview1 授予（env 端到端验证）；fs 位同构构建但
未验证。补充 fs 端到端：wasip1 C ABI 插件读取预打开根目录文件。

### 实现

- `wasm-plugins/hello-wasi/src/lib.rs`：`plugin_apply` 读环境变量（`M19_ENV_A`/
  `M19_ENV_B`，并行测试互不覆盖）+ 读根目录 `/dsh_fs_test.txt` → `host_log`。
- `m19_wasi_cabi.rs` 新增 2 项：
  1. `wasi_fs_cap_allows_file_read`：`caps: [provide, wasi-fs]` → 插件读到文件
     （`FS_READ=fs-cap-ok`）。
  2. `no_wasi_fs_cap_denies_file_read`：`caps: [provide, wasi-env]`（无 fs）→
     `FS_READ=<fs-error: ...>`（未预打开根目录）。
- 修复：env 测试用唯一变量名（`M19_ENV_A`/`M19_ENV_B`）避免并行 `set_var`
  覆盖（原 `DSH_TEST` 被并行测试互相覆盖导致断言失败）。

### 结论

C ABI 路径 WASI 精细授予闭环：env + fs 位端到端验证（授予可读、拒绝报错）。
剩余：`net` 位同构构建待验证。下一轮：loader group 对齐 / 真实模型 https /
HMR 完善。

## 38. M22 交付记录（2026）—— Group 插件 fiber 形态

**状态：补充交付**（`cargo test` 165 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §7 方向 1）

loader 差分暴露：TS 的 group 入口是真实插件（`Group extends EntryGroup`，
`plugin:Group`/`status:Group` trace，子入口 parent=Group），Rust 此前直接挂
子组（无 fiber）。实现 Group 插件形态。

### dsh-loader（`crates/dsh-loader/src/loader.rs`）

- `GroupPlugin`（`name="Group"`）：apply 解析 config（子入口数组）→ 逐个
  `insert_child` + `start_entry`（在 Group fiber 的 apply 期间注册 → 子入口
  parent 自动 = Group fiber）；注册 stop disposer（卸载时递归 `dispose_entry`
  子入口）。
- `start_entry`/`start_entry_async` 的 group 分支：改为 `load_group_plugin`/
  `load_group_plugin_async`（注册 GroupPlugin，`pending_entry` 关联 entry）。
- `dispose_entry`/`dispose_entry_async`：group 入口经 Group fiber 卸载
  （disposer 递归 stop 子入口），移除原手动递归 + `collect_subtree_postorder`；
  group 结构（subgroup/groups/group_owner）在卸载后清理。
- `sync_children`/group 分支（async 内联）：顺序修正为「先更新/新建、后移除
  缺席」——对齐 Cordis `EntryGroup.update(config)`（allSettled create 全部 →
  全成功后移除缺席）。

### 新增测试（1 项，`m2_loader.rs`）

- `group_plugin_fiber_mounts_children`：group 入口有 Group fiber；子入口 parent
  = Group fiber；卸载 → Group fiber + 子入口全部 Disposed。

### 结论

group 入口的 Group 插件 fiber 形态落地（`plugin:Group`、子入口 parent 链、
卸载递归 stop）。剩余差异：Rust `Plugin::apply` 同步契约 vs TS `[Service.init]`
async generator——Group apply 同步完成，Active 时序与子入口热更交错未逐行
一致，group 差分场景暂未纳入（m2 单测覆盖）。下一轮：Group apply 异步化 /
真实模型 https / HMR 完善。

## 39. M23 交付记录（2026）—— HMR 完善（`boot.refresh` async 事务）

**状态：补充交付**（`cargo test` 166 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §7 方向 3）

M14 已交付 `Include::load_async`（`sync_async` allSettled + 整事务回滚），
但 `boot.refresh` 仍用同步 `Include::load`（fail-fast）。接入 async 事务。

### dsh-cli（`crates/dsh-cli/src/lib.rs` / `Cargo.toml`）

- `tokio`（rt + macros）加入正式依赖。
- `Boot.refresh` 改为：重读配置 + overlays → `Include::new` → `load_async`
  经 current_thread runtime `block_on` 驱动；`AggregateError` → 错误消息含
  失败数量（`hmr refresh failed (N errors)`）。

### 新增测试（1 项，`m9_boot.rs`）

- `boot_refresh_async_transaction_reports_failure`：配置含未注册插件 →
  refresh 报错（含 errors 数量）不 panic；恢复配置后 refresh 成功、turn 正常。

### 结论

HMR 热重载的 loader 事务语义对齐：`boot.refresh` 走 allSettled（一个失败不
阻断其他、失败回滚），与 Cordis Include 的 `internal/update → EntryGroup.update`
路径一致。剩余：文件 watcher 事件驱动（当前轮询）。下一轮：Group apply
异步化 / 真实模型 https / C ABI WASI net。

## 40. M24 交付记录（2026）—— 同步 unload 的 async disposer 显式记录

**状态：补充交付**（`cargo test` 167 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（async 剩余差异收尾）

`run_unload`（同步）此前静默丢弃 `async_disposers`（`EffectOutcome::Async` 的
future）。同步 `unload` 无法 await（Cordis 的 `fiber.dispose()` 总是 async），
但不应无痕——显式记录跳过，完整异步清理由 `unload_async` 提供。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `run_unload`：卸载时检查 `fiber.async_disposers` 非空 → trace
  `async-disposers-skipped`（不静默丢弃）；同步 disposer 照常逆序执行。

### 新增测试（1 项，`m7_async.rs`）

- `sync_unload_skips_async_disposer_with_trace`：同步 `unload` 后异步 disposer
  未执行但 trace 有 `async-disposers-skipped`；对照 `unload_async`（current_thread
  runtime block_on）完整执行异步 disposer。

### 结论

async disposer 的同步/异步卸载路径语义明确：同步 `unload` = 同步 disposer +
显式跳过记录；`unload_async` = 完整并行清理。差异从「静默丢弃」变为「可观察」。
下一轮：Group apply 异步化 / 真实模型 https / HMR watcher。

## 41. M25 交付记录（2026）—— dsh-schema strict 贯穿

**状态：补充交付**（`cargo test` 171 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §6 dsh-schema 差异之一）

`strict` 标志未贯穿（Rust `ResolveOptions` 无 strict 字段）。TS 参照确认：
`Schema.resolve(data, schema, options, strict = false)` 第 4 参——dict/tuple/
object/intersect 的 strict 行为各异。

### dsh-schema（`crates/dsh-schema/src/lib.rs`）

- `ResolveOptions` 增加 `strict: bool`（M25）。
- 按 TS 语义贯穿（`resolve_kind`）：
  - **dict**：sKey 校验失败 → strict 跳过该键 / 非 strict 抛错（原恒跳过）。
  - **tuple**：strict 不追加多余项（非 strict 保留）。
  - **object**：strict 不合并多余键（丢弃）。
  - **intersect**：strict 不合并剩余对象键。

### 新增测试（4 项，`m4_schema.rs`）

1. `strict_object_drops_extra_keys`：非 strict 保留 / strict 丢弃多余键。
2. `strict_tuple_drops_extra_items`：非 strict 追加 / strict 丢弃多余项。
3. `strict_intersect_drops_extra_keys`：非 strict 合并 / strict 丢弃多余键。
4. `strict_dict_skips_invalid_key`：sKey 失败非 strict 抛错 / strict 跳过。

### 结论

Schemastery `strict` 语义完整落地（4 种 kind）。剩余 schema 差异：regex flags
（`i/m/s` 外）、`function`/`is(Class)` 映射。下一轮：Group apply 异步化 /
真实模型 https / HMR watcher / schema regex。

## 42. M26 交付记录（2026）—— schema regex flags + date/regExp 组合子

**状态：补充交付**（`cargo test` 174 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §6 dsh-schema 差异）

regex flags 覆盖验证 + TS 有的 `date`/`regExp` 组合子在 Rust 缺失（补全）。

### dsh-schema（`crates/dsh-schema/src/lib.rs`）

- **regex flags**：`i/m/s` 已实现（`build_regex` 前缀 `(?i)/(?m)/(?s)`）；
  `u` 为 Rust regex 默认（Unicode）；`g/y` 对 test 无意义、被安全忽略——补
  测试覆盖（含 `\p{L}` Unicode、多行 `^`、dotall `.`）。
- **`Schema::date()`**：union[is("Date"), transform(string 校验 RFC3339)]。
  新增 `parse_datetime`（轻量 RFC3339 校验：`YYYY-MM-DDTHH:MM:SS[.frac]?(Z|±HH:MM)`，
  无 chrono 依赖）。
- **`Schema::reg_exp(flag)`**：union[is("RegExp"), transform(string 校验可
  编译)]——源字符串经 `build_regex` 编译验证。
- Value-land 限制：is(Date)/is(RegExp) 恒失败（JSON 无此类），string 分支为
  实际路径（与 TS union 一致）。

### 新增测试（3 项，`m4_schema.rs`）

1. `regex_flags_behavior`：i/m/s 生效、u 默认、g/y 忽略。
2. `date_combinator_validates_rfc3339`：合法 RFC3339 原样返回、非法聚合报错。
3. `regexp_combinator_validates_source`：可编译正则通过、非法拒绝、flag 生效。

### 结论

schema 组合子补全：regex flags 行为验证 + date/regExp（TS 等价）。剩余
`function`/`is(Class)` 为 Value-land 本质限制。下一轮：Group apply 异步化 /
真实模型 https / HMR watcher。

## 43. M27 交付记录（2026）—— Group apply 异步化（EffectOutcome::Await）

**状态：补充交付**（`cargo test` 175 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（HANDOFF §7 方向 1）

Group 的 `[Service.init]` 是 async generator（await update 挂载子入口）→ Group
ACTIVE 在子入口之后。Rust `Plugin::apply` 同步契约无法表达——引入
`EffectOutcome::Await`（apply 期间异步完成）。

### dsh-core（`crates/dsh-core/src/`）

- `fiber.rs`：`EffectOutcome::Await(LocalBoxFuture<'static, EffectOutcome>)`；
  `FiberData.await_children: bool` 标记。
- `context.rs`：
  - `apply_body`：同步路径对 Await `now_or_never`（future 为同步体，立即完成；
    Group 子入口挂载仍在 current 上下文内 → parent 正确）；async 模式保留
    Await（current 不 pop，留给 drive_async_loads await 后 pop）。
  - `drive_async_loads` Apply 分支：Await → `fut.await`（current 保留，子入口
    注册 parent=Group）→ pop → 标记 `await_children` → 排 Finish。
  - `drive_async_loads` Finish 分支：仅 `await_children` 标记的 fiber 在仍有
    Loading 后代时重新入队（等子任务完成）；普通 fiber 不受影响（_reload 的
    父先 Active 时序保持——09 差分不回归）。
  - `unload_async` 的 stack 展开：Await 同 Async（await 得最终 outcome）。
- `runtime.rs`：`fiber_chain_contains`（parent 链判定）；`finish_load` 清标记。

### dsh-loader（`crates/dsh-loader/src/loader.rs`）

- `GroupPlugin.apply` 返回 `Await`（future 内挂载子入口）。
- `update_async`/`update_one_async`：移除队列迭代（group 子入口按 config 序
  立即处理，Box::pin 打破嵌套 group 的 async 递归）——c1 热更在 c3 新建之前
  （对齐 TS `config.map(create)` 顺序）。

### 新增测试（1 项，`m14_loader_async.rs`）

- `group_await_children_before_active`：async 路径 Group 子入口全部 Active 后
  Group Active；卸载递归 stop。

### 结论

Group apply 异步化落地：`EffectOutcome::Await` 表达「apply 期异步完成」，Group
等子入口 Active 后再 Active。group 差分场景新建段（14 行）与热更段已对齐；
剩余 remove 段子入口卸载分布时序略异（总数一致）。下一轮：group remove 段
对齐 / 真实模型 https / HMR watcher。

## 44. M28 交付记录（2026）—— group 热更误删修复 + stop 并行

**状态：补充交付**（`cargo test` 175 项全绿 + clippy 零警告；11 差分场景不变）。

### 目标（M27 后续：group 差分 remove 段诊断）

定位 group 热更后 c2 双卸 trace 冗余：`dispose_entry:c2 → remove:c2 →
dispose-entry:c2 → dispose-entry:c1`（c2 卸两次、c1 被误删）。

### 根因（临时调试确认）

`sync_async` 的 old_map 用 `st.entries.values()` 收集**全部**入口（含 group
子入口 c1/c2）。第二段 sync 时：`update_async(g1)` 的 group 分支已移除缺席
子入口 c2（从 subgroup + entries）；随后「移除缺席旧入口」循环遍历 old_map
（含 c1/c2）→ c1/c2 不在 new_map → 再次 remove（c2 二次卸 + c1 误删）。

### dsh-loader 修复（`crates/dsh-loader/src/loader.rs`）

1. `sync_async` old_map 只收集**根组**入口（`parent_group == root_group`）——
   group 子入口由 group 分支管理，不再被根级 remove 误删。
2. 同步 `dispose_entry`：group 入口先**同步串行**卸子入口（同步路径无法
   await Group 的 Async stop disposer——兜底保证 m2 同步 remove 语义）。
3. GroupPlugin stop disposer 改 `EffectOutcome::Async`（join_all 并行卸载子
   入口——unload_async 路径语义对齐 TS `Promise.allSettled(stop)`）。

### 结论

c2 双卸根因修复（old_map 范围）。group 差分新建段 + 热更段逐行对齐；剩余
remove 段差异：Rust 子入口卸载无内部 await（纯同步段）→ join_all 无法交错，
trace 串行 vs TS 并行（最终状态一致）。下一轮：remove 段并行（卸载路径插入
yield）/ 真实模型 https / HMR watcher。

## 45. M29 交付记录（2026）—— group 差分纳入（卸载让出并行）

**状态：补充交付**（`cargo test` 175 项全绿 + clippy 零警告；**12 差分场景
全部逐行一致**——9 核心 + 3 loader 事务含 group 嵌套）。

### 目标（M28 后续：remove 段串行 vs 并行）

group 差分剩余差异：Rust 子入口卸载是连续同步段（begin_unload → disposers
→ finish_unload 无 await），join_all 无法交错；TS `Promise.all` 卸载先全部
Unloading 再逐个 Disposed。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `unload_async` 插入两个卸载让出点：
  - `begin_unload` 后 `yield_now`——并行卸载的各 fiber 先提交 Unloading。
  - disposers 后、`finish_unload` 前 `yield_now`——Disposed 状态交错提交。
- 效果：Group 子入口并行卸载 trace = `a:Active:Unloading ×2` → `a:Unloading:
  Disposed ×2`（对齐 TS `Promise.all`）。

### 场景（`scenarios/loader-10-group-nested.json`）

- sync 两子入口 → sync 热更（c1 更新 + c3 新建 + c2 移除）→ remove g1。
- `verify-diff.mjs`：加入 ASYNC_SCENARIOS。

### 结论

group 嵌套场景（loader-10，34 行）逐行一致，group 差分正式纳入。loader 事务
/嵌套语义与 TS 完整对齐（12 场景）。下一轮：真实模型 https / HMR watcher /
C ABI WASI net。

## 46. M30 交付记录（2026）—— 组件路径 WASI net 验证

**状态：补充交付**（`cargo test` 177 项全绿 + clippy 零警告；12 差分场景不变）。

### 目标（HANDOFF §7 方向 5：net 位端到端）

C ABI 路径（preview1）net 位受 wasmtime 34 socket stub 限制；组件路径
（preview2 `inherit_network`/`allow_tcp`/`check_allowed_tcp`）已支持——验证
组件路径的 net 路径。

### 实现

- `wasm-plugins/hello-net/`（新）：wasip1 组件插件（dsh-plugin world），
  `apply(config)` 经 `std::net::TcpStream::connect(config.host:config.port)` →
  `host_api::log("NET_OK/NET_ERR=...")`。
- `m30_net_component.rs`（新，2 项）：
  1. `component_wasi_net_path_reachable`：组件尝试网络不崩溃、日志记录 NET_*
     （本地 mock TCP 服务器）。
  2. `component_wasi_net_capability_configured`：net 位下 `build_wasi_ctx`
     构建成功（能力授予路径存在）。

### 发现（平台限制）

wasm32-wasip1 的 `std::net::TcpStream` **未实现**（Rust std 不映射 preview2
`sockets`）→ 连接返回 `operation not supported on this platform`（NET_ERR，
不崩溃）。能力授予机制（`inherit_network`/`allow_tcp`/`check_allowed_tcp`）
已配置——端到端 TCP 受 wasmtime 34 + Rust std 平台限制（已知，待工具链支持）。

### 结论

WASI net 能力授予机制完整（配置 + 能力检查存在），组件网络路径可达（不
崩溃、结果记录）。端到端 TCP 受平台限制，记录为已知。下一轮：真实模型
https / HMR watcher。

## 47. M31 交付记录（2026）—— llm_http https 支持（native-tls）

**状态：补充交付**（`cargo test` 179 项全绿 + clippy 零警告；12 差分场景不变）。

### 目标（HANDOFF §7 方向 2）

真实 HTTP llm 客户端仅支持 http://；补 https（TLS）。

### dsh-core（`crates/dsh-core/src/llm_http.rs` + `Cargo.toml`）

- `native-tls` 依赖（dependencies + dev-dependencies）。
- `parse_base`：解析 `https://`（默认端口 443）与 `http://`（80）→ 返回
  (scheme, host, port, path)。
- `tcp_exchange(scheme, ...)`：https 时 `TlsConnector`（证书验证默认开启——
  生产安全）包裹 TcpStream；http 直连。
- `trait ReadWrite: Read + Write`（TcpStream/TlsStream 统一盒装）。

### 新增测试（2 项，`m17_http_llm.rs`）

1. `https_provider_tls_handshake_path`：openssl 生成自签证书 → native-tls
   服务端（PKCS#12 Identity）起 TLS mock 服务器 → https 客户端连接——证书
   验证拒绝自签 → error JSON（证明 TLS 层可达、生产验证路径正确）。
2. `https_base_defaults_to_443`：https URL 连接失败（conn refused）→ error
   JSON 不 panic。

### 结论

真实 HTTP llm 支持 https（TLS）：`parse_base` 双 scheme + native-tls 包裹。
证书验证默认开启（自签被正确拒绝——生产安全）；真实 API 验证需可信证书。
下一轮：HMR watcher / 其余收尾。

## 48. M32 交付记录（2026）—— session 缝投影对齐（空 assistant 跳过）

**状态：补充交付**（`cargo test` 180 项全绿 + clippy 零警告；12 差分场景不变）。

### 目标（session 缝权威契约对齐）

对照 vendored `@deepseek-ai/dsh-session` 的 `deriveEventMessage` 权威投影规则：
- `user/message` → `event.data`（消息）
- `assistant/message` → **空 content 返回 null**（仅承载 usage 的 max-tokens
  助手消息不入模型历史）；否则 `event.data.message`
- `tool/result` → `event.data.message`

Rust `SessionLog::derive_messages` 此前**无条件输出** assistant/message（即使
content 空）——补跳过规则。

### dsh-core（`crates/dsh-core/src/session.rs`）

- `derive_messages`：`assistant/message` 空 content（null 或空字符串）→ 跳过
  （continue）；其余消息照常投影。

### 新增测试（1 项，`m9_session_tools.rs`）

- `session_log_skips_empty_assistant`：user + 空 content assistant + 实 content
  assistant → 空消息被跳过、两条消息入历史。

### 结论

session 缝投影与 DSH `deriveEventMessage` 规则对齐（空 assistant 跳过）。
剩余预留差异：Rust 演示 loop 的消息形状为扁平契约（`{role, content}`），
DSH 生产为 message 对象（id/source/content 数组）——完整对齐需同时改写入端
（llm-loop 组件）与读取端，记录为预留。下一轮：HMR watcher / 预留差异。

## 49. M33 交付记录（2026）—— include patch 对齐（group insert + 嵌套命中）

**状态：补充交付**（`cargo test` 182 项全绿 + clippy 零警告；12 差分场景不变）。

### 目标（include patch 语义对齐）

对照 vendored `@deepseek-ai/cordis-plugin-include` 的 `applyEntryPatches`：
1. `insert` 带 `id` → 向该 id 的 **group** config 数组插入（目标非 group 则
   跳过 + warn）；无 id → 顶层追加。
2. `id` patch 命中**嵌套**入口（entryMap 含 group 子入口）。
3. name mismatch 跳过。

Rust 此前只支持顶层 insert / 顶层 id 命中。

### dsh-loader（`crates/dsh-loader/src/include.rs`）

- `apply_entry_patches`：insert 带 id → `patch_insert_into_group`（递归重建，
  目标必须是 group）；id patch → `patch_update`（`&dyn Fn` trait object 打破
  泛型递归的类型膨胀）。
- 纯函数式递归重建（patch 数据小，clone 可接受；无借用冲突）。

### 新增测试（2 项，`m3_include.rs`）

1. `apply_patches_insert_into_group`：insert 带 id → group config 追加；非
   group 目标跳过。
2. `apply_patches_hits_nested_group_child`：id patch 命中嵌套 c1 → config 覆盖。

### 结论

include patch 语义与 Cordis `applyEntryPatches` 对齐（group insert + 嵌套
命中）。剩余 include 差异：patch warn sink（Rust 静默跳过）——可选增强。
下一轮：HMR watcher / 预留差异。

## 50. M34 交付记录（2026）—— session 缝消息形状对齐（生产 Message 对象）

**状态：补充交付**（`cargo test` 184 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 DSH 层缝的承载形状）。

### 目标（session 缝消息形状完整对齐）

M32 记录预留差异：Rust 演示 loop 的消息为**扁平契约**（`{role, content}`），
DSH 生产为 **message 对象**（`{id, role, content: ContentBlock[], source}`）。
本轮把 session 缝的消息形状完整对齐生产——权威契约取自 deepseek-harness：

- `Message`（`packages/llm/llm/src/message.ts`）：`{id, role, content,
  source}`；role ∈ system/user/assistant。
- `ContentBlock`（`packages/llm/llm/src/types.ts`）：`text`/`reasoning`/
  `image`/`tool-call`/`tool-result`（按 `type` 判别）。
- `MessageSource`：`{kind:'user'}` / `{kind:'plugin', plugin}` /
  `{kind:'model', provider, model}` / `{kind:'tool', callId}`。
- `deriveEventMessage`（`packages/core/session/src/surface.ts`）：
  - `user/message` → **`event.data` 逐字透传**（data 本身即完整 Message 对象，
    生产 `'user/message': UserMessage`——非包装）；
  - `assistant/message` → `event.data.message`（data 为 `{turn, step, message,
    usage?}` 包装；content 空数组 → null 跳过）；
  - `tool/result` → `event.data.message`（data 为 `{turn, step, message,
    error?, meta?}` 包装；ToolResultMessage：role=user + tool-result block +
    source.tool）。

### WIT 契约（`crates/dsh-wasmrt/wit-dsh/dsh-loop.wit`）

session 接口的 record 定义更新为生产 Message 形状：
- 新增 `message-source` variant（user/plugin/model/tool）、`content-block`
  variant（text/reasoning/tool-call/tool-result）、`message` record
  （id/role/content/source）。
- `user-message` record = 完整 Message 对象（data 即消息本身）；
  `assistant-message` / `tool-result` record = `{turn, step, message}` 包装。
- ⚠️ WIT 禁止递归类型：生产 `ToolResultBlock.content` 为 `ContentBlock[]`，
  收敛为 `list<text-block>`（实际即文本块；`serialize.ts` 也只取
  `flattenText`）。

### dsh-core（`crates/dsh-core/src/session.rs` + `llm_http.rs`）

- `SessionLog::derive_messages` 重写：`user/message` → data 逐字透传；
  `assistant/message` / `tool/result` → `data.message`（空 content 数组跳过）。
- `llm_http::messages_to_wire`（新增）：生产 `Message[]` → OpenAI wire 序列化
  （对齐 DSH `serializeMessages`）——system/assistant 文本拼接 + tool-call →
  `tool_calls`、user 文本 + tool-result 展开为 `{role:'tool', tool_call_id}`；
  空 tool 输出补 `"(no output)"`；扁平形状（content 为字符串）原样透传
  （M17 兼容）。`chat_completions` 发送前先经此转换。

### dsh-wasmrt（`src/loop.rs` + `src/services.rs`）

- `LoopHost::derive_messages`（内存回退）同步对齐生产形状。
- 声明式适配器读生产形状：tool-first 判别 `content[0].type == "tool-result"`
  （不再 `role == "tool"`）；echo 取 user 消息的 text block 拼接（排除
  tool-result 消息——生产形状下 ToolResultMessage 的 role 也是 "user"）。

### WASM loop 插件（echo-loop / tool-loop / llm-loop）

写入端改生产形状：`user/message` data = 完整 Message（id 用确定性
`u{turn}`/`a{turn}`/`t{turn}`，不引入 uuid 依赖）；`assistant/message` /
`tool/result` data = `{turn, step, message}` 包装（assistant source.model、
tool source.tool + tool-result block）。llm-loop 的 turn 计数按
`content[0].type != "tool-result"` 判别（排除 ToolResultMessage）。

### 新增测试（2 项，`m17_http_llm.rs`）

1. `messages_to_wire_produces_openai_shape`：user 文本 + assistant
   text/tool-call + tool-result → 三条 wire 消息（含 `tool_calls` 映射）。
2. `messages_to_wire_empty_tool_and_flat_passthrough`：空 tool 输出 →
   `"(no output)"`；扁平形状原样透传。

### 更新测试

`m9_session_tools.rs`（投影断言改生产形状，含空 assistant 跳过）、
`m8_dsh_loop.rs` / `m9_loader_assemble.rs` / `m9_yaml_assemble.rs` /
`m9_boot.rs`（LLM 适配器判别与消息断言改生产形状）。

### 结论

session 缝消息形状与 DSH 生产 `Message` 对象完整对齐（写入端 + WIT +
投影端 + llm 消费端）。剩余差异：`image` block 为 forward-compatibility
（WIT 预留）；id 为确定性生成而非 uuid（不引入依赖）。下一轮：HMR watcher
事件驱动 / 其余收尾。

## 51. M35 交付记录（2026）—— HMR 文件 watcher 事件驱动（notify + mpsc 桥接）

**状态：补充交付**（`cargo test` 186 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 loader 的 HMR 基建）。

### 目标（事件驱动对齐 Cordis chokidar）

HMR 此前为**轮询**（`poll()` 全量扫描 + 内容指纹，CLI 50ms 周期）；Cordis
用 chokidar（OS 文件系统通知，事件驱动）。本轮把 HMR 改为事件驱动——文件
变化即时到达，消除固定轮询延迟与无谓扫描。

### 约束与解法（单线程纪律 × notify）

- `Hmr` 是 `Rc<RefCell>`（非 Send），refresh 回调 `Rc<dyn Fn>`（非 Send）——
  **不能**放进 notify 的后台线程；
- notify `recommended_watcher` 的 event_handler 需 `Send + 'static` 且在内部
  线程运行——**冲突**；
- 解法：**mpsc 桥接**——后台线程仅持有 `Sender<PathBuf>`（Send）收集变化
  路径；`Hmr` 持 `Receiver`，`poll()` 时 `try_recv` 消费；
- 事件只作**唤醒信号**：消费后仍做**指纹确认**（notify 事件可能重复/合并/
  误报临时文件——指纹兜底，保证「内容确实变化才 refresh」）；
- 无 watcher 时 `poll()` 退化为全量轮询（API 兼容，旧测试不破）。

### dsh-loader（`crates/dsh-loader/src/hmr.rs` + `Cargo.toml`）

- 新增依赖 `notify = "8"`。
- `Hmr::watch(paths)`：启动 notify watcher（非递归监视）+ mpsc；返回
  Err = 启动失败（路径不存在/无权限），Hmr 仍可用（退化轮询）。
- `Hmr::unwatch()`：停止 watcher（退回轮询）。
- `poll()`：有 watcher → 先 drain 事件队列（仅收集**注册过**的路径、去重）
  再指纹确认 + refresh；无 watcher → 全量轮询（原逻辑）。

### dsh-cli（`src/main.rs`）

`--watch` 启动时调用 `watch(watch_paths)`（失败 fallback 轮询并告警）；
主循环 `poll()` 语义不变（现在消费事件队列）。

### 新增测试（2 项，`m15_hmr.rs`）

1. `hmr_watch_event_driven_triggers_refresh`：watch 启动 → 改文件 → 事件
   驱动 poll 触发 refresh；事件消费后无新变化不触发。
2. `hmr_watch_ignores_unregistered_paths`：未注册路径变化（watcher 未监视）
   不触发；注册路径变化正常触发。

### 结论

HMR 从轮询升级为 OS 事件驱动（对齐 chokidar），单线程纪律经 mpsc 桥接
保持。剩余差异：notify 事件仅作唤醒、指纹确认兜底（比 chokidar 更稳）；
轮询路径保留为 fallback。下一轮：真实 API https / 其余收尾。

## 52. M36 交付记录（2026）—— session surface 折叠（append/replace + shadow）

**状态：补充交付**（`cargo test` 190 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 DSH 层 session 承载）。

### 目标（surface 折叠对齐 DSH `foldSurface`/`SessionSurface`）

M34 对齐了消息**形状**；但 DSH 生产的 `deriveMessages` 依赖 **surface 折叠**
（`packages/core/session/src/surface.ts`）：`SURFACE_EVENT_TYPES` =
user/message、assistant/message、tool/result；`foldSurface` 维护模型可见
节点序列——append 入列、replace 替换 [start, end] 范围（compaction 语义，
旧节点被 shadow）、`replaceGeneration` 递增；投影只对**当前 surface 节点**
进行。Rust `SessionLog` 此前遍历**全部事件**投影，无 replace/shadow 语义
（compaction 等依赖它）。

### dsh-core（`crates/dsh-core/src/session.rs` + `lib.rs`）

- `SurfaceOp` enum（`Append` | `Replace { start, end }`）导出。
- `SessionLog` 增加 surface 折叠状态：`surface: Vec<u64>`（节点 seq，模型
  可见顺序）+ `replace_generation: u64`。
- `append(kind, payload)` 签名不变（WIT 缝契约）：surface-eligible 事件以
  `Append` 自动入列——纯 append 场景与遍历全部事件投影**完全等价**
  （既有测试/loop 插件零改动）。
- `append_with_op(kind, payload, op)`（新增，宿主侧 compaction 等用）：
  - 非 surface-eligible 事件带 `Replace` → 报错（对齐 `surfaceOpOf`）；
  - `Replace` 的 start/end 必须都在当前 surface 上且 start ≤ end（对齐
    `replacementRange`）；失败**原子**（splice 前无状态变更）。
- `surface_nodes()` / `replace_generation()` 访问器（对齐 `SessionSurface`）。
- `derive_messages` 改为遍历 surface nodes（`events[seq]`）而非全部事件——
  replace 后旧节点被 shadow，投影只含当前节点。

### 新增测试（4 项，`m9_session_tools.rs`）

1. `session_surface_append_tracks_nodes`：turn/start 等日志事件不入列；
   surface = eligible seq；投影与遍历等价。
2. `session_surface_replace_shadows_old_nodes`：replace [0,1] → 新 user 节点
   替换，旧 user/assistant 被 shadow；`replaceGeneration` 递增；投影含新
   user + 原 tool-result。
3. `session_surface_replace_invalid_range_fails`：start/end 不在 surface →
   报错；失败原子（surface 未破坏）。
4. `session_surface_replace_rejected_on_log_events`：日志事件带 replace →
   报错。

### 结论

session surface 折叠与 DSH `foldSurface` 语义对齐（append 入列 + replace
替换 + shadow + generation 计数 + 投影只含当前节点）；WIT 缝签名不变
（loop 走 append；replace 是宿主侧 compaction 能力）。剩余差异：
`sourceEventSeqs` 来源校验 / tool-result replace 仅改 content 约束（生产
防御性校验，Rust 侧未实现——无 compaction 消费方）；`image` block 预留。
下一轮：真实 API https / 其余收尾。

## 53. M37 交付记录（2026）—— session surface 防御性校验（provenance + tool-result rewrite）

**状态：补充交付**（`cargo test` 193 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 DSH 层 session 承载）。

### 目标（对齐 DSH `surface.ts` 校验链）

M36 实现了 surface 折叠核心，但生产 `surface.ts` 的 `planSurfaceEvent` 还
有**防御性校验链**未对齐：`surfaceOpOf` → `replacementRange` →
`assertProvenance` → `assertToolResultRewrite`。本轮补全后两环：

- `assertProvenance`：`sourceEventSeqs` 引用必须**早于**当前事件 seq、
  无重复；空数组仅 `assistant/message` 允许（known empty provider stream）；
  replace 时**必须覆盖全部被 shadow 节点**（missing 报错）。
- `assertToolResultRewrite`：`tool/result` 的 replace 必须恰好重写 1 个
  当前 `tool/result` 节点，且只允许改 content（双方 `message.content[0]`
  的 content 置 null 后深比较其余字段）。

### dsh-core（`crates/dsh-core/src/session.rs`）

- `append_with_provenance(kind, payload, op, source_event_seqs)`（新增）：
  完整校验链 + 原子提交（全部通过才 splice + push）。
- `append_with_op` 保留为便捷入口（source_event_seqs=None：append 合法；
  replace 无来源覆盖按 `assertProvenance` 报错）。
- `append` 不变（WIT 缝签名；Append 便捷路径）。
- `tool_result_only_content_changed`：对齐生产的「content 置 null 深比较」。

### 新增/更新测试（`m9_session_tools.rs`）

新增 3 项：
1. `session_surface_replace_requires_provenance`：replace 缺 source_event_seqs
   / 部分覆盖 → 报错；失败原子。
2. `session_surface_provenance_reference_validation`：引用 >= 当前 seq /
   重复 / 非 assistant 空数组 → 报错；assistant 空数组 → 允许。
3. `session_surface_tool_result_rewrite_rule`：tool/result replace 只改
   content → 允许（generation 递增）；改 callId 等 → 报错。

更新 1 项：`session_surface_replace_shadows_old_nodes` 改用
`append_with_provenance`（带 source_event_seqs=[0,1]）。

### 结论

session surface 校验链与 DSH `surface.ts` 完整对齐（provenance 引用/覆盖 +
tool-result 仅改 content）。剩余差异：生产 `Session.append` 要求 surface-
eligible 事件必须带 surfaceOp（Rust WIT 缝 append 无 op 参数，宽松接受）；
`image` block 预留。下一轮：真实 API https / 其余收尾。

## 54. M38 交付记录（2026）—— HMR refresh 失败事件化（hmr/config-update-failed）

**状态：补充交付**（`cargo test` 195 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 loader 的 HMR 通知机制）。

### 目标（失败通知对齐 Cordis 事件语义）

对照 vendored `cordis-plugin-hmr`（`vendor/hmr/src/index.ts`）：refresh 失败
时 `ctx.parallel('hmr/config-update-failed', filename, error)`（**parallel
模式事件**，任何插件可监听）——注意事件名是 `hmr/config-update-failed`，
不是此前 HANDOFF 误记的 `hmr/error`。Rust `Hmr` 此前只有 `take_errors()`
查询式（宿主轮询被动拉取），未对齐事件式通知。

### dsh-loader（`crates/dsh-loader/src/hmr.rs`）

- `Hmr::set_error_sink(Rc<ErrorSink>)`（M38）：注册 refresh 失败的事件通知
  `Fn(&str, &CordisError)`（filename, error）。
- `poll()` 失败分支：既记录 errors（`take_errors` 保留，向后兼容）**又**调用
  sink（事件式；None = 仅记录，与旧行为一致）。
- `ErrorSink = dyn Fn(&str, &CordisError)` type 别名（clippy 复杂类型）。

### dsh-cli（`src/main.rs`）

`--watch` 时 `set_error_sink`：经 `ctx.parallel("hmr/config-update-failed",
[filename, {message}])` emit（对齐 Cordis parallel 事件，监听者经
`ctx.on("hmr/config-update-failed", …)` 注册）+ eprintln 诊断；主循环
`take_errors` 只清空不重复打印。

### 新增测试（2 项，`m15_hmr.rs`）

1. `hmr_error_sink_receives_failures`：refresh 失败 → sink 收到 (filename,
   error)；`take_errors` 仍可用（双通道）。
2. `hmr_without_error_sink_records_only`：无 sink → 仅记录，不 panic。

### 结论

HMR 失败通知与 Cordis `hmr/config-update-failed` 事件语义对齐（parallel
emit + 查询并存）。剩余差异：Cordis 的模块级 HMR（partialReload/依赖图）
非配置 HMR 范畴；`hmr/change`/`hmr/reload` 事件对应模块热更，Rust 侧无
（配置驱动）。下一轮：真实 API https / 其余收尾。

## 55. M39 交付记录（2026）—— include patch warn sink（未命中诊断）

**状态：补充交付**（`cargo test` 197 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 include patch 的诊断可观测性）。

### 目标（对齐 Cordis `applyEntryPatches` 的 warn sink）

对照 vendored `cordis-plugin-include`（`vendor/include/src/index.ts`）：
`applyEntryPatches(data, patches, warn)` 带 **warn sink**——patch 未命中
（id 找不到 / 非 group / name mismatch / 缺 id）时 `warn('patch …')`
（printf 风格 `%C`）输出诊断，否则失败**静默**。Rust
`apply_entry_patches(data, patches)` 此前静默跳过（无 sink），调用方
（Include.read）无法感知 patch 失败——诊断可观测性差异。

### dsh-loader（`crates/dsh-loader/src/include.rs` + `lib.rs`）

- `apply_entry_patches_with_warn(data, patches, warn: &mut dyn FnMut(String))`
  （新增）：warn sink 版（消息为格式化好的字符串，等价 TS `%C` 展开）；
  跳过场景与 TS 逐条对齐：
  - `patch: id is required for non-insert patches`（缺 id）；
  - `patch: entry {id} not found`（id 未命中）；
  - `patch: name mismatch for {id} (expected {got}, got {name}), skipping`；
  - `patch insert: entry {id} not found` / `is not a group`（insert 目标）。
- `apply_entry_patches` 保留（静默版，委托 with_warn 丢弃 warn——向后兼容
  M33 测试）。
- `Include` 增加 `warns: RefCell<Vec<String>>` + `take_warns()`：`read()` 用
  with_warn 收集（每次 read 重置；`load`/`refresh` 后宿主可查询诊断）。

### 新增测试（2 项，`m3_include.rs`）

1. `apply_patches_with_warn_reports_skips`：5 种跳过场景 → warn sink 收到
   对应消息；结果 = 原数据（跳过不影响结果）。
2. `include_take_warns_collects_patch_skips`：Include.read 收集未命中警告
   （take_warns）；命中 patch 无警告；refresh 后重新收集。

### 结论

include patch 未命中诊断与 Cordis warn sink 语义对齐（logger 输出 vs
`take_warns` 查询）。剩余 include 差异：无（insert/嵌套/递归/warn 全对齐）。
下一轮：真实 API https / 其余收尾。

## 56. M40 交付记录（2026）—— timer 服务（timeout/interval/debounce/throttle）

**状态：补充交付**（`cargo test` 204 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core 新增调度原语）。

### 目标（对齐 deepseek-harness `vendor/timer`）

Cordis 的 `ctx.timeout`/`interval`/`debounce`/`throttle`（deepseek-harness
`vendor/timer/src/index.ts`）是**生命周期绑定的调度原语**——timer 经
`ctx.effect` 注册 disposer（fiber 卸载清除），回调在 fiber 上下文中执行；
HMR 等插件依赖它（`partialReload` 用 `ctx.debounce`）。Rust 侧此前**无
timer 服务**。

### 约束与解法（单线程纪律 × 宿主驱动）

- Rust 单线程无事件循环 → timer 不能自触发；解法：**宿主时钟 + 驱动**——
  `Cordis::set_timer_clock(now: impl Fn() -> u64)`（宿主注入毫秒时钟）+
  `Cordis::drive_timers()`（宿主事件循环调用，CLI 已有 50ms 循环）；
- timer 经 `effect` 绑定当前 fiber（`InactiveEffect` 拒绝非 Active 注册），
  disposer 取消；`collect_due_timers` 双重过滤（fiber 仍 Active 才执行）；
- debounce/throttle 用 `TimerSlot`（包装函数更新 pending，driver 到期执行）；
  单线程捕获（`Rc<RefCell>`），参数经 `Value` 传递。

### dsh-core（`crates/dsh-core/src/runtime.rs` + `context.rs` + `lib.rs`）

- runtime：`TimerKind`（Once/Interval）、`TimerEntry`（deadline/period/cb/
  fid/alive）、`TimerSlot`（last_at/pending/pending_deadline/cb/fid，`NEVER`
  哨兵 = 从未执行）、`timer_clock`/`timers`/`timer_slots`/`timer_drivers`
  字段；纯变更方法 `register_timer`/`cancel_timer`/`collect_due_timers`/
  `register_timer_slot`/`cancel_timer_slot`/`collect_due_slots`。
- `Cordis`：`set_timer_clock`、`timeout(cb, delay) -> Disposer`（Once）、
  `interval(cb, delay) -> Disposer`（Interval，到期重排）、
  `debounce(cb, delay) -> (TimerFn, Disposer)`（delay 内只执行最后一次）、
  `throttle(cb, delay) -> (TimerFn, Disposer)`（leading 立即 + trailing
  窗口末次，对齐 `noTrailing=false`）、`drive_timers()`（收集-再执行：
  先收集到期回调，释放借用后执行——用户代码可重入）。
- `TimerFn = Rc<dyn Fn(Value)>` 导出（clippy 复杂类型）。

### dsh-cli（`src/main.rs`）

boot 后注入宿主时钟（`SystemTime::now` 毫秒）；主循环每轮
`boot.ctx.drive_timers()` 驱动 timer（对齐 Cordis 事件循环驱动）。

### 新增测试（7 项，`m40_timer.rs`，FakeClock 可控时钟）

1. `timer_timeout_fires_after_delay`：未到期不触发、到期触发、一次性。
2. `timer_timeout_disposed_on_unload`：卸载清除未到期 timer。
3. `timer_interval_fires_repeatedly`：周期触发（到期重排）。
4. `timer_interval_disposed_on_unload`：卸载停止周期。
5. `timer_debounce_fires_once_after_idle`：三次调用只执行最后一次。
6. `timer_debounce_disposed_on_unload`：卸载取消 pending。
7. `timer_throttle_fires_leading_and_trailing`：leading 立即 + trailing 末次。

### 结论

timer 服务与 Cordis `vendor/timer` 语义对齐（timeout/interval/debounce/
throttle + 生命周期绑定 + 宿主驱动）。差异：`setTimeout`/`setInterval`
别名（deprecated，未实现——语义等价 timeout/interval）。下一轮：真实
API https / 其余收尾。

## 57. M41 交付记录（2026）—— timer 无回调形态（timeout_async / interval_ticks）

**状态：补充交付**（`cargo test` 207 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core timer 补全）。

### 目标（对齐 `vendor/timer` 的 Promise / AsyncIterable 形态）

M40 实现了回调形态（`timeout(cb)`/`interval(cb)`/`debounce`/`throttle`）；
Cordis 还有**无回调形态**：
- `timeout(delay): Promise<void>`——delay 后 resolve；fiber 卸载 reject
  （"Context has been disposed"）；
- `interval(delay): AsyncIterableIterator<void>`——每 delay 一个 tick，调用方
  `for await` 消费；卸载时 throw。

### 实现（dsh-core `context.rs` + `lib.rs`；纯自驱动，无 tokio 正式依赖）

- `Cordis::timeout_async(delay) -> LocalBoxFuture<'static, Result<(), CordisError>>`：
  `deadline = now + delay` + `cancelled`（Rc<Cell>）；effect 注册 disposer
  （fiber 卸载置 cancelled → future 返回 Err，对齐 Promise reject）；future
  轮询 `yield_now`（与 `fiber_await` 同模式）检查时钟到期。
- `Cordis::interval_ticks(delay) -> IntervalTicks`：`next_tick`（Rc<Cell>）+
  `cancelled`；effect disposer（卸载 → 流结束 None）；`Stream` impl——
  poll 检查时钟，到期产出 + 重排，未到期 Pending（wake_by_ref）。
- `IntervalTicks` 导出（Stream；非 Send——单线程纪律）。

### 新增测试（3 项，`m40_timer.rs`，手动 poll noop-waker 驱动）

1. `timer_timeout_async_resolves_after_delay`：apply 内构造 → 推进时钟 +
   手动 poll → delay 后 Ok。
2. `timer_timeout_async_rejects_on_unload`：卸载 → disposer 置 cancelled →
   poll 返回 Err（"disposed"）。
3. `timer_interval_ticks_yields_periodically`：推进时钟 150ms → 3 个 tick。

### 结论

timer 无回调形态与 Cordis `vendor/timer` 对齐（Promise 延迟 + AsyncIterable
tick + 卸载 reject/结束）。差异：`setTimeout`/`setInterval` 别名（deprecated）
未实现；`interval(delay)` 的 `return()`/`throw()` 显式终止方法（Rust Stream
经 drop 等价）。下一轮：真实 API https / 其余收尾。

## 58. M42 交付记录（2026）—— ctx.once（一次性监听器）

**状态：补充交付**（`cargo test` 210 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core 事件注册补全）。

### 目标（对齐 Cordis `ctx.once`）

对照 vendored `cordis`（`vendor/cordis/src/events.ts`）：`once(name,
listener, options)` = `on` 的包装——首次触发时**先 dispose 自身再调用
listener**（`const dispose = this.on(name, (...args) => { dispose();
listener.apply(this, args) }, options)`），返回同一 disposer（手动移除 /
fiber 卸载均生效）。Rust 侧有 `on`/`on_async`，缺 `once`。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `Cordis::once(name, listener, global, prepend) -> Disposer`：包装监听器
  （`Rc<RefCell<Option<Disposer>>>` 延迟绑定——`on_cb` 返回后赋 disposer；
  首次触发 `take()` + 调用 disposer 移除自身，再调原监听器）。
- `Cordis::once_async(...)`：同语义的异步形态（首次触发同步移除，再 await）。
- 收集-再执行纪律：disposer 内部 `remove_hook` 是纯数据变更，触发时（emit
  已收集 cbs 后）调用安全。

### 新增测试（3 项，`m0_events.rs`）

1. `once_fires_once_then_removes_itself`：首次触发 once + persist；第二次
   emit 只有 persist（once 已移除）。
2. `once_disposer_removes_and_is_idempotent`：手动 dispose → 不触发；重复
   dispose 幂等。
3. `once_works_with_serial_bail`：once 返回值照常传播（bail）；第二次
   serial 无监听器 → None。

### 结论

`ctx.once` 与 Cordis 事件 API 对齐（一次性 + 自移除 + disposer 幂等）。
剩余事件差异：`ctx.once` 的 `EventOptions`（prepend/global 已支持）；
`internal/listener` 拦截（Rust 无该内部事件）。下一轮：真实 API https /
其余收尾。

## 59. M43 交付记录（2026）—— internal/get、internal/set 服务读写拦截

**状态：补充交付**（`cargo test` 213 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core 服务读写补拦截）。

### 目标（对齐 Cordis `ctx.get`/`ctx.set` 的 Proxy 拦截）

对照 vendored `cordis`（`vendor/cordis/src/reflect.ts`）：`ctx.get`/`ctx.set`
经 **Proxy handler** 走 `internal/get`/`internal/set` **waterfall**——插件可
拦截服务读写（`internal/get` 返回替代值短路 inner；`internal/set` 返回
false veto 写入），inner 是实际查表/写入。Rust 侧 `get`/`set` 此前直接查表/
写入，无拦截点。

### dsh-core（`crates/dsh-core/src/context.rs` + `reflect.rs`）

- `get_value(name)`：先 `internal/get` waterfall——`args: [name, Null]`，
  inner = `get_raw_value`（accessor 或 Value 服务）；拦截器短路返回替代值。
- `set_value(name, value)`（新增）：先 `internal/set` waterfall——`args:
  [name, value, Null]`，inner = `set_raw_value`（accessor set 钩子或覆盖
  Value 服务值，仅提供者 fiber 可写，对齐 Cordis `set` 所有者校验）；
  监听器返回 false（veto）→ Err。
- `AccessorGet`/`AccessorSet` 改 `Box<dyn Fn>` → `Rc<dyn Fn>`（可 clone 取出
  后**无借用调用**——accessor get 内可重入，修复 mixin 重入的 RefCell 冲突；
  收集-再执行纪律的延伸）。
- `get` 的 accessor 分支同样改为 clone 闭包无借用调用（旧代码 borrow 内调
  用户代码的隐患）。

### 新增测试（3 项，`m1_service.rs`）

1. `internal_get_intercept_overrides_service_read`：拦截器命中 "cfg" → 返回
   替代值（短路 inner）；未拦截名走 inner（None）。
2. `internal_set_intercept_vetoes_write`：拦截器 veto → Err；值不变。
3. `internal_set_passthrough_writes`：未拦截 → 提供者 fiber 内写入生效。

### 结论

`ctx.get`/`ctx.set` 的服务读写拦截与 Cordis Proxy 语义对齐（internal/get/set
waterfall + 短路/veto）。差异：`Arc<dyn Any>` 形态的 `get`/`set` 不拦截
（拦截是 Value 层面，`Arc<dyn Any>` 非 JSON 可表达——Value-land 本质限制）。
下一轮：真实 API https / 其余收尾。

## 60. M44 交付记录（2026）—— internal/listener 注册拦截

**状态：补充交付**（`cargo test` 215 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core 事件注册补拦截）。

### 目标（对齐 Cordis `ctx.on` 的 `internal/listener` bail）

对照 vendored `cordis`（`vendor/cordis/src/events.ts`）：`on(name, listener,
options)` 注册前先 `bail('internal/listener', name, listener, options)`——
bail 有结果（非 null）则**直接返回该结果**（拦截注册，如权限守卫拒绝/
替换）。Rust 侧 `on_cb` 此前直接注册，无拦截点。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `on_cb` 注册前先 `bail("internal/listener", [name, global, prepend])`；
  bail 值非 null → 注册被拦截，返回 **no-op disposer**（调用方拿到 disposer
  但实际未注册）。`once`/`once_async` 内部走 `on_cb` 自动生效。
- 差异（Value-land）：bail 值无法表达 Rust disposer（`Rc<dyn Fn>`），仅作
  拦截标记（Cordis 可返回替代 disposer 替换注册）。

### 新增测试（2 项，`m0_events.rs`）

1. `internal_listener_intercept_blocks_registration`：拦截器命中 "blocked" →
   返回非 null → 监听器未注册（emit 不触发）；未拦截名正常注册触发。
2. `internal_listener_intercept_blocks_once`：once 内部走 on → 同样被拦截。

### 结论

`ctx.on` 的注册拦截与 Cordis `internal/listener` bail 语义对齐（拦截 →
不注册）。事件/服务拦截面（M42 once / M43 internal-get-set / M44
internal-listener）补齐。差异：bail 值仅作拦截标记（无法替换 disposer，
Value-land 限制）。下一轮：真实 API https / 其余收尾。

## 61. M45 交付记录（2026）—— headless 单发模式（--once）

**状态：补充交付**（`cargo test` 219 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-cli 增加 headless 入口）。

### 目标（对齐 DSH `dsh --profile headless "job"`）

生产 CLI 的 headless profile：提交一个任务（user 消息）→ 驱动 loop → 等
quiescence → **从持久化 session 事件推导最终答案**（最后一个非空 assistant
文本）与 **turn/end reason** → 打印文本，`completed` 退出 0 否则 1；成功不
写 stderr。Rust `dsh` CLI 此前只有交互式 stdin 模式。

### dsh-cli（`crates/dsh-cli/src/lib.rs` + `main.rs`）

- `run_headless(boot, task) -> Result<HeadlessResult, CordisError>`：`run_turn`
  驱动 loop；`derive_headless(events)` 从 session 事件流推导——
  - 最后一条 `assistant/message` 的 `data.message.content[0].text`（M34 生产
    形状；空文本跳过，对齐 `derive_messages` 空 assistant 跳过）；
  - 最后 `turn/end` 的 `data.reason`；
  - 无非空 assistant → Err（fail loud）。
- `derive_headless` 为 `pub(crate)` 独立函数（错误路径可单测）。
- `main.rs`：`--once <task>` 参数——headless 单发：打印答案，`reason ==
  "completed"` → exit 0 否则 1；失败 stderr + exit 1。

### 新增测试（4 项）

集成（`m9_boot.rs`，2 项）：
1. `headless_echo_loop_returns_answer_and_reason`：echo-loop 答案 + completed。
2. `headless_llm_loop_full_turn`：llm-loop 完整 turn（模型→工具→回答），
   答案含 "sum is 5"（tool-first ctx=N）。

单元（`lib.rs` `#[cfg(test)]`，2 项）：
3. `derive_headless_no_answer_fails`：无 assistant → Err。
4. `derive_headless_skips_empty_assistant`：空 content 助手消息跳过，取后
   续非空答案。

### 结论

`dsh --once` headless 单发与 DSH headless profile 语义对齐（session 事件
推导最终答案 + reason → 退出码）。差异：无持久化（内存 session）；无
`SIGINT`/`SIGTERM` dispose（交互模式已有 EOF 路径）。下一轮：真实 API
https / 其余收尾。

## 62. M46 交付记录（2026）—— timer 别名（setTimeout / setInterval）

**状态：补充交付**（`cargo test` 221 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core timer API 补全）。

### 目标（对齐 `vendor/timer` 的 deprecated 别名）

对照 deepseek-harness `vendor/timer`：`setTimeout(cb, delay)` /
`setInterval(cb, delay)` 是 **deprecated 别名**——直接委托 `timeout` /
`interval`。M40 实现了 timeout/interval，别名此前缺失（HANDOFF 记录为
可继续项）。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `Cordis::set_timeout(cb, delay) -> Disposer`：委托 `timeout`。
- `Cordis::set_interval(cb, delay) -> Disposer`：委托 `interval`。

### 新增测试（2 项，`m40_timer.rs`）

1. `timer_set_timeout_alias_fires_once`：别名一次性触发 + 不重复。
2. `timer_set_interval_alias_repeats`：别名周期触发 + 卸载停止。

### 结论

timer API 与 `vendor/timer` 全量对齐（timeout/interval/debounce/throttle +
timeout_async/interval_ticks + setTimeout/setInterval 别名）。下一轮：真实
API https / 其余收尾。

## 63. M47 交付记录（2026）—— session JSONL 持久化（save_to / load_from）

**状态：补充交付**（`cargo test` 224 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core session 持久化 + CLI 保存）。

### 目标（对齐 DSH `session-persistence-jsonl`）

生产 `session-persistence-jsonl`（`packages/session/session-persistence-jsonl/
src/format.ts`）：JSONL 文件首行 header（`{"type":"session","version",...}`），
之后每行一个事件记录；`scanLog` 容忍 torn tail（损坏/无换行尾部丢弃）、
seq 连续校验。Rust `SessionLog` 此前纯内存。

### dsh-core（`crates/dsh-core/src/session.rs`）

- `save_to(path)`：写 header 行（`{"type":"session","version":0}`）+ 每事件
  一行 `{"kind","seq","payload"}`（payload 为事件 data JSON 字节，合法 JSON
  内联、否则字符串保真）。
- `load_from(path)`：读 header（必须 `{"type":"session"}`，否则报错）+ 事件
  行；torn tail（损坏行）→ 停止保留完整前缀（对齐 `scanLog`）；重建
  events + surface（append 语义重放——持久化会话为 append 轨迹，replace
  的 sourceEventSeqs 不可恢复）。
- `SessionLog` 加 `#[derive(Debug)]`。

### dsh-cli（`src/main.rs`）

`--session-out <file>`：headless（`--once`）后把会话保存为 JSONL。

### 新增测试（3 项，`m9_session_tools.rs`）

1. `session_save_load_roundtrip`：保存 → 文件首行 header + 事件行；重建后
   events/surface/投影一致。
2. `session_load_tolerates_torn_tail`：追加损坏行 → 忽略，保留完整前缀。
3. `session_load_missing_header_fails`：缺 header → 报错（fail loud）。

### 结论

session JSONL 持久化与 DSH `session-persistence-jsonl` 核心格式对齐（header
+ 事件行 + torn-tail 容忍）。差异：无 id/createdAt header 字段（Rust
SessionLog 无元数据）；无 zstd 压缩；无 seq 校验（append 天然连续）。
下一轮：真实 API https / 其余收尾。

## 64. M48 交付记录（2026）—— 恢复会话继续（restore_session / --session-in）

**状态：补充交付**（`cargo test` 226 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-cli 增加恢复入口）。

### 目标（对齐 DSH resume 语义）

DSH resume = 从持久化事件日志重建 Session → 继续追加新 turn（`session_
history()` 投影含前轮消息 → 多轮共享上下文）。M47 有了 `load_from`；
缺 CLI 入口——`--session-in`：boot 后把历史 events 导入 `boot.sessions`，
后续 turn 的 llm 输入含前轮上下文。

### dsh-cli（`crates/dsh-cli/src/lib.rs` + `main.rs`）

- `restore_session(boot, path)`：`SessionLog::load_from` → 遍历 events 用
  `append(kind, payload)` 导入 `boot.sessions`（append 重放 events + surface；
  handle 不可替换——逐条导入）。
- `main.rs`：`--session-in <file>`——boot 后、headless/交互前恢复；失败
  stderr + exit 1。

### 新增测试（2 项，`m9_boot.rs`）

1. `restore_session_resumes_context`：跑一轮 + save（--session-out）→ 新
   boot + restore → 再跑一轮：tool-first 回答的 ctx=N 增大（历史累积，
   多轮共享上下文）。
2. `restore_session_missing_file_fails`：不存在文件 → 报错（fail loud）。

### 结论

会话恢复与 DSH resume 语义对齐（JSONL 加载 + 多轮上下文延续）。至此
session 生命周期闭环：记录（append）→ 保存（--session-out）→ 恢复
（--session-in）→ 继续。差异：无 fork/分支会话（DSH `Session.fork`）。
下一轮：真实 API https / 其余收尾。

## 65. M49 交付记录（2026）—— fork 分支会话

**状态：补充交付**（`cargo test` 229 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core session 分支）。

### 目标（对齐 DSH `Session.fork`）

生产 `packages/core/session/src/index.ts` 的 `fork(source, boundary?)`：
从**稳定前缀**派生子会话——boundary 为包含的源事件 seq（省略 = 最后事件，
空源 → 空子）；boundary 必须存在且是连续 seq（否则 `INVALID_BOUNDARY`）；
前缀内最后一个 turn 边界若是 turn/start → `OPEN_TURN` 报错；返回子会话
（events 前缀 + parent 元数据）。Rust `SessionLog` 此前无分支能力。

### dsh-core（`crates/dsh-core/src/session.rs`）

- `SessionLog::fork(boundary: Option<u64>) -> Result<SessionLog, CordisError>`：
  截取 [0, boundary] 前缀 + 三重校验（存在性/连续性/OPEN_TURN）+ 前缀重放
  （append 语义重建 events + surface）。父会话不可变。

### 新增测试（3 项，`m9_session_tools.rs`）

1. `session_fork_slices_prefix`：显式边界（turn/end 处）→ 前缀 + 投影一致；
   分支继续追加不影响父会话；默认边界（open turn 内）→ 报错。
2. `session_fork_empty_yields_empty`：空源 → 空子。
3. `session_fork_invalid_boundary_fails`：越界 / open turn → 报错（父会话
   不受影响）。

### 结论

fork 分支会话与 DSH `Session.fork` 语义对齐（稳定前缀 + 边界校验 +
OPEN_TURN）。session 能力集完整：append/surface/replace/provenance/
持久化/恢复/fork。差异：无 parentSession 元数据（SessionLog 无 header）。
下一轮：真实 API https / 其余收尾。

## 66. M50 交付记录（2026）—— dsh-eval 可选链（?.）

**状态：补充交付**（`cargo test` 230 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS 可选链）

Cordis `!!js` 用完整 JS eval；Rust `dsh-eval` 是受限子集，此前不支持
`?.`（null 安全成员访问）——`config?.foo` 会 tokenize 为 `?` + `.` 报错。
JS 语义：`a?.b` 当 `a` 为 null/undefined 时返回 undefined（Rust: Null）
而非报错；缺失成员传播为 undefined（链式 `a?.b.c` 中 `a?.b` 缺失 → 继续
短路）。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- tokenizer：`?.` 两字符 token（`?.[` 也经此——`?` + `[` 分离，parser 处理）。
- `Expr::OptionalMember(base, key)` 变体；eval：
  - 基对象 null 或**未定义标识符**（不在 scope）→ 短路 Null；
  - 成员未命中（缺失键/越界）→ 传播 Null（链式可选链）；
  - 否则等价 `Member`。
- `member_access` 提取为共享辅助（Member/OptionalMember 共用）。
- parser postfix：`?.name` / `?.[expr]` 分支。

### 新增测试（1 项，`m3_eval.rs`）

`optional_chaining_null_safe`：非 null 等价成员访问；null/缺失成员/未定义
标识符短路 Null；数组索引 `?.[0]`；普通 `.` 在 null 上仍报错（fail loud）。

### 结论

`?.` 可选链与 JS 语义对齐（短路 + 链式传播 + fail loud 保留）。剩余
dsh-eval 差异：`in`/`typeof`/模板字符串等（子集边界，按需扩展）。
下一轮：真实 API https / 其余收尾。

## 67. M51 交付记录（2026）—— dsh-eval nullish coalescing（??）

**状态：补充交付**（`cargo test` 231 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS `??`）

M50 记录的可继续项：`??`（nullish coalescing）——仅当左侧为 null/undefined
时取右侧；与 `||` 的 truthiness 短路不同（`0 ?? 'x'` → 0，`0 || 'x'` →
'x'）。Rust Value 无 undefined——null 即短路条件。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- tokenizer：`??` 两字符 token。
- `binary_op` 加 `"??"` 分支：`a.is_null() ? b : a`。
- `parse_or`：`??` 与 `||` 同级循环（左结合，对齐 JS 优先级）。

### 新增测试（1 项，`m3_eval.rs`）

`nullish_coalescing`：null 左侧取右侧；0/''/false 保留左侧（与 `||` 对比）；
链式；与 `||` 同级左结合。

### 结论

`??` 与 JS 语义对齐（null-only 短路 + 优先级）。剩余 dsh-eval 差异：
`in`/`typeof`/模板字符串/`?.()` 可选调用（子集边界，按需扩展）。
下一轮：真实 API https / 其余收尾。

## 68. M52 交付记录（2026）—— CLI --patch 别名 + merge_entries 单测

**状态：补充交付**（`cargo test` 232 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-cli 参数别名 + 测试补全）。

### 目标（对齐生产 CLI 的 `--patch`）

生产 CLI：`dsh --patch <path>`（可重复，argv 顺序）——patch overlay：同 id
行**完整 config 替换** + 可插入新行。Rust CLI 已有 `--overlay`（语义等价），
缺 `--patch` 参数名（兼容性差异）。

### dsh-cli（`crates/dsh-cli/src/main.rs` + `lib.rs`）

- `main.rs`：`--patch <file>` 作为 `--overlay` 的别名（都 push 进 overlays）。
- `lib.rs`：`merge_entries` 新增 `#[cfg(test)]` 单测——同 id 完整 config 替换
  + 新 id 追加插入 + 未命中行保留（对齐生产 patch overlay 语义）。

### 新增测试（1 项，`lib.rs` 单元）

`merge_entries_replaces_config_and_inserts`：替换 loop 完整 config + 插入
extra entry + services 保留。

### 结论

CLI patch overlay 参数与生产对齐（`--patch` 别名 + 行级替换 + insert 语义
单测固化）。差异：无 `--dump-config`/`--help` 应用参数边界（`ctx.cmdlineArgs`
非 Rust 迁移范畴）。下一轮：真实 API https / 其余收尾。

## 69. M53 交付记录（2026）—— dsh-eval typeof 一元运算符

**状态：补充交付**（`cargo test` 233 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS `typeof`）

`!!js` 配置常用 `typeof config.x === 'string'` 守卫；Rust `dsh-eval` 此前
不支持 `typeof`（会被解析为标识符访问 → not in scope 报错）。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- `Expr::Typeof(Box<Expr>)` 变体；eval：null → "object"（JS 遗留）、
  bool/number/string/object 按 JSON 类型、**未定义标识符 → "undefined"**
  （Rust 无 undefined，经 not-in-scope 错误识别）。
- parser `parse_unary`：Ident "typeof" → `Expr::Typeof`（优先级高于二元）。

### 新增测试（1 项，`m3_eval.rs`）

`typeof_operator`：string/number/boolean/object/null（JS 遗留）→ "object"、
未定义 → "undefined"；与 `===` 组合（守卫模式）；优先级（`typeof x === 'n'
? 'yes' : 'no'`）。

### 结论

`typeof` 与 JS 语义对齐（类型字符串 + undefined 映射 + 优先级）。剩余
dsh-eval 差异：`in`/`?.()` 可选调用（子集边界，按需扩展）。
下一轮：真实 API https / 其余收尾。

## 70. M54 交付记录（2026）—— dsh-eval 模板字符串（${expr} 插值）

**状态：补充交付**（`cargo test` 234 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS 模板字符串）

`!!js` 配置常用反引号模板（`` `prefix ${config.k} suffix` ``）；Rust
`dsh-eval` 此前不支持（反引号 → unexpected character 报错）。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- tokenizer：反引号 → `Tok::Template`（保留原始文本含 `${...}`）。
- `Expr::Template(Vec<Expr>)` 变体；eval：文本段直接追加、表达式段经
  `value_str` 转字符串（JS 隐式 String()：null → "null"）。
- `parse_template(raw, scope)`：按 `${...}` 分割为段序列（文本段 →
  `Expr::Value`，表达式段 → 递归 `evaluate`——立即求值）。
- parser `parse_primary`：`Tok::Template` → `parse_template`。

### 新增测试（1 项，`m3_eval.rs`）

`template_strings`：纯字面量；单/多段插值；表达式（成员/算术/字符串拼接）；
数字转字符串；空插值。

### 结论

模板字符串与 JS 语义对齐（段拼接 + 隐式 String()）。剩余 dsh-eval 差异：
`?.()` 可选调用（子集边界，按需扩展）。下一轮：真实 API https /
其余收尾。

## 71. M55 交付记录（2026）—— dsh-eval in 运算符

**状态：补充交付**（`cargo test` 235 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS `in`）

`!!js` 配置常用 `'key' in obj` 守卫（如 `'provider' in config`）；Rust
`dsh-eval` 此前不支持（`in` 被解析为标识符 → not in scope 报错）。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- `binary_op` 加 `"in"` 分支：左侧为字符串键或数字索引，右侧对象 → 键
  存在性、数组 → 索引越界检查；右侧非对象/数组 → 报错（fail loud）；
  左侧非键 → 报错（JS `false in {}` 同样 TypeError）。
- `parse_comparison`：Ident "in" 作为关系运算符（与 `<`/`>` 同级）。

### 新增测试（1 项，`m3_eval.rs`）

`in_operator`：对象键存在性（true/false）；数组索引（'0' in [1,2] → true、
越界 false）；与 `&&` 组合（守卫模式）；非对象右侧/非键左侧报错。

### 结论

`in` 与 JS 语义对齐（键存在性 + 类型校验 + fail loud）。剩余 dsh-eval
差异：`?.()` 可选调用（子集边界，按需扩展）。下一轮：真实 API https /
其余收尾。

## 72. M56 交付记录（2026）—— CLI --dump-config（生效配置转储）

**状态：补充交付**（`cargo test` 236 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-cli 配置查看入口）。

### 目标（对齐生产 `dsh --dump-config`）

生产 CLI：`dsh --profile web --dump-config` 打印**生效配置树**（合并 overlay
后的 entries；不 boot app）。Rust CLI 此前只有 boot 路径。

### dsh-cli（`crates/dsh-cli/src/lib.rs` + `main.rs`）

- `dump_config(config, overlays) -> Result<String, CordisError>`：读主配置 +
  overlays 合并（同 id 覆盖 config、新 id 追加）→ 序列化 YAML；不 boot loop。
- `main.rs`：`--dump-config` 参数——boot 前打印生效配置后退出（0）；失败
  stderr + exit 1。

### 新增测试（1 项，`m9_boot.rs`）

`dump_config_merges_overlays`：overlay 替换 loop name + services 保留 +
输出是合法 YAML entries 列表。

### 结论

配置转储与生产对齐（合并 overlays + YAML 输出 + 不 boot）。差异：无
`--dump-default-config`（默认配置模板——Rust 无 bundle 模板机制）。
下一轮：真实 API https / 其余收尾。

## 73. M57 交付记录（2026）—— Schema.extend 自定义类型注册

**状态：补充交付**（`cargo test` 238 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-schema 扩展机制补全）。

### 目标（对齐 Schemastery `Schema.extend`）

生产 schemastery：`Schema.extend(type, resolve)` 注册**全局 resolver 表**
（`resolvers[type]`）；`Schema.resolve` 对未知 type 查表（未注册 →
`unsupported type`）。Rust `dsh-schema` 此前用 `SchemaKind` enum 静态匹配，
无运行时扩展点——插件无法注册自定义 schema 类型。

### dsh-schema（`crates/dsh-schema/src/lib.rs`）

- `SchemaKind::Custom(String)` 变体。
- 全局注册表：`OnceLock<Mutex<HashMap<String, CustomResolver>>>`（`CustomResolver
  = Arc<dyn Fn(&Value, &SchemaRef, &ResolveOptions) -> Result<Value,
  ValidationError> + Send + Sync>`——Mutex 要求 Send+Sync，`Arc` 而非 Rc）。
- `Schema::extend(type, resolver)`（注册）+ `Schema::custom(type)`（构造节点）。
- `resolve_kind` Custom 分支：查表（未注册 → `unsupported type` fail loud）。
- `schema_to_string` Custom 分支（type 名）。

### 新增测试（2 项，`m4_schema.rs`）

1. `schema_extend_custom_type`：注册 "duration"（数字 >= 0）——正/零通过、
   负值/非数字报错；未注册 type → unsupported。
2. `schema_extend_composes`：自定义类型参与 object 组合与 union 分支。

### 结论

`schema.extend` 自定义类型与 Schemastery 对齐（全局注册表 + resolve 查表 +
unsupported fail loud + 组合性）。dsh-schema 扩展机制补全。剩余差异：
`function`/`is(Class)` Value-land 本质限制。下一轮：真实 API https /
其余收尾。

## 74. M58 交付记录（2026）—— HMR 换 loop 组件（boot.refresh 重建插件）

**状态：补充交付**（`cargo test` 239 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-cli 的 HMR loop 重建）。

### 目标（对齐 Cordis loader 按名重解析插件）

Cordis loader 按 name 解析插件（运行时重解析——HMR 换 entry 的 name 即换
插件实现）；Rust boot 的 `WasmLoopPlugin` 实例在启动时构建（wasm 字节固定），
refresh 重挂载 entry（fiber 重启）但 `run_turn` 仍用旧插件实例——**config.wasm
指向不同组件时 HMR 不生效**（实质缺口）。

### dsh-cli（`crates/dsh-cli/src/lib.rs`）

- `Boot.loop_plugin` 改 `Rc<RefCell<Arc<WasmLoopPlugin>>>`（可变句柄）。
- refresh 闭包：重挂载（load_async 事务）后按合并后 loop entry 的
  `config.wasm` **重建 WasmLoopPlugin 并替换**（config.wasm 变化时新组件
  生效）。
- `run_turn` 经 `borrow()` 读当前插件。

### 新增测试（1 项，`m9_boot.rs`）

`boot_refresh_swaps_loop_component`：初始 echo-loop → refresh 改 config.wasm
为 tool-loop → run_turn 返回 summary（新组件生效）。

### 结论

HMR 换 loop 组件与 Cordis loader 重解析语义对齐（refresh 重建插件 +
run_turn 用新实例）。差异：boot 启动时仍一次性构建（无懒加载）；
`--watch` 场景经 HMR 生效。下一轮：真实 API https / 其余收尾。

## 75. M59 交付记录（2026）—— dsh-eval 可选调用（?.()）

**状态：补充交付**（`cargo test` 240 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-eval 表达式能力补全）。

### 目标（对齐 JS `?.()`）

JS `fn?.()`：callee 为 null/undefined 时**不调用**返回 undefined；否则正常
调用。Rust `dsh-eval` 此前 `?.` 后跟 `(` 报错（`?.` 分支只处理成员名/索引）。
dsh-eval 子集边界的最后一项。

### dsh-eval（`crates/dsh-eval/src/lib.rs`）

- `Expr::OptionalCall(Box<Expr>, Vec<Expr>)` 变体；eval：
  - callee 为白名单函数名（String/Number/Boolean）→ 直接调用（非 scope
    标识符但合法目标）；
  - callee 为未定义标识符 → 短路 Null；
  - callee 求值为 null（如 `config?.handler` 缺失）→ 短路 Null；
  - 否则等价普通调用。
- `eval_call` 提取为共享辅助（Call/OptionalCall 共用）。
- parser postfix：`?.(` 分支（`?.` 后跟 `(` → OptionalCall）。

### 新增测试（1 项，`m3_eval.rs`）

`optional_call_short_circuits`：白名单经 `?.()` 正常调用；缺失成员/未定义
标识符/基对象 null → 短路 Null；普通调用在缺失成员上仍报错。

### 结论

`?.()` 可选调用与 JS 语义对齐（短路 + 白名单直调 + fail loud 保留）。
dsh-eval 子集边界补齐（?. / ?? / typeof / 模板字符串 / in / ?.()）。
下一轮：真实 API https / 其余收尾。

## 76. M60 交付记录（2026）—— parallel_async 返回结果数组

**状态：补充交付**（`cargo test` 241 项全绿 + clippy 零警告；12 差分场景
不变——Cordis 层未动，仅 dsh-core async 分派补返回值）。

### 目标（对齐 Cordis `ctx.parallel` 的 Promise.all 结果数组）

Cordis `ctx.parallel` = `Promise.all(listeners.map(fn))` → **结果数组**（各
监听器返回值）；Rust `parallel_async` 此前丢弃结果（返回 `()`）——只报
错误。

### dsh-core（`crates/dsh-core/src/context.rs`）

- `parallel_async` 返回类型 `Result<(), AggregateError>` →
  `Result<Vec<Value>, AggregateError>`：成功返回各监听器返回值（`Continue`
  → null，`Returned(v)` → v）；错误仍聚合为 AggregateError（allSettled）。

### 新增测试（1 项，`m7_async.rs`）

`parallel_async_returns_result_values`：两个异步监听器（Continue + 返回值）
→ 结果数组 `[null, "b-val"]`（注册顺序）。

### 结论

`parallel_async` 与 Cordis `ctx.parallel` 语义对齐（Promise.all 结果数组 +
错误聚合）。现有调用方（loader 事务等）不受影响（`unwrap()` 兼容）。
下一轮：真实 API https / 其余收尾。

## 77. M61 交付记录（2026）—— disabled entry 差分场景（loader-11）

**状态：补充交付**（13 差分场景全 PASS——含新增 loader-11-disabled-entry；
`cargo test` 241 项不变）。

### 目标（差分覆盖 disabled entry）

Rust 侧 loader 的 `disabled`/`disabled_expr` 已实现（M3 单测）；差分场景
此前未覆盖 disabled entry（12 场景只有 group/inject）。补 loader 差分——
TS 侧 vendored loader 的 disabled 行为作参照。

### 差分（`scenarios/loader-11-disabled-entry.json` + `verify-diff.mjs`）

场景：loader-sync 含 disabled e2（不 apply）→ update e2 enabled（apply）→
update e1 disabled（卸载）。`verify-diff.mjs` 的 `ASYNC_SCENARIOS` 加入
loader-11。

### 验证

`loader-11-disabled-entry.golden`（15 行，TS 生成）与 Rust 侧逐行一致：
- 初始 sync：仅 e1 apply（e2 disabled 不 apply）；
- e2 → enabled：apply；
- e1 → disabled：Active→Unloading→Disposed。

### 结论

disabled entry 的 TS↔Rust 行为完全对齐（含 disabled→enabled 热更 +
enabled→disabled 卸载）。差分覆盖增强。剩余差分面：isolate/intercept
（Rust 单测已覆盖，TS 宿主需扩展）。下一轮：真实 API https / 其余收尾。

## 78. M62 交付记录（2026）—— isolate/intercept 差分场景（loader-12）

**状态：补充交付**（14 差分场景全 PASS——含新增 loader-12-isolate-intercept；
`cargo test` 241 项不变；`cargo clippy --all-targets` 零警告）。

### 目标（差分覆盖服务接线字段）

Rust 侧 `EntryOptions.isolate`/`intercept` 已实现（M3，loader.rs 注入
pending_isolate/pending_intercept）；但 `dsh_diff::to_entry_options` 只透传
id/name/config/disabled/group，差分场景里 entry 的 isolate/intercept 被静默
丢弃——与 TS 宿主 `{...e}` 原样透传不一致。补齐透传并纳入差分。

### 改动

1. `crates/dsh-diff/src/lib.rs`：`to_entry_options` 补 `opts.isolate`/
   `opts.intercept`（新增 `obj_map` 辅助，从 entry JSON 的 object 字段转
   `HashMap<String, Value>`）。
2. `scenarios/loader-12-isolate-intercept.json`：sync 含
   `isolate:{svc:true}`（入口本地 realm）与 `intercept:{svc:{tag:"x"}}` →
   update e2 切 `isolate:{svc:"global-a"}`（label）+ `intercept:{}` →
   update e1 切 `intercept:{svc:{tag:"y"}}`。
3. `diff/ts-host/verify-diff.mjs`：`ASYNC_SCENARIOS` 加入 loader-12。

### 验证

`loader-12-isolate-intercept.golden`（23 行，TS 生成）与 Rust 侧逐行一致：
- line 1：`loader-sync` 行含 isolate/intercept 字段（规范化键序）；
- line 12：update 切接线字段 → isolate/intercept 变化触发 e2 卸载重载；
- line 18：e1 切 intercept → 重载；
- fiber 轨迹（plugin/status/apply/log）与 TS 一致。

### 结论

服务接线字段（isolate/intercept）的 TS↔Rust 透传与事务稳定性逐行对齐。
注：trace 层（plugin/status/log）不体现服务 realm 实例差异，本场景验证
字段透传与事务稳定性；服务实例隔离语义由 Rust 单测覆盖。

## 79. M63 交付记录（2026）—— include 纯函数差分场景（include-01）

**状态：补充交付**（15 差分场景全 PASS——含新增 include-01-apply-patches-full；
`cargo test` 242 项全绿（+1 新增测试）；`cargo clippy --all-targets` 零警告）。

### 目标（include patch 差分覆盖）

include patch（`apply_entry_patches`，M33/M39）此前仅有 Rust 单测，差分集
未含 include 场景。本轮把 include 的**纯函数级**差分纳入：TS 侧以 vendored
`@deepseek-ai/cordis-plugin-include@1.0.6`（经 `node_modules` 装入）的
`applyEntryPatches(data, patches, warn)` 为权威，Rust 侧以
`apply_entry_patches_with_warn` 对比，覆盖 insert 进 group / 顶层追加 / 嵌套
命中 / 各 warn 诊断。

### 改动

1. `crates/dsh-loader/src/include.rs`：`Patch` 补 `Serialize, Deserialize`
   derive（JSON patch → 运行时 patch，承载差分场景）。
2. `crates/dsh-diff/src/lib.rs`：新增 `pub fn run_include(text)`——解析
   `{data, patches}` 场景，`apply_entry_patches_with_warn` 执行，输出到
   `include-data` / `include-warn` / `include-result` trace 行（Uniform
   `sorted_json` 按键字典序，与 TS canonical 对齐）。
3. `crates/dsh-diff/src/main.rs`：按场景顶层含 `patches` 键分发到 `run_include`
   （否则 Scenario Runner）。
4. `diff/ts-host/include-host.mjs`：include 场景宿主——用
   `applyEntryPatches` 权威执行，`warn` 回调做 printf `%C` 展开（无颜色 =
   原始字符串，与 Rust `format!` 逐字一致）。
5. `diff/ts-host/verify-diff.mjs`：`include-` 前缀分发到 include-host.mjs。
6. `scenarios/include-01-apply-patches-full.json` + `.golden`（7 行，TS 生成）。
7. `crates/dsh-diff/tests/m63_include_diff.rs`：单测断言 data/warn/result 形态。

### 差分协约（对齐细则）

- 场景 entries 与 insert 显式给**全字段**（含 `config:{}`）——两侧序列化一致
  （Rust `EntryOptions` 序列化补默认字段；TS canonical 输出原键）。
- 顶层 data/result 以 `EntryOptions` 视图（全字段），嵌套 group 子入口保持
  原始 `Value` 视角——Rust 的 group.config 是 `Value`，TS 的也是平 JSON。
- `disabled_expr`：输入无则两侧均不输出（Rust `skip_serializing_if`）。
- `%C` 展开（cordis `defaultFormatters.C` 无颜色）= 原始字符串：物理 `%s`。

### 验证

`include-01-apply-patches-full.golden`（7 行，TS 生成）与 Rust 侧逐行一致：
- `include-data`：输入 a/g（含组子入口 c1），按键序。
- 5 条 `include-warn` 按序：`entry ghost not found`、`entry a is not a
  group`、`entry nope not found`、`id is required for non-insert patches`、
  `name mismatch for a (expected a, got WRONG), skipping`。
- `include-result`：a.config={k:2}、a.disabled=true、顶层追加 x、
  g.config 插 c2、c1 嵌套命中 {nested:true}。

### 结论

include patch 的纯函数语义（insert 进 group / 顶层追加 / 嵌套命中 / warn
诊断）TS↔Rust 逐行对齐，差分场景 15 个全 PASS。**剩余真实缺口**：TS
`applyEntryPatches` 支持任意 entry 字段覆盖（`{id, insert, name, ...overrides}
→ target[key]=value`），Rust `patch_update` 仅覆盖 config/disabled/group——
通用 overrides 覆盖待补（下一轮候选）。

## 80. M64 交付记录（2026）—— include patch 通用 overrides 字段覆盖

**状态：补充交付**（16 差分场景全 PASS——含新增 include-02-apply-patches-
overrides；`cargo test` 245 项全绿（+3 新增 overrides 测试）；`cargo clippy
--all-targets` 零警告）。

### 目标（补齐 include patch 的通用字段覆盖语义）

M63 结论记录的**真实缺口**：TS `applyEntryPatches` 的 `{ id, insert, name,
...overrides }` 把除 id/insert/name 外的**所有** patch 字段逐一 `target[key]
= value` 覆盖到 entry；Rust `Patch`/`patch_update`（M33/M39）仅覆盖
config/disabled/group 三个显式字段。本轮引入 `overrides` 收集，使任意 entry
字段（inject/isolate/intercept/disabled_expr/...）的覆盖与 TS 对齐。

### 改动

1. `crates/dsh-loader/src/include.rs`：
   - `Patch` 加 `#[serde(flatten, default)] overrides: HashMap<String, Value>`
     ——显式字段（config/disabled/group）由具体字段消费，**其余任意键**在
     反序列化时进入该 map；序列化时 flatten 并回同一对象（与 TS patch 对象
     形态一致）。
   - 新增 `apply_entry_override(&mut EntryOptions, key: &str, value)`：按
     `EntryOptions` 字段名匹配——`inject`（`Vec<String>` 整体替换）、
     `isolate`/`intercept`（对象整体替换）、`disabled_expr`（转 `String`）、
     `config`/`disabled`/`group`（与显式路径重复但无损）；类型不符则忽略
     （EntryOptions 字段类型固定，TS 宽松赋值在 Rust 处收紧）。
   - `apply_entry_patches_with_warn` 的 `patch_update` 闭包在显式字段后遍历
     `patch.overrides` 应用（合并后 = TS 的完整 overrides）。
2. `scenarios/include-02-apply-patches-overrides.json` + `.golden`（2 行，TS 生成）。
3. `crates/dsh-loader/tests/m3_include.rs`：3 项新增测试——
   `apply_patches_overrides_any_entry_field`（inject/isolate/intercept/
   disabled_expr 覆盖）、`patch_deserializes_flatten_overrides_and_roundtrips`
   （JSON → overrides 收集 + round-trip 并回）、`apply_patches_overrides_
   nested_group_child`（递归到 group 子入口）。

### 差分协约（M63 扩展）

- overrides 差分的 trace 观察面与 M63 一致：顶层 entry/insert 全字段、嵌套
  group 子入口保持原始 Value 视角、`disabled_expr` 输入无则两侧均不输出。
- `%C` 展开（cordis `defaultFormatters.C` 无颜色 = 原始字符串）逐字一致。
- TS 侧无需改动——`applyEntryPatches` 天然支持任意 overrides 字段。

### 验证

`include-02-apply-patches-overrides.golden`（2 行，TS 生成）与 Rust 逐行一致：
- patch `{id:p, inject:[svc1,svc2], isolate:{svc3:true}, intercept:{svc4:
  {tag:'x'}}}` → p.inject 整体替换、isolate/intercept 对象替换；
- `{id:p, disabled_expr}` → p 新增 disabled_expr:"expr"；
- `{id:p, isolate:{svc3:'global-a'}}` → isolate 整体替换；
- `{id:p, inject:[]}` → inject 清空。
- include-data → 2 行含 p/q；include-result 反映全部覆盖。

### 结论

include patch 的通用字段覆盖语义与 TS `applyEntryPatches` 完全对齐（serde
flatten overrides + 逐字段收紧），差分场景 16 个全 PASS。至此 include 的
insert / 嵌套命中 / warn 诊断 / 通用字段覆盖四方面 TS↔Rust 逐行一致。

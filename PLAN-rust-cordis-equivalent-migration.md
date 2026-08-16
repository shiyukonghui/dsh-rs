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

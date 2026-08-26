// TS 侧场景宿主：读取场景 DSL JSON，用 npm 原版 cordis 执行并输出规范化 trace。
//
// trace 行格式与 Rust 侧 dsh-diff 一致：
// - `plugin:{name}` / `status:{name}:{old}:{new}`（框架层，宿主监听 internal/*）
// - `apply:{name}` / `log:{text}` / `effect-reg:{text}` / `dispose:{text}`
//   / `on:{event}:{log}` / `on-return:{event}:{log}` / `provide:{service}:{json}`
//   / `intercept:{service}:{json}`（解释器层）
// - `emit:{event}` / `serial:{event}` / `bail:{event}` / `waterfall:{event}`
//   / `serial-result:{json}` / `bail-result:{json}` / `waterfall-result:{json}`（宿主层）

import { Context, FiberState } from 'cordis'
import { readFileSync } from 'node:fs'

const FIBER_STATE_NAMES = {
  [FiberState.PENDING]: 'Pending',
  [FiberState.LOADING]: 'Loading',
  [FiberState.ACTIVE]: 'Active',
  [FiberState.FAILED]: 'Failed',
  [FiberState.DISPOSED]: 'Disposed',
  [FiberState.UNLOADING]: 'Unloading',
}

function buildPlugin(desc, plugins, trace) {
  const plugin = function (ctx, config) {
    trace.push(`apply:${desc.name}`)
    const disposers = []
    for (const op of desc.apply) {
      applyOp(ctx, op, config, plugins, trace, disposers)
    }
  }
  Object.defineProperty(plugin, 'name', { value: desc.name, configurable: true })
  if (desc.inject?.length) plugin.inject = desc.inject
  return plugin
}

function applyOp(ctx, op, config, plugins, trace, disposers) {
  switch (op.op) {
    case 'log':
      trace.push(`log:${op.text}`)
      break
    case 'log-config':
      trace.push(`log-config:${JSON.stringify(config)}`)
      break
    case 'effect':
      trace.push(`effect-reg:${op.dispose}`)
      disposers.push(ctx.effect(() => () => trace.push(`dispose:${op.dispose}`)))
      break
    case 'dispose-effect':
      if (disposers[op.index]) disposers[op.index]()
      break
    case 'on':
      trace.push(`on:${op.event}:${op.log}`)
      ctx.on(op.event, () => { trace.push(`log:${op.log}`) })
      break
    case 'on-prepend':
      trace.push(`on-prepend:${op.event}:${op.log}`)
      ctx.on(op.event, () => { trace.push(`log:${op.log}`) }, { prepend: true })
      break
    case 'on-return':
      trace.push(`on-return:${op.event}:${op.log}`)
      ctx.on(op.event, () => {
        trace.push(`log:${op.log}`)
        return op.value
      })
      break
    case 'on-waterfall':
      trace.push(`on-waterfall:${op.event}:${op.log}`)
      ctx.on(op.event, (...args) => {
        const next = args.pop()
        trace.push(`log:${op.log}`)
        const result = next(...args)
        trace.push(`log:${op.after}`)
        return result
      })
      break
    case 'on-short':
      trace.push(`on-short:${op.event}:${op.log}`)
      ctx.on(op.event, () => { trace.push(`log:${op.log}`) })
      break
    case 'provide':
      trace.push(`provide:${op.service}:${JSON.stringify(op.value)}`)
      // check === false → 可用性谓词不成立（依赖方 PENDING）；providers 的 disposer
      // 可被 `dispose-effect` 定向（A4a：unprovide 而 fiber 不卸载）。
      disposers.push(ctx.provide(op.service, op.value, op.check === false ? () => false : undefined))
      break
    case 'intercept': {
      trace.push(`intercept:${op.service}:${JSON.stringify(op.config)}`)
      // 在当前 fiber 的 ctx 上创建一层 intercept（子层继承父层，own 条目本层可见）
      const layer = Object.create(ctx[Context.intercept])
      layer[op.service] = op.config
      ctx[Context.intercept] = layer
      break
    }
    case 'resolve-config': {
      const merged = {}
      let intercept = ctx[Context.intercept]
      const configs = []
      while (op.service in intercept) {
        if (Object.hasOwn(intercept, op.service)) configs.unshift(intercept[op.service])
        intercept = Object.getPrototypeOf(intercept)
      }
      Object.assign(merged, ...configs)
      trace.push(`resolve-config:${op.service}:${JSON.stringify(merged)}`)
      break
    }
    case 'plugin':
      ctx.plugin(buildPlugin(plugins[op.id], plugins, trace), {})
      break
    default:
      throw new Error(`unknown apply op ${op.op}`)
  }
}

async function runScenario(scenario) {
  const trace = []
  const ctx = new Context()
  const fibers = new Map()

  // 框架层 trace：internal/plugin（创建）+ internal/status
  ctx.on('internal/plugin', (fiber) => {
    if (fiber.uid) trace.push(`plugin:${fiber.name}`)
  })
  ctx.on('internal/status', (fiber, oldValue) => {
    trace.push(`status:${fiber.name}:${FIBER_STATE_NAMES[oldValue]}:${FIBER_STATE_NAMES[fiber.state]}`)
  })

  for (const step of scenario.steps) {
    switch (step.op) {
      case 'plugin':
      case 'plugin-with-config': {
        const desc = scenario.plugins[step.id]
        const config = step.config ?? {}
        const fiber = await ctx.plugin(buildPlugin(desc, scenario.plugins, trace), config)
        fibers.set(step.id, fiber)
        break
      }
      case 'emit':
        trace.push(`emit:${step.event}`)
        ctx.emit(step.event, ...(step.args ?? []))
        break
      case 'serial': {
        trace.push(`serial:${step.event}`)
        const result = await ctx.serial(step.event, ...(step.args ?? []))
        if (result !== undefined) trace.push(`serial-result:${JSON.stringify(result)}`)
        break
      }
      case 'bail': {
        trace.push(`bail:${step.event}`)
        const result = ctx.bail(step.event, ...(step.args ?? []))
        if (result !== undefined) trace.push(`bail-result:${JSON.stringify(result)}`)
        break
      }
      case 'waterfall': {
        trace.push(`waterfall:${step.event}`)
        const result = ctx.waterfall(step.event, ...(step.args ?? []), () => null)
        if (result !== undefined) trace.push(`waterfall-result:${JSON.stringify(result)}`)
        break
      }
      case 'unload': {
        const fiber = fibers.get(step.id)
        if (!fiber) throw new Error(`unknown plugin ${step.id}`)
        await fiber.dispose()
        break
      }
      case 'update': {
        const fiber = fibers.get(step.id)
        if (!fiber) throw new Error(`unknown plugin ${step.id}`)
        // fiber.update() 不返回 restart promise（cordis 4.x），需再 await 等待重载完成
        await fiber.update(step.config)
        await fiber.await()
        break
      }
      default:
        throw new Error(`unknown step ${step.op}`)
    }
  }
  return trace
}

const path = process.argv[2]
const scenario = JSON.parse(readFileSync(path, 'utf8'))
const trace = await runScenario(scenario)
for (const line of trace) console.log(line)

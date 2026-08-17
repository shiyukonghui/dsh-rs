// TS 侧 loader 场景宿主（M20）：用 vendored `@deepseek-ai/cordis-plugin-loader`
// 的 EntryTree/EntryGroup 事务语义执行 loader 场景，输出规范化 trace。
//
// trace 行格式（与 Rust 侧 dsh-diff loader 场景一致）：
// - `loader-sync:{json(entries)}`（宿主层：执行 sync 事务）
// - `plugin:{name}` / `status:{name}:{old}:{new}`（框架层，internal/*）
// - `apply:{name}` / `log:{text}`（解释器层，插件 body）
// - `loader-error:{json}`（事务失败：AggregateError 的 errors 序列化）
//
// 场景 DSL（JSON）：
// {
//   "name": "...",
//   "plugins": { "<id>": { "name": "...", "apply": [{ "op": "log", "text": "..." }] } },
//   "steps": [
//     { "op": "sync", "entries": [{ "id": "e1", "name": "<plugin-id>", "config": {...} }] },
//     { "op": "create", "options": {...} },
//     { "op": "update", "id": "e1", "options": {...} },
//     { "op": "remove", "id": "e1" }
//   ]
// }

import { Context } from '@deepseek-ai/cordis'
import { Loader, Group } from '@deepseek-ai/cordis-plugin-loader'
import { readFileSync } from 'node:fs'

// vendored cordis 的 FiberState 是数字常量（无具名导出）：
// 0=Pending 1=Loading 2=Active 3=Failed 4=Disposed 5=Unloading
const FIBER_STATE_NAMES = ['Pending', 'Loading', 'Active', 'Failed', 'Disposed', 'Unloading']

function buildPlugin(desc, trace) {
  const plugin = function (ctx, config) {
    trace.push(`apply:${desc.name}`)
    for (const op of desc.apply ?? []) {
      switch (op.op) {
        case 'log':
          trace.push(`log:${op.text}`)
          break
        case 'log-config':
          trace.push(`log-config:${JSON.stringify(config)}`)
          break
        default:
          throw new Error(`unknown apply op ${op.op}`)
      }
    }
  }
  Object.defineProperty(plugin, 'name', { value: desc.name, configurable: true })
  if (desc.inject?.length) plugin.inject = desc.inject
  return plugin
}

function normalizeError(e) {
  // AggregateError → errors 列表；Error → 单条。差分只取失败数量
  // （两边错误消息文本不同，数量语义一致即可）。
  if (e && typeof e === 'object' && Array.isArray(e.errors)) return e.errors.length
  return 1
}

function deepClone(v) {
  return v === undefined ? undefined : JSON.parse(JSON.stringify(v))
}

// 规范化 JSON（递归按键字典序排序）——与 Rust serde_json 默认 BTreeMap 排序一致。
function canonical(v) {
  if (Array.isArray(v)) return v.map(canonical)
  if (v && typeof v === 'object') {
    const out = {}
    for (const k of Object.keys(v).sort()) out[k] = canonical(v[k])
    return out
  }
  return v
}

// 递归映射 entry name → `cordis:<id>`（顶层 + group config 嵌套子入口）。
function mapEntryName(e, plugins) {
  const opts = { ...e }
  if (opts.name === 'group' || opts.name in plugins) opts.name = `cordis:${opts.name}`
  if (Array.isArray(opts.config)) opts.config = opts.config.map((c) => mapEntryName(c, plugins))
  return opts
}

async function runScenario(scenario) {
  const trace = []
  const ctx = new Context()
  // DSH 同款挂载：Loader 是插件（ctx.plugin(Loader) → ctx.loader 服务 +
  // isolate/intercept 钩子 + internal/* 写回）。builtins 注册内置插件。
  await ctx.plugin(Loader)

  // 框架层 trace：internal/plugin + internal/status
  ctx.on('internal/plugin', (fiber) => {
    if (fiber.uid) trace.push(`plugin:${fiber.name}`)
  })
  ctx.on('internal/status', (fiber, oldValue) => {
    trace.push(`status:${fiber.name}:${FIBER_STATE_NAMES[oldValue]}:${FIBER_STATE_NAMES[fiber.state]}`)
  })

  // 注册内置插件（cordis:builtin 命名空间；entry 的 name 用 `cordis:<id>`）
  ctx.loader.builtins.group = Group
  for (const [id, desc] of Object.entries(scenario.plugins ?? {})) {
    ctx.loader.builtins[id] = buildPlugin(desc, trace)
  }
  const loader = ctx.loader

  for (const step of scenario.steps) {
    switch (step.op) {
      case 'loader-sync': {
        // entry name `cordis:<id>` 指向 builtins；缺省 name 原样
        const raw = (step.entries ?? []).map((e) => deepClone(e))
        const entries = raw.map((e) => mapEntryName(e, scenario.plugins ?? {}))
        trace.push(`loader-sync:${JSON.stringify(canonical(raw))}`)
        try {
          await loader.root.update(entries)
        } catch (e) {
          trace.push(`loader-error:${JSON.stringify(normalizeError(e))}`)
        }
        break
      }
      case 'loader-create': {
        const raw = deepClone(step.options)
        const opts = { ...raw }
        if (opts.name in (scenario.plugins ?? {})) opts.name = `cordis:${opts.name}`
        trace.push(`loader-create:${JSON.stringify(canonical(raw))}`)
        try {
          await loader.root.create(opts)
        } catch (e) {
          trace.push(`loader-error:${JSON.stringify(normalizeError(e))}`)
        }
        break
      }
      case 'loader-update': {
        const raw = deepClone(step.options)
        const opts = { ...raw }
        if (opts.name in (scenario.plugins ?? {})) opts.name = `cordis:${opts.name}`
        trace.push(`loader-update:${step.id}:${JSON.stringify(canonical(raw))}`)
        try {
          await loader.update(step.id, opts)
        } catch (e) {
          trace.push(`loader-error:${JSON.stringify(normalizeError(e))}`)
        }
        break
      }
      case 'loader-remove': {
        trace.push(`loader-remove:${step.id}`)
        try {
          await loader.remove(step.id)
        } catch (e) {
          trace.push(`loader-error:${JSON.stringify(normalizeError(e))}`)
        }
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

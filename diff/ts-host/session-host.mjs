// M6 step10（D-090）：session 差分宿主——TS 侧镜像 dsh-session 会话事件契约，
// 输出规范化 trace（与 Rust 侧 dsh-diff 的 session 场景步骤逐字节一致）。
//
// 契约（与 Rust 侧实现共享）：
// - `session-create:{id}`：创建空会话（会话事件 seq = 事件数，首 append = 0）。
// - `session-append:{id}:{seq}:{kind}`：append 一行（kind = 事件 kind 名）。
//   - `surface: "append"` → surface-eligible 事件附加 surfaceOp（与 dsh-session
//     `SurfaceIntent{Append}` 要求一致；marker 不进 trace）。
// - `session-event-read:{id}:{seq}:{kind}:{canonical-json}`：回读，data 为
//   **canonical（按键字典序）** JSON——与 serde_json 默认 BTreeMap 序一致。
//   数值限定整数（serde_json/JSON.stringify 整数格式一致；浮点不做对齐面）。

import { readFileSync } from 'node:fs'

// canonical：递归按键字典序序列化（对齐 serde_json 默认 BTreeMap 序）。
function canonicalStringify(value) {
  if (Array.isArray(value)) {
    return '[' + value.map(canonicalStringify).join(',') + ']'
  }
  if (value !== null && typeof value === 'object') {
    const keys = Object.keys(value).sort()
    return '{' + keys
      .map((k) => JSON.stringify(k) + ':' + canonicalStringify(value[k]))
      .join(',') + '}'
  }
  return JSON.stringify(value)
}

const SURFACE_KINDS = new Set(['user/message', 'assistant/message', 'tool/result'])

function runScenario(scenario) {
  const trace = []
  /** @type {Record<string, {seq: number, kind: string, data: any}[]>} */
  const sessions = {}

  for (const step of scenario.steps) {
    switch (step.op) {
      case 'session-create': {
        sessions[step.id] = []
        trace.push(`session-create:${step.id}`)
        break
      }
      case 'session-append': {
        const events = sessions[step.id]
        if (!events) throw new Error(`session ${step.id} not created`)
        const seq = events.length
        const event = { seq, kind: step.kind, data: step.data }
        if (step.surface === 'append') {
          if (!SURFACE_KINDS.has(step.kind)) {
            throw new Error(`surface marker on non-surface kind ${step.kind}`)
          }
          event.surfaceOp = 'append'
        } else if (SURFACE_KINDS.has(step.kind)) {
          throw new Error(`surface-eligible kind ${step.kind} requires surface: "append"`)
        }
        events.push(event)
        trace.push(`session-append:${step.id}:${seq}:${step.kind}`)
        break
      }
      case 'session-events': {
        const events = sessions[step.id]
        if (!events) throw new Error(`session ${step.id} not created`)
        for (const ev of events) {
          trace.push(
            `session-event-read:${step.id}:${ev.seq}:${ev.kind}:${canonicalStringify(ev.data)}`
          )
        }
        break
      }
      default:
        throw new Error(`unknown session step ${step.op}`)
    }
  }
  return trace
}

const path = process.argv[2]
const scenario = JSON.parse(readFileSync(path, 'utf8'))
const trace = runScenario(scenario)
for (const line of trace) console.log(line)

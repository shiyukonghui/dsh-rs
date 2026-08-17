// TS 侧 include 场景宿主（M63）：用 vendored `@deepseek-ai/cordis-plugin-include`
// 的 `applyEntryPatches` 纯函数执行 include patch，输出规范化 trace。
//
// trace 行格式（与 Rust 侧 dsh-diff include 场景一致）：
// - `include-data:{json(data)}`    （宿主层：初始 entry 列表）
// - `include-warn:{message}`        （每条 warn，按序；`%C` 展开为原始字符串）
// - `include-result:{json(out)}`    （最终 entry 列表）
//
// 场景 DSL（JSON）：
// {
//   "name": "...",
//   "data": [ { "id": "a", "name": "a", ... }, ... ],
//   "patches": [
//     { "id": "a", "config": {...} },
//     { "insert": [{ ... }] },
//     { "id": "g", "insert": [{ ... }], "name": "g" }
//   ]
// }

import { applyEntryPatches } from '@deepseek-ai/cordis-plugin-include'
import { readFileSync } from 'node:fs'

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

// printf `%C` → 原始字符串（cordis defaultFormatters.C 无颜色时 = String(value)），
// 与 Rust `format!("{value}")` 逐字一致。
function printfC(message, ...args) {
  let i = 0
  return message.replace(/%C/g, () => String(args[i++]))
}

async function main() {
  const scenario = JSON.parse(readFileSync(process.argv[2], 'utf8'))
  const warns = []
  const out = applyEntryPatches(
    scenario.data,
    scenario.patches,
    (message, ...args) => warns.push(printfC(message, ...args)),
  )
  const lines = []
  lines.push(`include-data:${JSON.stringify(canonical(scenario.data))}`)
  for (const w of warns) lines.push(`include-warn:${w}`)
  lines.push(`include-result:${JSON.stringify(canonical(out))}`)
  process.stdout.write(lines.join('\n') + '\n')
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})

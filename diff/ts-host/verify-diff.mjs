// Verify all scenarios: TS side generates goldens, Rust side checks them.
// Usage: node verify-diff.mjs [scenario-dir] [dsh-diff-binary]
import { readdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const here = dirname(fileURLToPath(import.meta.url))
const dir = process.argv[2] ?? join(here, '..', '..', 'scenarios')
const diffBin = process.argv[3] ?? join(here, '..', '..', 'target', 'debug', 'dsh-diff.exe')
// M7：深嵌套（3 层以上）场景的微任务交错需用 async 编排（真实 yield_now）才与 TS 一致；
// 其余场景走同步路径（两阶段延迟）。
// M20：loader 场景（事务 allSettled + 并行 create）必须走 async 路径，且 golden
// 由 loader-host.mjs 生成（vendored @deepseek-ai/cordis-plugin-loader 为参照）。
// M28：group 场景（loader-10）——Group apply 异步化 + 卸载并行让出后逐行一致。
const ASYNC_SCENARIOS = new Set([
  '09-deep-nesting-3-levels',
  'loader-01-sync-success',
  'loader-02-partial-failure-rollback',
  'loader-10-group-nested',
  'loader-11-disabled-entry',
  'loader-12-isolate-intercept',
])
const scenarios = readdirSync(dir).filter((f) => f.endsWith('.json')).sort()
let failed = 0
for (const file of scenarios) {
  const base = file.replace(/\.json$/, '')
  const scenarioPath = join(dir, file)
  const isLoader = base.startsWith('loader-')
  const isInclude = base.startsWith('include-')
  const isSession = base.startsWith('session-')
  const host = isSession ? 'session-host.mjs' : (isInclude ? 'include-host.mjs' : (isLoader ? 'loader-host.mjs' : 'scenario-host.mjs'))
  const ts = spawnSync(process.execPath, [host, scenarioPath], {
    encoding: 'utf8',
    cwd: here,
  })
  if (ts.status !== 0) {
    console.error('TS FAIL ' + file + ': ' + (ts.stderr || ts.error))
    failed += 1
    continue
  }
  const goldenPath = join(dir, base + '.golden')
  // 去除 BOM（PowerShell 重定向可能写入）
  const cleaned = ts.stdout.replace(/^\uFEFF/, '')
  writeFileSync(goldenPath, cleaned)
  const flags = ASYNC_SCENARIOS.has(base) ? ['--async'] : []
  const rust = spawnSync(diffBin, [scenarioPath, '--golden', goldenPath, ...flags], { encoding: 'utf8' })
  process.stdout.write(rust.stdout)
  if (rust.status !== 0) {
    process.stdout.write(rust.stderr)
    failed += 1
  }
}
if (failed) {
  console.error(`${failed} scenario(s) FAILED`)
  process.exit(1)
}
console.log('ALL SCENARIOS PASS')

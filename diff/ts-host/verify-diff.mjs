// Verify all scenarios: TS side generates goldens, Rust side checks them.
// Usage: node verify-diff.mjs [scenario-dir] [dsh-diff-binary]
import { readdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'

const dir = process.argv[2] ?? join('..', '..', 'scenarios')
const diffBin = process.argv[3] ?? join('..', '..', 'target', 'debug', 'dsh-diff.exe')
const scenarios = readdirSync(dir).filter((f) => f.endsWith('.json')).sort()
let failed = 0
for (const file of scenarios) {
  const base = file.replace(/\.json$/, '')
  const scenarioPath = join(dir, file)
  const ts = spawnSync('node', ['scenario-host.mjs', scenarioPath], { encoding: 'utf8' })
  if (ts.status !== 0) {
    console.error('TS FAIL ' + file + ': ' + ts.stderr)
    failed += 1
    continue
  }
  const goldenPath = join(dir, base + '.golden')
  writeFileSync(goldenPath, ts.stdout)
  const rust = spawnSync(diffBin, [scenarioPath, '--golden', goldenPath], { encoding: 'utf8' })
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

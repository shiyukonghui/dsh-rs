// Generate golden traces (TS side) for every scenario JSON.
import { readdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'

const dir = process.argv[2] ?? join('..', 'scenarios')
const scenarios = readdirSync(dir).filter((f) => f.endsWith('.json')).sort()
for (const file of scenarios) {
  const base = file.replace(/\.json$/, '')
  const ts = spawnSync('node', ['scenario-host.mjs', join(dir, file)], { encoding: 'utf8' })
  if (ts.status !== 0) {
    console.error('TS FAIL ' + file + ': ' + ts.stderr)
    process.exitCode = 1
    continue
  }
  const golden = join(dir, base + '.golden')
  writeFileSync(golden, ts.stdout)
  console.log('golden written: ' + golden)
}

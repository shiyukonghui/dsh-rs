# D-A (decision A: copy + self-host). Copy the 4 vendored DeepSeek Harness shipped
# agent presets into resources/agent-presets/ and faithfully translate the TS-only
# `!!js` YAML tag syntax:
#   1. `disabled: !!js <expr>`      -> `disabled_expr: "<expr>"`
#       (dsh-loader EntryOptions.disabled_expr; M3 diff, include.rs:6 convention)
#   2. config value `cwd: !!js <expr>` -> `cwd: {"__jsExpr": "<expr>"}`
#       (evaluated recursively by dsh_eval::interpolate)
#   3. config array item `- !!js "<expr>"` -> `- {"__jsExpr": "<expr>"}`
# Syntax-only (faithful semantics); NO win32 capability rewrite yet - that waits on
# the section 6.1-2 A/B decision. preset.yml is copied byte-for-byte.
# Usage (from repo root): powershell -NoProfile -ExecutionPolicy Bypass -File tools\translate-agent-presets.ps1
# Verify: grep 'disabled_expr|__jsExpr' resources/agent-presets/*/agent.cordis.yml => 12 nodes.

$ErrorActionPreference = 'Stop'

# Resolve paths robustly under both -File and wrapped command invocations.
$scriptDir = $PSScriptRoot
if (-not $scriptDir) { $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition }
$root = Split-Path -Parent $scriptDir
$srcBase = Join-Path $root 'deepseek-harness\apps\cli\config\agent-presets'
$dstBase = Join-Path $root 'resources\agent-presets'
$ids = @('minimal', 'standard', 'code', 'cordis')

function Translate-AgentCordis {
    param([string[]]$Lines)
    $out = [System.Collections.Generic.List[string]]::new()
    $count = 0
    foreach ($raw in $Lines) {
        $line = $raw.TrimEnd("`r")
        # Rule 1: disabled: !!js <expr> -> disabled_expr: "<expr>"
        if ($line -match '^(\s*)disabled:\s*!!js\s+(.+?)\s*$') {
            $indent = $Matches[1]; $expr = $Matches[2]
            $out.Add("$indent`disabled_expr: `"$expr`"")
            $count++
            continue
        }
        # Rule 2: config value cwd: !!js <expr> -> cwd: {"__jsExpr": "<expr>"}
        if ($line -match '^(\s*)cwd:\s*!!js\s+(.+?)\s*$') {
            $indent = $Matches[1]; $expr = $Matches[2]
            $out.Add("$indent`cwd: {`"__jsExpr`": `"$expr`"}")
            $count++
            continue
        }
        # Rule 3: array item - !!js "<expr>" -> - {"__jsExpr": "<expr>"}
        if ($line -match '^(\s*)-+\s*!!js\s+"(.+)"\s*$') {
            $indent = $Matches[1]; $expr = $Matches[2]
            $out.Add("$indent- {`"__jsExpr`": `"$expr`"}")
            $count++
            continue
        }
        $out.Add($raw)
    }
    return [pscustomobject]@{ Lines = $out; Count = $count }
}

$total = 0
foreach ($id in $ids) {
    $src = Join-Path $srcBase $id
    $dst = Join-Path $dstBase $id
    New-Item -ItemType Directory -Force -Path $dst | Out-Null

    $cordisSrc = Join-Path $src 'agent.cordis.yml'
    $cordisDst = Join-Path $dst 'agent.cordis.yml'
    $lines = Get-Content -LiteralPath $cordisSrc -Encoding UTF8
    $res = Translate-AgentCordis -Lines $lines
    $translated = $res.Lines
    $n = $res.Count
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllLines($cordisDst, $translated, $utf8NoBom)

    # preset.yml: byte-for-byte copy
    $presetSrc = Join-Path $src 'preset.yml'
    $presetDst = Join-Path $dst 'preset.yml'
    if (Test-Path -LiteralPath $presetSrc) {
        Copy-Item -LiteralPath $presetSrc -Destination $presetDst -Force
    }

    # code preset: copy skills dir if present
    $skillsSrc = Join-Path $src 'skills'
    if (Test-Path -LiteralPath $skillsSrc) {
        Copy-Item -LiteralPath $skillsSrc -Destination $dst -Recurse -Force
    }

    Write-Output ("  {0}: translated {1} !!js node(s)" -f $id, $n)
    $total += $n
}
Write-Output ("D-A copy done: {0} !!js nodes translated in total" -f $total)

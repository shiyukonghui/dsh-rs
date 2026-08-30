# canvas-shell 构建配方（D-210）：Rust→浏览器三件套。
# 前置：rustup target add wasm32-unknown-unknown；
#       wasm-bindgen-cli 版本必须 = Cargo.lock 锁定的 wasm-bindgen（不匹配会拒绝输入）。
#       二进制获取（不编译）：GitHub releases wasm-bindgen/wasm-bindgen 的 *.tar.gz
#       （注意是 tar.gz 非 zip；仓库已从 rustwasm 迁至 wasm-bindgen 组织）。
$ErrorActionPreference = "Stop"
Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue
$lock = Get-Content "$PSScriptRoot\Cargo.lock"
$ver = $null
for ($i = 0; $i -lt $lock.Count; $i++) { if ($lock[$i] -eq 'name = "wasm-bindgen"') { $ver = ($lock[$i+1] -split '"')[1]; break } }
"locked wasm-bindgen = $ver"
if (-not $env:WBG) { $env:WBG = "$env:TEMP\wasm-bindgen-$ver-x86_64-pc-windows-msvc\wasm-bindgen.exe" }
if (-not (Test-Path $env:WBG)) { throw "wasm-bindgen CLI 不存在：$env:WBG（设 WBG 环境变量或按头部注释获取）" }
& $env:WBG --version | Select-Object -First 1
cargo build --manifest-path "$PSScriptRoot\Cargo.toml" --target wasm32-unknown-unknown --release
& $env:WBG --target web --out-dir "$PSScriptRoot\dist" "$PSScriptRoot\target\wasm32-unknown-unknown\release\canvas-shell.wasm"
# 可选体积优化（D-210 第 54 轮）：binaryen wasm-opt -Oz，实测 1.37MB→0.89MB（-32%）且
# 全量交互审计零回归（e2e-audit 全绿）。获取：GitHub WebAssembly/binaryen releases 的
# x86_64-windows.tar.gz（当前 windows 只发 tar.gz 非 zip；用 tar -xzf 解），设 WAO 指 wasm-opt.exe。
$wasm = "$PSScriptRoot\dist\canvas-shell_bg.wasm"
if (-not $env:WAO) { $env:WAO = "$env:TEMP\binaryen-132\binaryen-version_132\bin\wasm-opt.exe" }
if (Test-Path $env:WAO) {
    $before = (Get-Item $wasm).Length
    & $env:WAO $wasm -Oz --shrink-level=2 --enable-mutable-globals --enable-sign-ext `
        --enable-nontrapping-float-to-int --enable-bulk-memory --enable-reference-types --enable-multivalue `
        -o "$wasm.tmp"
    if ($LASTEXITCODE -eq 0 -and (Test-Path "$wasm.tmp")) {
        Move-Item "$wasm.tmp" $wasm -Force
        "wasm-opt: $before -> $((Get-Item $wasm).Length) 字节"
    } else { Remove-Item "$wasm.tmp" -ErrorAction SilentlyContinue; "wasm-opt 失败（保留未优化件）" }
} else { "跳过 wasm-opt（WAO 未就绪：$env:WAO）" }
Get-ChildItem "$PSScriptRoot\dist" | Select-Object Name, Length

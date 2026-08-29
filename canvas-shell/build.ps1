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
Get-ChildItem "$PSScriptRoot\dist" | Select-Object Name, Length

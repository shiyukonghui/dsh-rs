# M5 依赖安装清单（D-054）

> **背景（已实测确认）**：本机网络真实可达 rsproxy / crates.io 镜像（Node、Python 直连
> rsproxy.cn 均 HTTP 200）。此前 cargo 拉取失败是**受限执行沙箱的假故障**——沙箱把
> Windows Schannel 的凭据/证书存储剥夺，导致凡走 Schannel 的传输（cargo / git /
> PowerShell）报 `SEC_E_NO_CREDENTIALS (0x8009030E)`；Node/Python 因用自带 TLS 栈不受
> 影响。且沙箱内 cargo registry 缓存目录不可写，故**需在普通（非受限于沙箱）终端执行
> 下面命令一次**，把缺失/未提取的 crate 落进 `~/.cargo`，之后我这边 `--offline` 即可用。
>
> 已存在的（无需操作）：`jiff` 全家、`globset` 0.4.18、`ignore` 0.4.26、`which` 6.0.3、
> `sysinfo` 0.38.4、`nix` 0.30.1、`windows-sys`（0.45~0.61）、`winapi`、`walkdir`、
> `memchr`、`glob`、`regex`、`tempfile`、`chrono`、`portable-atomic` 等均在本地 registry。
> globset/ignore 已在缓存但未提取，跑一次普通 `cargo check` 会自动提取。

## 一、只缺这一个 crate：portable-pty（P1 PTY / terminal 落地的唯一硬依赖）

在普通终端（无沙箱受限）执行：

```powershell
# 直接用 cargo 拉取并放进全局缓存（需网络；验证会走到 rsproxy）
cd F:\RustProjects\dsh-rs
cargo add portable-pty --version 0.8 --dry-run   # 先看解析结果（可选）
```

> 若 `cargo add` 对该环境不合适，可直接建一个临时工程把它 fetch 进缓存：

```powershell
$dir = "$env:TEMP\m5deps"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
@"
[package]
name = "m5deps"
version = "0.1.0"
edition = "2021"
[dependencies]
portable-pty = "0.8"
"@ | Set-Content -Path "$dir\Cargo.toml" -Encoding utf8
New-Item -ItemType Directory -Force -Path "$dir\src" | Out-Null
"fn main() {}" | Set-Content -Path "$dir\src\main.rs" -Encoding utf8
cargo fetch --manifest-path "$dir\Cargo.toml"
cargo check --manifest-path "$dir\Cargo.toml"   # 此步同时提取到 registry/src
```

验证（任选其一，若失败把完整报错发我）：

```powershell
Test-Path "$env:USERPROFILE\.cargo\registry\cache\*\portable-pty-*.crate"
Get-ChildItem "$env:USERPROFILE\.cargo\registry\src" -Recurse -Directory -Filter 'portable-pty-*'
```

## 二、globset / ignore（已在缓存，需要一次提取动作）

以上 `cargo fetch` + `cargo check` 的临时工程若把 `globset` / `ignore` 也加上，一次跑完：

```powershell
@"
[package]
name = "m5deps"
version = "0.1.0"
edition = "2021"
[dependencies]
portable-pty = "0.8"
globset = "0.4"
ignore = "0.4"
"@ | Set-Content -Path "$env:TEMP\m5deps\Cargo.toml" -Encoding utf8
cargo check --manifest-path "$env:TEMP\m5deps\Cargo.toml"
```

## 三、验证方法（我可以直接跑）

你执行完毕后告诉我一声即可；我会用 `cargo check --offline --manifest-path <临时工程>` 复验
portable-pty / globset / ignore 是否可离线解析——离线通过即视为依赖阻塞解除。

## 四、需要用户裁定的关联项（如前面 M5-REQUIREMENTS P1/P2 所述）

- **P1 terminal**：若 portable-pty 装好，M5 可真实做 terminal（不再必须推迟 M6）。
- **P2 IANA 时区**：jiff 已就绪，M5 取 P2(a) 已无任何依赖障碍。
- **fs-search（glob/grep）**：globset+ignore 引擎已可落地，M5 可真实实现搜索工具，
  不再受「ripgrep 二进制不可得」限制（可选入 M5，见需求文档裁定表）。
- **strip-ansi-escapes**：shell 输出 ANSI 剥离可选依赖，未在本地；如需则同样 fetch。

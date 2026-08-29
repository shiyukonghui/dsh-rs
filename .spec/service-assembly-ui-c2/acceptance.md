# 验收结论：桌布 C2 —— `uiManifest/list` 实时清单端点

日期：2026-09-04
阶段：测试验证 + 部署与维护（瀑布流阶段 4/5）——基于 requirements.md（已过闸）+ design.md。
决策记录：`docs/DECISIONS.md` **D-183**。
git 链：`b95d260`（设计闸：需求/设计工件 + D-183 + 回写 canvas design）→ 本验收提交（代码 + 测试）。

---

## 1. 交付物

| 件 | 内容 |
|---|---|
| `crates/dsh-cli/src/ui_manifest.rs` | 新模块：`build_manifest`（实时聚合纯函数）+ `ui_manifest_result`（wire）+ 校验链/归一/sha256 rev；含 8 个单元测试 |
| `crates/dsh-cli/src/web.rs` | `dispatch()` 原生臂 `"uiManifest/list"`（与 `commands/list` 同型，零路由例外）；3 个集成测试 |
| `crates/dsh-cli/Cargo.toml` | `sha2 = "0.10"`（已在 lock/本地缓存，离线解析成功，零新增供应链） |
| `crates/dsh-cli/src/lib.rs` | `pub mod ui_manifest;` |
| canvas design §6.1/§1/§8/§10/§11/§13 | wire 形状纠偏 + C2 状态点亮（D-183 批次） |

## 2. 逐条验收（requirements §2 判据 → 证据）

| # | 判据 | 证据（测试名） | 结果 |
|---|---|---|---|
| S1 | 聚合正确（六元组、声明序、无坐标） | `aggregates_two_good_packages_in_declaration_order` | ✅ |
| S2 | 实时性（无缓存） | `rpc_ui_manifest_is_live_no_cache`（同 boot 两请求间改 ui.json → 条目+rev 变）+ **缓存探针红验证**（见 §3.2） | ✅ |
| S3 | rev 内容哈希语义 | `rev_is_content_hash_stable_and_changes`（同内容稳定 / title 改 / 加卡 / 删卡 / 坏→修好 皆变；空清单确定；64-hex）+ `rpc_ui_manifest_list_shape` 空清单 rev | ✅ |
| S4 | 坏包 error 条目不静默 | `broken_declarations_become_error_entries`（四码齐：`declaration-unparseable` / `schema-version-unsupported` / `card-kind-unknown` / `card-id-missing`；坏不连坐） | ✅ |
| S5 | 无 UI 安静跳过 | `skips_packages_without_ui_json` | ✅ |
| S6 | 归一在清单层 | `unknown_type_falls_to_misc_keeping_declared_type` + `oversized_size_clamped_and_recorded`（9×9→4×8 + declaredSize + x/y 不泄漏）+ `size_defaults_by_view_kind`（status 2×2 / list 4×4 / form 2×3） | ✅ |
| S7 | disabled 交叉 | `disabled_entry_excludes_card`（全禁排除 / 任一 enabled 出 / 无 entry 出 / group 不参与） | ✅ |
| S8 | 不回归 | dsh-cli **241/0**（基线 230 + 新增 11，零劣化）；m9_boot 23/23；dsh-wasmrt 14 目标全绿（含 m32 **8/8**、m31 8/8）；clippy `-D warnings` **0**；verify-diff **26/26 ALL PASS** | ✅ |
| 协商 | `args.rev` 短路 | `rpc_ui_manifest_unchanged_short_circuit`（`{rev, unchanged:true}` 无 cards；过期 rev → 全量） | ✅ |

## 3. TDD 纪律记录（红→绿→重构）

### 3.1 桩红（行为缺失验证）
先写全部 11 个测试 + 空桩实现（编译通过、零行为）：
**10/11 FAILED**（聚合/坏包/归一/裁剪/默认/rev/disabled/三集成），
唯一 pass 的是 `skips_packages_without_ui_json`——负向断言被空输出平凡满足（预期内，
它守护的是"缺 ui.json 不得变 error 条目"这条回归，真实实现里该路径有独立代码支撑）。
失败信息均为行为性（如「两个好包两张卡，得 []」），非编译错。

### 3.2 缓存探针红验证（承 D-182 手法，护栏非恒真）
临时在 `ui_manifest_result` 注入 `OnceLock` 快照缓存（模拟被契约禁止的"启动期缓存"）：
`rpc_ui_manifest_is_live_no_cache` **FAILED**（缓存先行者填充后，后续请求看到陈旧清单）、
`rpc_ui_manifest_list_shape` **FAILED**——**实时性护栏能真实抓住缓存违规**。探针已移除，移除后 11/11 复绿。

### 3.3 重构
clippy 指出的 8 处 `&[x.clone()]` → `std::slice::from_ref`（测试代码），复绿后 clippy **0**。

## 4. 环境执行记录（接手文档 §4 地雷全数应验/规避）

- 全部命令先 `Remove-Item Env:RUSTC_WRAPPER` + `$env:CARGO_NET_OFFLINE="true"`，**不带** `--offline` → 构建/测试正常。
- PowerShell `NativeCommandError` 假失败（exit 1 + stderr 进度）出现于每次 cargo 调用——以 `Finished`/`test result: ok` 为准，**未误判**。
- 含中文文件全部经编辑器工具（write/edit）读写，未用 pwsh 写；提交中文信息经 `.git/` 临时文件 + `git commit -F`（字节校验 UTF-8 序列 81 处通过）。
- `sha2` 离线新增依赖解析成功（本地缓存 sha2-0.10.8/0.10.9 命中 lock 的 0.10.9）。

## 5. 诚实台账

1. **基线数字优于接手文档**：本机 5 个「M5 环境性失败」全绿 → 以 **230/0**（现 241/0）为底线；
   接手文档的 225/5 未在本机复现（环境差异，非代码差异）。
2. **serve 冒烟**：未跑真实 `dsh serve` 进程冒烟（需完整配置/键环境）；`/api/uiManifest/list`
   到 `dispatch()` 的通路是**通用** `/api` POST 路由（未加任何路由分支），由
   `handle_rpc` 集成层 + `rpc_ui_manifest_*` 三测 + 套件内 gated 冒烟测
   `serve_closure_real_endpoint_smoke_gated`（绿）覆盖。
3. **探针红的形态**：OnceLock 为进程级，探针红经由「缓存被先行测试填充 → 后续测试见陈旧」
   路径触发，比「单测试内自见陈旧」略间接，但足以证明护栏非恒真。
4. **清单不含 `view.kind`**：严守 D-181「清单只元数据」六元组；C3 分派渲染器需经
   `declPath` 拉声明后取 kind（三条通道分工的设计本意，不是遗漏）。
5. **C2 边界外未动**：`ui-manifest-changed` SSE（C5）、桌布壳（C3）、`status/list` 渲染器（C4）、
   试点 entry 化、wasm 侧一行，均未触碰。

## 6. 回滚点

单提交回滚：撤销本实现提交即回到 `b95d260`（设计闸）；再撤设计提交回 `44f9618`。
代码面 = 新模块 + 一臂 + 一行依赖 + 测试，既有 wire 面/路由/wasm 零改动，无数据迁移。

## 7. 下一步（C3 入口条件已备）

桌布壳可直接消费本端点：`POST /api/uiManifest/list` → 侧栏按 `type` 分组 + 计数、
右侧按 `size` 排布、error 条目画 fail-loud 卡、`declPath` 拉声明分派 `view.kind`、
轮询携 `rev`（C5 SSE 落地前的过渡）。

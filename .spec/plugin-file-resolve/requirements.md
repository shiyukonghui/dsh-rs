# 需求结论 v3（正式）：插件包 = 文件夹（wasm 组件 + 前端组件），文件夹名 = 插件注册名

日期：2026-08-27（v2 草案 → v3 正式：四项决策用户确认）
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件；**已过闸（用户确认）**。

## 0. 需求变更记录

- **v1**（弃）：声明形态 A/B/C 三选一。用户否决，定向：**插件=文件夹（wasm+前端），文件夹名=注册名**。
- **v2**（草案）：以上述模型重构；标出 Q1-Q4 开放点。
- **v3**（本版，正式）：Q1-Q4 用户拍板（推荐项全选）→ 需求冻结。

## 1. 目标（第一性原理 + 用户定向）

真实 harness 的「插件文件→注册名」=`Tree.import(name)`（name 即 specifier，文件/包/内置统一解析，
插件凭服务自我描述运行）。我们对齐：**插件 = 一个文件夹包**，起即解析：

- 文件夹内含 **wasm 组件**（dsh-plugin / dsh-loop world）＋ **前端组件**（web UI 部分）；
- **文件夹名 = 插件注册名**：`name: <文件夹名>` 即解析为该包（与真实 harness「name 即包」同构）；
- 启动按 name 解析文件夹 → 加载 wasm（world 判别选适配器）→ 按文件夹名注册 → 前端组件挂 web serve。

## 2. 冻结决策（用户确认，2026-08-27）

| # | 决策 | 选择 |
|---|---|---|
| D1 布局 | 插件包文件夹布局 | **plugin.json 清单声明 + 约定回退**：`plugin.json`（wasm 路径 / web 目录 / caps / 可选 world 提示）；缺省回退到既有构建约定 `<name>/target/wasm32-wasip1/debug/<name>_plugin.wasm` |
| D2 前端 | 前端组件形态 | **a 静态资源目录挂接**：包 `web/`（或清单声明的 web 目录）由 `dsh web` 挂到 `/plugins/<name>/**`；主 SPA 按 URL 引用 |
| D3 范围 | 本轮范围 | **Rust 侧**：folder→wasm 注册 + 前端静态挂接/元数据 + 测试；web GUI 消费侧留待后续 |
| D4 loop | turn-loop 与 folder 关系 | **name=folder + world 判别为唯一路径**（dsh-loop→turn 句柄，dsh-plugin→组件插件）；**移除 config.wasm 特判**；web-cordis.yml 迁纯 folder 形态 |

## 3. 成功标准（验收，S1-S4）

- **S1 文件夹解析**：`name` 未命中内置/宿主注册 → `<wasm_base>/<name>/` 为包 → 读 plugin.json
  （或缺省）定位 wasm + web + caps；world 判别（预检组件导出接口：`plugin-api`→Plugin，
  `agent-loop`→Loop，其它→fail-loud 明确报错）→ 按 `name` 注册。
- **S2 loop 统一**：首个 dsh-loop 包 = turn 句柄（`Boot.loop_plugin`；run_turn 具体类型）；其余
  dsh-loop 包照常注册非句柄；**无 config.wasm 键**。
- **S3 前端挂接**：`dsh web` 增加 `/plugins/<name>/**` 静态路由（包 web 目录；目录→index）；
  路由在 SPA fallback 之前；不破坏 SPA。
- **S4 回归**：web-cordis.yml 迁移后 boot/serve 冒烟（HTTP 200/13270）不破坏；全回归基线
  （workspace 0 / clippy 0 / verify-diff 26/26）。

## 4. 非目标（划界）

- 不做插件文件 watch（HMR 显式 refresh 重解析；DIV-HMR-2 边界保持）。
- 前端组件不构建期打进主 SPA（D2=a 静态目录；b/c 否决）。
- 非 dsh-world wasm /其它模块语言不支持；不动 loader 核心（register_plugin/entry 语义）。
- web GUI 消费 `/plugins/**` 与配置编辑面留待后续（D3）。

## 5. 验收标准（阶段关卡细化）

1. wasmrt：`detect_component_kind` 两向正确（echo-loop→Loop、hello-component→Plugin）+ 非法字节→Unknown。
2. cli：`resolve_package` 清单/回退两路；boot 装配 folder 包（loop + plugin 并存、按名牌注册、
   apply 生效）；错误路径 fail-loud（包目录存在但 wasm 缺失 / 非 dsh-world 明报）。
3. cli web：`/plugins/<name>/<asset>` 命中包 web 资源、目录索引、SPA miss 不受影响。
4. web-cordis.yml 纯 folder 形态 boot/serve 冒烟 200/13270；`cargo test --workspace` 0；`clippy -D
   warnings` 0；`verify-diff` 26/26。
5. 工件三件 + DECISIONS（D-174 起）+ git 提交互查；回滚点明确。


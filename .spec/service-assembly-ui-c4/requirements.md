# 需求结论：桌布 C4 —— status/list 渲染器 + 首个 harness 面板改写（插件清单服务单元）

日期：2026-09-05
阶段：需求分析（瀑布流阶段 1）
过关方式：用户授权自主推进（同 C3），默认值可回退；决策记录 D-185。
上游：`.spec/service-assembly-ui-c3/`（桌布壳已落地）、canvas design §4.1（status/list 契约）、
§11 C4（首个 list 试点建议 = 插件清单）、用户目标「完成C3后，继续将其他的 deepseek harness
插件改写为服务单元」。

---

## 1. 目标（第一性原理拆解）

用户目标的根本形态：**「UI + 逻辑」逐块从 harness 前端拆成服务装配单元**（D-179/D-181
迁移模型：迁一块、验一块，未迁部分仍用原前端）。C3 的桌布只有 form 一档 + 一个试点卡，
承载不了面板级改写。C4 补齐改写所需的最小闭环：

1. **渲染器点亮 `status`/`list`**（§4.1 契约早已定稿——本轮是实现，不是新契约）；
   `chat/chart/table` 仍契约预留。
2. **首个改写试点 = 插件清单卡**（canvas design §11 既定建议）：harness 的插件面板
   本质是 loader 条目的列表（大量面板皆列表，form 表达会别扭）。
3. **改写通路泛化**（关键架构步）：新单元接入不能再靠 `web.rs` 硬编码——
   **「每装配单元一载体、namespace 分流」**（接手文档 §2.2 预告的扩展方向）：
   `Boot.remote_carriers`（namespace → 载体）+ serve **扫描 wasm_base 发现装配
   world:"remote" 的包**并挂载。这才兑现「热插拔第一等」：新单元 = 放文件夹，
   不改宿主一行。

**C4 之后的改写就是复制此型**（每个 harness 面板一个包），不再动宿主。

## 2. 决策回执（自主过闸；默认值均可回退）

| # | 开放点 | 默认值 | 理由/回退 |
|---|---|---|---|
| 1 | 插件清单卡 `type` | **`runtime`**（loader 装配态 = 运行时编排；`capability` 留给工具/技能） | 侧栏归类观感；改枚举值一行 |
| 2 | 数据面 | 新单元 `handle("panel-plugin-inventory","list")` → `host_services.get("loader")` → `value.items` 行投影 | 单元自带逻辑（服务装配单元本义，不是 JSON 壳）；宿主 projector 现成 |
| 3 | 行动作 | v1 **只读**（rowActions 空）；渲染器给 list/status 内置「刷新」affordance（重放 dataRpc） | 装卸动作走 dynamicCordisRunner 面，卡内动作是后续；刷新是渲染器 affordance 不改契约 |
| 4 | 行投影 | `{name, id, state}`：state = disabled?`disabled`:fiber?`active`:`ready`；group 条目**过滤** | projector 已有字段；group 是目录不是单元 |
| 5 | 载体注册形态 | `Boot.remote_carriers: Vec<(String, Rc<RefCell<WasmRemoteEndpointPlugin>>)>`；命中→专属载体，**未命中→host-remote（既有语义不变）** | 单线程 Vec 足够；llm_deepseek_remote 字段并入 map（行为零变） |
| 6 | 发现挂载 | serve 扫 `wasm_base/*/plugin.json`：`world:"remote"` 且 wasm 构建物存在 → 载体装配 + 包 push；缺构建物 → **跳过 + eprintln 诚实提示**（不 fail serve） | 与既有「缺组件→诚实回落」同纪律 |
| 7 | size 默认 | list 卡走契约默认 4×4（不写 size 即得） | §5.1 既有规则 |

## 3. 自上而下：成功标准分解

| # | 子目标 | 验收判据（设计阶段细化到测试名） |
|---|---|---|
| S1 | 渲染器点亮 | core.js：`IMPLEMENTED = form,status,list`；`renderer-unimplemented` 只剩 chat/chart/table；status 体/list 体纯函数有测试（items/rowsPath 提取、空态 emptyText、静态兜底、tone/kind 映射） |
| S2 | 首个改写单元 | `wasm-plugins/panel-plugin-inventory/`（wit+src+plugin.json+web/ui.json+离线 Cargo.lock）：describeUI 与静态 ui.json **逐字段一致**（m33 断言，仿 m32）；list 端点行投影正确（含 disabled 行、group 过滤）；未知端点 fail-loud |
| S3 | 双模型防线延伸 | m32 `no_legacy_v1_top_level_declaration_anywhere` 自动覆盖新包（它遍历全仓）——新声明合 v2 |
| S4 | 载体泛化 | `remote_carriers` 命中分流（含 llm-deepseek 行为零变——既有测试全绿即证）；`dispatch_wasm_remote` 无硬编码 namespace |
| S5 | 发现挂载 | serve：world:"remote" 包被发现（载体可路由 + `/plugins/panel-plugin-inventory/ui.json` 静态 200）；无构建物包 → 跳过不炸 |
| S6 | 清单联动 | 新包挂载后 `/api/uiManifest/list` 出第二张卡（type runtime）——C2 零改动自动兑现（这就是 S5 的断言面） |
| S7 | 不回归 | dsh-cli 244/0 基础上新增全绿；m32 8/8；node --test 12/12 + 新增；clippy 0；verify-diff 26/26 |

## 4. 自下而上：现有事实（已读实证）

| 事实 | 证据 | 含义 |
|---|---|---|
| wasm 单元可用 `host_services::get("loader")` 拿 loader 条目 | `llm-deepseek/src/lib.rs:128-140`（get_service 模式）；`remote_host.rs:152-169/260-264`（"loader" 投影 `{ok,entries[{id,name,disabled,group,fiber}]}`） | 数据面零新宿主后端 |
| `dispatch_wasm_remote` 硬编码 `namespace == "llm-deepseek"` | `web.rs:4463` | 泛化点 1：map 分流，未命中走 host-remote（现状语义保留） |
| serve 硬编码装配 llm-deepseek（bytes 构建 + resolve + push） | `web.rs:278-296` | 泛化点 2：扫 wasm_base + plugin.json.world=="remote" |
| `PluginPackage.world` 字段已有（"loop"/"plugin"，remote 未定义） | `plugin_pkg.rs:35/87` | plugin.json 已写 `world:"remote"`（试点包如此）——扫描器按此判别；resolve_package 不动（world 只是提示） |
| llm-deepseek 包 = 离线 wasm 包完整模板（wit 复用 host-remote 接口身份/Cargo.lock 手工复制/8 测模板） | `wasm-plugins/llm-deepseek/**` + 接手文档 §4-4 | 新包照抄型；m33 仿 m32 |
| m32 遍历全仓 ui.json | `m32_llm_deepseek.rs` | 新包自动被双模型防线覆盖 |
| harness 前端在 `deepseek-harness/`（TS 参考件，只读参照） | 目录实证 | 改写以「面板→卡」抽象，不改 TS 件 |

## 5. 非目标

`chat/chart/table` 渲染器 · 卡内装卸插件动作（rowActions 只读）· loader entry 化 ·
SSE（C5）· 第二个以后的面板改写（C4 立型，后续复制）· 推翻 harness 前端 · SSR ·
wasm 内真实网络。

## 6. 假设与常见错误

- **假设**：§2 表默认值；「其他的 deepseek harness 插件」按「面板→卡」推进，
  C4 立型后逐个复制（清单→设置→任务/调度…按面板数据面就绪度排序）。
- **最易犯错误**：
  1. **给面板配 JSON 壳包**（无 wasm 逻辑）——违背服务装配单元「UI+逻辑同包」本义；
     resolve_package 也要求 wasm 存在。逻辑就在 wasm 的 handle 里。
  2. **再硬编码一个 namespace**——C4 的意义就是关掉这条路；发现式挂载一步到位。
  3. 扫描器把**无构建物**的目录当可用包 push（serve 当场炸或清单出永远 404 的死卡）——
     缺构件必须跳过 + 诚实提示。
  4. list 渲染器在壳里**复制宿主行投影**（双权威漂移）——行数据只信单元 list 端点。

## 7. 验收（阶段关卡）

1. S1-S7 全部有测试名；新断言红验证（m33 先对 m32 型模板红）。
2. 工件：`.spec/service-assembly-ui-c4/{requirements,design,acceptance}.md` + D-185 +
   canvas §13 + git 提交互查。

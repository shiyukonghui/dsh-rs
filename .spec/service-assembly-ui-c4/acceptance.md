# 验收结论：桌布 C4 —— status/list 渲染器 + 插件清单服务单元 + 载体泛化

日期：2026-09-05 | 关卡：自主过闸（用户授权）| 决策记录 **D-185**
git：`3d49f2f`（设计闸）→ `957b244`（A+B）→ 本提交（C + 验收）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | 渲染器点亮 | `validateDeclaration …nine rows`（status/list 直通、chat/chart/table 仍预留、list 缺 rowsPath → view-malformed）+ `extractPath/listRows/statusItems` 3 测 + 「伪造行」桩探针被抓 | ✅ |
| S2 | 改写单元 | m33 5/5：v2 list 卡契约 / 静态=describeUI 逐字段 / loader 行投影（group 过滤 + state 映射）/ 服务失败 fail-loud 不夹带 items / 未知端点 fail-loud | ✅ |
| S3 | 双模型防线 | m32 `no_legacy_v1_top_level_declaration_anywhere` 遍历全仓（新包自动覆盖）绿 | ✅ |
| S4 | 载体泛化 | `llm_deepseek_remote_routes_and_serves_static` 迁移 carriers（分流断言主体不动）+ `dispatch_wasm_remote_unwraps_args_entry` / `rpc_dynamic_cordis_runner_unassembled` 零改动绿（host-remote 语义不变） | ✅ |
| S5 | 发现挂载 | `scan_remote_units_discovers_world_remote_and_skips_broken`（remote+构件收；非 remote/坏 json/无 json/缺构件/host-remote 跳） | ✅ |
| S6 | 清单联动 | `scan_mounted_units_appear_in_manifest`：scan → build_manifest → 第二卡（runtime, 4×4, declPath 正确），C2 零改动 | ✅ |
| S7 | 回归 | dsh-cli **246/0**；dsh-wasmrt 全绿（m32 8/8、m33 5/5）；clippy **0**；node --test **16/16** | ✅ |

## TDD 记录
A：桩红（validate 新档失败 + listRows 桩**故意返回伪造行**被诚实断言抓）→ 绿。
B：m33 先对不存在包红 → 包落地绿（构建走 §4 离线姿势）。
C：重构以既有测试为回归锚（host-remote 家族断言零改动即证行为零变）。

## 诚实台账
1. app.js 的 status/list DOM 绘制仍属无自动化边界（core 纯函数已全测；DOM 待人工浏览器验证）。
2. 未装配 boot 下 `llm-deepseek/*` 回落 code 由 `not-implemented` 统一为 `internal`
   （泛化的直接结果，记录于 D-185 验收段；host-remote 家族不变）。
3. 面板改写进度：**1/N**（插件清单）。后续面板（设置/任务/调度/会话…）按本型复制：
   新包（wit 复用 host-remote 身份 + ui.json + handle）→ scan 自动挂载 → 桌布自动出卡。
4. C5（`ui-manifest-changed` SSE 热插拔验证）仍未做——当前实时性 = 4s rev 轮询。

## 下一步（按优先级）
1. C5：`/plugins/events` 加 `ui-manifest-changed {rev}`（载体在 serve 挂载变化/清单内容变化时广播）。
2. 面板改写 ×N：设置卡（form）、任务/调度（list+status）……逐块走需求→设计→TDD。

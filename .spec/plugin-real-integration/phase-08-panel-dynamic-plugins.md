# 阶段 8 · panel-dynamic-plugins（动态插件）— ✅ 通过

- 功能清单：列表卡 + rowActions（启用/停止 confirm/卸载 confirm）；
  数据面 dataRpc `panel-dynamic-plugins/list` → 载体 → host services
  `dynamicPlugins`（定义+activeRun 投影）；动作 → `dynamicActivate/Stop/Undefine` 臂。
- 场景配置（R4「最真实形态」）：serve 追加
  `--dynamic-plugins-dir target\web\dynamic-plugins`（夹具=hello/package.json
  + hello-component 组件 3.8MB，`dsh-plugin` world，行为=greeting 服务 +
  ping→pong 监听器 fiber）。**新启动配方**（后续阶段沿用）：
  `dsh web scenarios\web-smoke.cordis.yml --port 60890 --agent-loop --service-units --dynamic-plugins-dir target\web\dynamic-plugins`
- 浏览器实测（verify-action-dynamic.mjs，**4/4 PASS**，console 零错）：
  1. 列表真行：`hello  hello-component  defined`；
  2. 点「启用」→ 重载行态 **`running`**（fiber 真起=监听器注册）；
  3. 点「停止」→ **confirm 弹窗真实出现并被应答**（dialog 日志
     `confirm:确认「停止」？`）→ 行态回 `defined`（fiber dispose）；
  4. 点「卸载」→ confirm → 行消失，卡显空态文案「没有已定义的动态插件」。
- 留痕与复原：undefine 为内存级（磁盘夹具无损）→ serve 重启即复原 defined 基线。
- 过程诚实：首跑 2 步误判=校验脚本自身 bug（激活轮询初条件即断 + confirm 阻塞式
  click 需 fire-and-forget+dialog 自动应答），修复后复跑取净证据；产品面首跑即
  已证明 undefine 生效（空态），无产品缺陷。

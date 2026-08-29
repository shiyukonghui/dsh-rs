# llm-deepseek 试点包前端资源（桌布契约 v2：卡片声明 静态面）

- `ui.json` —— **静态卡片声明**（`kind:"card"` + `view:{kind:"form",…}`，数据非代码）；
  与 wasm `describeUI` 输出**逐字段一致**（m32 `static_ui_json_matches_describe_ui` 断言守护，
  即「声明=数据、一份契约」）。
- `renderer.js` —— 该卡片的最小渲染实现（C1）：只读声明 → 校验（契约 §7 fail-loud 表）→
  按 `view.kind` 分派 → `dataRpc` 预填 → 动作 RPC。未实现/被否决/坏声明一律落
  fail-loud 元数据卡，**不白屏、不伪造**。
- `index.html` —— demo 容器（标题/描述/表单/动作/状态行）。

契约来源：`.spec/service-assembly-ui-canvas/design.md`（v2，D-181）。
完整桌布（左侧分类栏 + 右侧网格工作台）在 C3 落地；`status`/`list` 渲染器在 C4。

通过宿主 `/plugins/llm-deepseek/**` 静态挂接分发（`serve_package_asset`，D-175）。

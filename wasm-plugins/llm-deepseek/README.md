# llm-deepseek —— P2 服务装配单元试点（rust + ui 声明 + wasm）

由 deepseek-harness `packages/llm/llm-deepseek`（TS cordis 一行插件）转换而来。

## 为什么是试点

- HANDOFF 点名的 canonical「cordis.yml 一行插件」：`name='llm-deepseek'`、`inject=['llm']`。
- Config 是纯声明式表单（apiKeyEnv/baseURL/thinking/reasoningEffort/maxTokens/
  defaultContextWindow/models[]）——天然是 P2 声明（数据），无需 P1 JS bundle。
- 复用 host-remote world 接口身份：`export remote` + `import host-services`，宿主
  `WasmRemoteEndpointPlugin` **零改动**即可加载。

## 布局

```
llm-deepseek/
  Cargo.toml            # wasm 组件（cdylib + wit-bindgen-rt 0.44）
  wit/llm-deepseek.wit  # world: export remote + import host-services
  src/lib.rs            # describeUI / currentValues / save / discoverModels
  src/bindings.rs       # cargo component 生成
  plugin.json           # 清单：wasm + web + caps(remote) + world(remote)
  web/
    ui.json             # 静态 UI 声明（与 describeUI 逐字段一致）
    index.html          # 最小通用渲染器 demo
    renderer.js         # 只读声明 → 渲染表单 → 动作 RPC
```

## 动作面（namespace = `llm-deepseek`）

| method | 说明 |
|---|---|
| `describeUI` | 返回 UI 声明（数据，非代码） |
| `currentValues` | 读宿主 kv `llm-deepseek/settings` 已保存值 |
| `save` | 白名单校验 → `host-services.set("kv", …)` 落盘 |
| `discoverModels` | 返回默认模型目录（V4 Flash / V4 Pro / V4 Flash Vision Exp） |

## 构建（离线环境）

```
$env:CARGO_NET_OFFLINE="true"; cargo component build --manifest-path Cargo.toml
```

## 测试

- `crates/dsh-wasmrt/tests/m32_llm_deepseek.rs`（6 断言：声明面/动作面/failloud/契约一致）
- `crates/dsh-cli` `llm_deepseek_remote_routes_and_serves_static`（路由 + 静态分发）

## 验收与边界

验收：`.spec/service-assembly-ui-pilot/acceptance.md`。
边界：不做真实 LLM 调用 / 不做 loader entry 依赖激活 / 不做 SSR（见 requirements §1.3）。

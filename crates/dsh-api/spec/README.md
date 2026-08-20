# dsh-api:spec — Remote 契约仓库（基线转译）

> 单一权威：Rust 侧 RPC dispatch（M1/M3）按本仓库校验方法与错误码，前端
> `UNARY_VALUE_SCHEMAS` 形状测试以本仓库为锚。所有 JSON 均为对固定基线
> `deepseek-harness @ 47f943859b` 的**机械转译**（不手抄），每条记录带 `$provenance`。

## 文件

| 文件 | 内容 | 权威源 |
|---|---|---|
| `methods.json` | 52 个 RPC 方法目录（wire/namespace/method + request/value schema 引用） | `api/rpc-map.ts` |
| `errors.json` | 39 个错误码 + details 字段 required/optional | `api/rpc.ts` `RpcErrorDetailsMap` |
| `messages.json` | 四象限消息模型 + RpcResult + RpcReceipt + error 体 | `api/rpc.ts` `rpc.schema.ts` |
| `schemas/session.json` | **session 域** request/value 的 JSON Schema 转译（示例域） | `api/sessions.schema.ts` |

## 转译纪律

- `z.string().min(1)` → `type:string, minLength:1`；`z.number().int().nonnegative()` →
  `type:integer, minimum:0`；`z.literal('x')` → `const:"x"`；`z.optional()` → 不出现在
  `required`。
- `z.unknown()` / `z.looseObject(...)`（合并可扩展通路，如 `sessionEvent.data`、
  `contentBlockSchema`、`sessionProjectionsBlock.values`）在 JSON Schema 用宽对象/空元数据
  标注——严格校验只锁信封，data 宽（与 TS wire 层一致）。
- 品牌 cast 点（`sessionIdSchema` 等）在 JSON Schema 中即 `string, minLength:1`
  （类型品牌是编译期身份，wire 上就是普通字符串）。

## 已知延迟（deferred）

- **schemas/：其余域**（host/workspace/skills/goals/settings/credentials/llm/subagents/
  agentPresets）的 request/value JSON Schema 在 M3 各域 dispatch 落地时按 `session.json`
  的同一模板补交——M0 固化词表与消息模型，避免在无消费方的今天为全量手抄而滋生偏差。
- **mux/host 帧**（`events.mux`/`events.host` 的 frame unions）分属 download/事件下链，
  于 M2 事件下链语义落地时补交为 `schemas/events.json`。

## 升级路径

前端 Remote 描述符随版本演进时：改 `$provenance` 的基线 SHA → 按上表重新转译 →
`cargo test -p dsh-api` 的方法/错误码逐项断言驱动核对差异。

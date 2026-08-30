// 挂死复现：黑洞 LLM 端点（接受连接永不响应）→ 卡 baseURL 指黑洞 → prompt → 测 GET / 与 RPC 响应时间。
import http from "node:http";
const API = "http://127.0.0.1:60890/";
const BLACK = 39991;
const conns = [];
const srv = http.createServer(() => { /* 永不响应 */ });
srv.on("connection", (s) => conns.push(s));
await new Promise((r) => srv.listen(BLACK, "127.0.0.1", r));
const rpc = (m, payload, ms = 90000) => fetch(API + "api/" + m, { method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ type: "client-request", rpcId: "r", method: m, payload }), signal: AbortSignal.timeout(ms) })
  .then((r) => r.text()).then((t) => t.slice(0, 80)).catch((e) => "ERR " + String(e).slice(0, 40));
// 1. 卡面契约直写 kv：baseURL=黑洞（effort low 防 400）
console.log("save:", await rpc("llm-deepseek/save", { args: { values: { apiKeyEnv: "DEEPSEEK_API_KEY", baseURL: `http://127.0.0.1:${BLACK}/v1`, reasoningEffort: "low", thinking: "enabled", maxTokens: 256000, defaultContextWindow: 1000000, models: [] } } }));
// 2. 黑洞 prompt（fire-and-forget 线程侧接受即返回）
console.log("prompt:", await rpc("session/prompt", { args: { sessionId: "default", text: "hi" } }, 20000));
// 3. 并发探活 40s：GET / 响应时间
const times = [];
for (let i = 0; i < 14; i++) {
  const t0 = Date.now();
  const ok = await fetch(API, { signal: AbortSignal.timeout(2500) }).then((r) => r.status).catch(() => "TIMEOUT");
  times.push(`${i * 3}s:${Date.now() - t0}ms/${ok}`);
  await new Promise((r) => setTimeout(r, 500));
}
console.log("probes:", times.join(" "));
console.log("blackhole conns:", conns.length);
for (const s of conns) s.destroy();
srv.close();
process.exit(0);

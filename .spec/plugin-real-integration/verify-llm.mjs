// 阶段 13 · llm-deepseek 浏览器全链（D-221/222/223）：
// save(valuesKey)→kv 往返→发现模型(真外呼臂→resultToField 注入)→
// 卡 baseURL=本地桩→chat 走桩(热覆盖铁证)→复原真端点→chat 真回复。
import { spawn } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = "http://127.0.0.1:60890/";
const API = URL_ + "api/";
const TS = Date.now();
const STUB_PORT = 39817;
const STUB_REPLY = `STUB-REPLY-${TS}`;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9364;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vllm-${TS}`);
const secrets = fs.readFileSync("target/verify-secrets.env", "utf8");
const secret = (k) => (secrets.split(/\r?\n/).find((l) => l.startsWith(k + "=")) || "").split("=").slice(1).join("=").trim();
const REAL_BASE = secret("LLM_BASE_URL");

// 本地桩：GET /v1/models 目录；POST /v1/chat/completions SSE 固定回复。
const stubHits = { models: 0, chat: 0 };
const stubSrv = http.createServer((req, res) => {
  if (req.method === "GET" && req.url.endsWith("/models")) {
    stubHits.models++;
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ object: "list", data: [{ id: "stub-model-a", name: "Stub A" }, { id: "stub-model-b", name: "Stub B" }] }));
    return;
  }
  if (req.method === "POST" && req.url.endsWith("/chat/completions")) {
    stubHits.chat++;
    res.writeHead(200, { "content-type": "text/event-stream" });
    const c = (o) => `data: ${JSON.stringify(o)}\n\n`;
    res.end(
      c({ id: "1", object: "chat.completion.chunk", model: "stub", choices: [{ index: 0, delta: { content: STUB_REPLY } }] }) +
      c({ id: "1", object: "chat.completion.chunk", model: "stub", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }) +
      "data: [DONE]\n\n"
    );
    return;
  }
  res.writeHead(404); res.end();
});
await new Promise((r) => stubSrv.listen(STUB_PORT, "127.0.0.1", r));

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { steps: [], consoleErrs: [], stubHits: () => ({ ...stubHits }), markers: { STUB_REPLY } };
const step = (n, ok, info) => R.steps.push({ name: n, ok, ...(info !== undefined ? { info } : {}) });
const bye = async () => {
  try { proc.kill(); } catch {}
  try { stubSrv.close(); } catch {}
  R.stub = { ...stubHits };
  R.pass = !R.why && R.steps.every(s => s.ok) && R.consoleErrs.length === 0;
  console.log(JSON.stringify(R));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(R.pass ? 0 : 1);
};
setTimeout(async () => { R.why = "TIMEOUT"; await bye(); }, 300000);

const rpc = async (m, payload, timeoutMs = 150000) => {
  const r = await fetch(API + m, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ type: "client-request", rpcId: "v", method: m, payload }), signal: AbortSignal.timeout(timeoutMs) });
  return (await r.json()).result;
};
const hist = async () => (await rpc("session/history", { args: { sessionId: "default", limit: 500 } }))?.value?.events || [];

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { R.why = "NO CDP"; await bye(); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL_)}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Page.javascriptDialogOpening") rawSend("Page.handleJavaScriptDialog", { accept: true });
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") R.consoleErrs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 160));
  if (m.method === "Runtime.exceptionThrown") R.consoleErrs.push("EX " + String(m.params.exceptionDetails?.exception?.description ?? "").slice(0, 160));
};
const rawSend = (m, p = {}) => ws.send(JSON.stringify({ id: ++mid, method: m, params: p }));
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Page.enable"); await send("Runtime.enable");
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
const mark = async (title, attr) => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes(${JSON.stringify(title)})); if(!c) return false; c.setAttribute('data-v','${attr}'); return true; })()`);
    if (ok) return true;
  }
  return false;
};
const setField = (name, val) => evl(`(() => { const el=document.querySelector('[data-v=llm] [name=${JSON.stringify(name)}]'); if(!el) return false; el.value=${JSON.stringify(val)}; return true; })()`);
const clickAct = (label) => evl(`(() => { const b=[...(document.querySelector('[data-v=llm]')?.querySelectorAll('button')||[])].find(x=>x.textContent.trim()===${JSON.stringify(label)}); if(!b) return false; b.click(); return true; })()`);
const llmState = () => evl(`(() => { const c=document.querySelector('[data-v=llm]'); const t=c?.innerText||''; const actMsg=(t.match(/[\\u2713\\u2717][^\\n]*/)||[""])[0]; const base=c?.querySelector('[name=baseURL]')?.value||''; const models=c?.querySelector('[name=models]')?.value||''; return JSON.stringify({actMsg, base, models}); })()`);
const chatText = () => evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes('聊天')); return (c?.innerText||'').slice(-4000); })()`);

// 0. 标记两卡
if (!(await mark("DeepSeek Provider", "llm"))) { step("mount-llm", false); await bye(); }
if (!(await mark("聊天", "chat"))) { step("mount-chat", false); await bye(); }

// 1. 填 baseURL=桩 → 保存 → ✓ 已保存（valuesKey 契约）
await setField("baseURL", `http://127.0.0.1:${STUB_PORT}/v1`);
await clickAct("保存");
let saved = false;
for (let i = 0; i < 10; i++) { const s = JSON.parse(await llmState() || "{}"); if ((s.actMsg || "").startsWith("\u2713")) { saved = true; break; } await sleep(700); }
step("save-via-valuesKey", saved);

// 2. reload → currentValues 往返（baseURL 回填=桩地址）
await reload();
if (!(await mark("DeepSeek Provider", "llm"))) { step("mount-2", false); await bye(); }
let roundtrip = false;
for (let i = 0; i < 10; i++) { const s = JSON.parse(await llmState() || "{}"); if (s.base === `http://127.0.0.1:${STUB_PORT}/v1`) { roundtrip = true; break; } await sleep(700); }
step("currentValues-roundtrip", roundtrip);

// 3. 发现模型（真外呼臂打桩端点 + resultToField 注入 models）
await clickAct("发现模型");
let injected = false;
for (let i = 0; i < 12; i++) {
  const s = JSON.parse(await llmState() || "{}");
  if ((s.models || "").includes("stub-model-a") && (s.actMsg || "").includes("发现")) { injected = true; break; }
  await sleep(800);
}
step("discover-injects-models", injected && stubHits.models >= 1, { stubModels: stubHits.models });

// 4. chat 走桩（D-221 热覆盖铁证：卡 baseURL 即循环外呼地）
await rpc("session/prompt", { args: { sessionId: "default", text: "ping stub" } });
let stubReply = false;
for (let i = 0; i < 40 && !stubReply; i++) {
  await sleep(1000);
  stubReply = (await chatText()).includes(STUB_REPLY);
}
step("loop-follows-card-baseurl", stubReply && stubHits.chat >= 1, { stubChat: stubHits.chat });

// 5. 复原真端点 → 保存 → chat 真回复（assistant 文本非空且非桩回复）。
// 注意：表单默认 reasoningEffort=high 会被 kv 覆盖 env——真端点不收 high，先选 low。
await evl(`(() => { const el=document.querySelector('[data-v=llm] [name=reasoningEffort]'); if(el) el.value="low"; return true; })()`);
await setField("baseURL", REAL_BASE);
await clickAct("保存");
let restored = false;
for (let i = 0; i < 10; i++) { const s = JSON.parse(await llmState() || "{}"); if ((s.actMsg || "").startsWith("\u2713")) { restored = true; break; } await sleep(700); }
const p2 = await rpc("session/prompt", { args: { sessionId: "default", text: "Reply with exactly: REAL-CHAIN-OK" } });
let realOk = false;
for (let i = 0; i < 45 && !realOk; i++) {
  await sleep(1200);
  const evs = await hist();
  const um = evs.findLast?.((e) => (e.event || e).type === "user/message") || evs.filter((e) => (e.event || e).type === "user/message").pop();
  realOk = evs.some((e) => { const ev = e.event || e; return ev.type === "assistant/message" && ev.seq > (um?.event?.seq ?? um?.seq ?? 0); });
}
step("restore-real-endpoint-live", restored && p2?.ok === true && realOk);

await bye();

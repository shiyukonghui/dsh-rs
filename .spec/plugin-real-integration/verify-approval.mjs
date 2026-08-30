// 阶段 12 · panel-approval 浏览器全链（v2 自足）：提示→真 pending→浏览器允许→bash 真执行→
// 再提示→浏览器拒绝(确认)→两条 decided 留痕→plan 复原。断言只认类型化事件（防散文误报）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = "http://127.0.0.1:60890/";
const API = URL_ + "api/";
const TS = Date.now();
const M_OK = `E2E-APPR-ALLOW-${TS}`;
const M_DENY = `E2E-APPR-DENY-${TS}`;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9363;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vappr2-${TS}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { steps: [], consoleErrs: [], dialogs: [], markers: { M_OK, M_DENY } };
const step = (n, ok, info) => R.steps.push({ name: n, ok, ...(info !== undefined ? { info } : {}) });
const bye = async () => {
  try { proc.kill(); } catch {}
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
const hasEvent = (evs, type, sub) => evs.some((e) => { const ev = e.event || e; return ev.type === type && JSON.stringify(ev.data ?? {}).includes(sub); });

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { R.why = "NO CDP"; await bye(); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL_)}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const rawSend = (m, p = {}) => ws.send(JSON.stringify({ id: ++mid, method: m, params: p }));
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Page.javascriptDialogOpening") { R.dialogs.push(m.params.type + ":" + String(m.params.message || "").slice(0, 30)); rawSend("Page.handleJavaScriptDialog", { accept: true }); }
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") R.consoleErrs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 160));
  if (m.method === "Runtime.exceptionThrown") R.consoleErrs.push("EX " + String(m.params.exceptionDetails?.exception?.description ?? "").slice(0, 160));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Page.enable"); await send("Runtime.enable");
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
const markAppr = async () => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes('待审批')); if(!c) return false; c.setAttribute('data-va','1'); return true; })()`);
    if (ok) return true;
  }
  return false;
};
const apprState = () => evl(`(() => { const t=document.querySelector('[data-va]')?.innerText||''; return JSON.stringify({bash: t.includes('bash'), empty: t.includes('没有待审批项')}); })()`);
const clickInRow = (label, blocking) => {
  const js = `(() => { const c=document.querySelector('[data-va]'); const tr=[...(c?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes('bash')); const b=[...(tr?.querySelectorAll('button')||[])].find(x=>x.textContent.trim()===${JSON.stringify(label)}); if(!b) return false; b.click(); return true; })()`;
  if (blocking) { rawSend("Runtime.evaluate", { expression: js }); return Promise.resolve(true); }
  return evl(js);
};
const waitPending = async (want) => {
  for (let i = 0; i < 20; i++) {
    const p = await rpc("approval/pending", {});
    const n = (p?.value?.items || []).length;
    if (want ? n >= 1 : n === 0) return true;
    await sleep(1000);
  }
  return false;
};

// plan on（flat 形——该 RPC 无卡消费方，flat 是其消费契约）
const on = await rpc("session.plan.mode", { active: true, message: "approval-e2e" });
step("plan-on", on?.value?.active === true);
if (!(await markAppr())) { step("mount", false); await bye(); }

// —— 第一轮：提示→pending→浏览器允许→真执行 ——
const p1 = await rpc("session/prompt", { args: { sessionId: "default", text: `Call the bash tool now to run: echo ${M_OK} . Use the tool, do not answer in prose.` } });
const pend1 = await waitPending(true);
// 卡列表=打开时快照（数据面不自刷新）→ 提示后重载取新行
await reload();
if (!(await markAppr())) { step("mount1", false); await bye(); }
let row1 = false;
for (let i = 0; i < 12; i++) { const s = JSON.parse(await apprState() || "{}"); if (s.bash) { row1 = true; break; } await sleep(700); }
const cA = await clickInRow("允许", false);
let allowed = false, bashOut = false;
for (let i = 0; i < 45 && !(allowed && bashOut); i++) {
  await sleep(1200);
  const evs = await hist();
  allowed = hasEvent(evs, "approval/decided", "allowedOnce");
  bashOut = hasEvent(evs, "tool/result", M_OK);
}
step("allow-row-click-and-executed", pend1 && row1 && cA && allowed && bashOut, { prompt1: p1?.ok === true });
const pendGone1 = await waitPending(false);
await reload();
let empty1 = false;
if (await markAppr()) { for (let i = 0; i < 10; i++) { const s = JSON.parse(await apprState() || "{}"); if (s.empty) { empty1 = true; break; } await sleep(700); } }
step("card-empties-after-allow", pendGone1 && empty1);

// —— 第二轮：提示→pending→浏览器拒绝(确认弹窗)→rejected 留痕 ——
await rpc("session/prompt", { args: { sessionId: "default", text: `Call the bash tool now to run: echo ${M_DENY} . Use the tool, do not answer in prose.` } });
const pend2 = await waitPending(true);
await reload();
if (!(await markAppr())) { step("mount2", false); await bye(); }
let row2 = false;
for (let i = 0; i < 12; i++) { const s = JSON.parse(await apprState() || "{}"); if (s.bash) { row2 = true; break; } await sleep(700); }
const cR = await clickInRow("拒绝", true);
let rejected = false;
for (let i = 0; i < 30 && !rejected; i++) { await sleep(1200); rejected = hasEvent(await hist(), "approval/decided", "rejected"); }
step("reject-confirm-decided", pend2 && row2 && cR && rejected);

// —— 复原 ——
const off = await rpc("session.plan.mode", { active: false });
step("plan-restored-off", off?.value?.active === false);

await bye();

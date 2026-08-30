// 阶段 10 · panel-schedule-create：浏览器创建 → 调度行在列 → ~60s 真触发 → 聊天卡可见（留痕 R2）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = "http://127.0.0.1:60890/";
const MARKER = process.argv[2] || `E2E-FIRE-${Date.now()}`;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9359;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vfire-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { marker: MARKER, steps: [], consoleErrs: [] };
const step = (n, ok, info) => R.steps.push({ name: n, ok, ...(info ? { info } : {}) });
const bye = async () => {
  try { proc.kill(); } catch {}
  const four = R.steps.filter(s => s.name.startsWith("chat") || s.name.startsWith("assistant"));
  R.pass = !R.why && R.steps.length >= 5 && R.steps.every(s => s.ok) && R.consoleErrs.length === 0 && four.length === 2;
  console.log(JSON.stringify(R));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(R.pass ? 0 : 1);
};
setTimeout(async () => { R.why = "TIMEOUT"; await bye(); }, 300000);

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
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") R.consoleErrs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 160));
  if (m.method === "Runtime.exceptionThrown") R.consoleErrs.push("EX " + String(m.params.exceptionDetails?.exception?.description ?? "").slice(0, 160));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Page.enable"); await send("Runtime.enable");
const MK = JSON.stringify(MARKER);
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
const markAll = async () => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => {
      const cs=[...document.querySelectorAll('#workbench .card')];
      const f=cs.find(c=>(c.innerText||'').includes('创建调度'));
      const l=cs.find(c=>(c.innerText||'').includes('调度任务'));
      const h=cs.find(c=>(c.innerText||'').includes('聊天')&&c.querySelector('.chat-send,input'));
      if(!f||!l||!h) return false;
      f.setAttribute('data-vf','form'); l.setAttribute('data-vf','list'); h.setAttribute('data-vf','chat'); return true; })()`);
    if (ok) return true;
  }
  return false;
};

if (!(await markAll())) { step("mount", false); await bye(); }

// 1. 表单真填真点
const formState = await evl(`(() => {
  const c=document.querySelector('[data-vf=form]');
  const p=c.querySelector('[name=prompt]'); if(!p) return "noprompt";
  p.value=${MK}; p.dispatchEvent(new Event('input',{bubbles:true}));
  const a=c.querySelector('[name=afterSeconds]');
  return JSON.stringify({kind:(c.querySelector('[name=kind]')||{}).value, after:(a||{}).value});
})()`);
step("form-filled", formState !== "noprompt", formState);
const t0 = Date.now();
await evl(`(() => { const b=[...document.querySelectorAll('[data-vf=form] button')].find(x=>x.textContent.includes('创建')); if(!b) return false; b.click(); return true; })()`);
await sleep(1500);
const act = await evl(`(() => { const t=(document.querySelector('[data-vf=form]')?.innerText||''); const l=t.split('\\n').find(l=>l.trim().startsWith('✓')||l.trim().startsWith('✗')); return l||""; })()`);
step("create-act-ok", act.includes("✓"), act);

// 2. 重载 → 调度行在列
await reload();
if (!(await markAll())) { step("reload1", false); await bye(); }
let row = "";
for (let i = 0; i < 10 && !row; i++) { await sleep(700);
  row = await evl(`(() => { const tr=[...(document.querySelector('[data-vf=list]')?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes(${MK})); return tr?(tr.innerText||'').replace(/\\n/g,' | '):""; })()`); }
step("list-shows-scheduled", !!row, row);

// 3. 等真触发（SSE 活折为主，每 15s 重载兜底；上限 ~150s）
let chatHit = "";
for (let i = 0; i < 50; i++) {
  await sleep(3000);
  chatHit = await evl(`(() => { const t=document.querySelector('[data-vf=chat]')?.innerText||''; return t.includes(${MK})?t.slice(t.indexOf(${MK}), t.indexOf(${MK})+260):""; })()`);
  if (chatHit) break;
  if (i % 5 === 4) { await reload(); await markAll(); }
}
step("chat-shows-firing", !!chatHit, chatHit.slice(0, 200));
R.firedAfterMs = Date.now() - t0;

// 4. 回应气泡（echo/助手）
let reply = "";
for (let i = 0; i < 10 && !reply; i++) {
  await sleep(2000);
  reply = await evl(`(() => { const t=document.querySelector('[data-vf=chat]')?.innerText||''; const i=t.indexOf(${MK}); if(i<0) return ""; const after=t.slice(i); return /助手|echo/i.test(after)?"yes":""; })()`);
}
step("assistant-echo-after", reply === "yes");

await bye();

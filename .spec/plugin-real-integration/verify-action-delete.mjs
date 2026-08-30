// 行删除动作浏览器校验（阶段 9 调度任务；confirm 弹窗自动应答）。
// 用法: node verify-action-delete.mjs --title 调度任务 --marker <行内子串> [--btn 删除]
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf("--" + k); return i >= 0 ? args[i + 1] : d; };
const URL_ = "http://127.0.0.1:60890/";
const TITLE = opt("title", "调度任务");
const MARKER = opt("marker", "hello");
const BTN = opt("btn", "删除");

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = Number(opt("port", "9358"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vdel-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { steps: [], consoleErrs: [], dialogs: [] };
const step = (n, ok, info) => R.steps.push({ name: n, ok, ...(info ? { info } : {}) });
const bye = async () => {
  try { proc.kill(); } catch {}
  R.pass = R.steps.every(s => s.ok) && R.consoleErrs.length === 0;
  console.log(JSON.stringify(R));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(R.pass ? 0 : 1);
};
setTimeout(async () => { R.why = "TIMEOUT"; await bye(); }, 120000);

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { R.why = "NO CDP"; await bye(); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL_)}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const rawSend = (method, params = {}) => ws.send(JSON.stringify({ id: ++mid, method, params }));
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

const T = JSON.stringify(TITLE), MK = JSON.stringify(MARKER);
const mark = async () => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes(${T})); if(!c) return false; c.setAttribute('data-vdel','1'); return true; })()`);
    if (ok) return true;
  }
  return false;
};
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
const rowOf = () => evl(`(() => { const c=document.querySelector('[data-vdel]'); const tr=[...(c?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes(${MK})); return tr?(tr.innerText||'').replace(/\\n/g,' | '):""; })()`);

if (!(await mark())) { step("mount", false); await bye(); }
let row0 = "";
for (let i = 0; i < 10 && !row0; i++) { await sleep(700); row0 = await rowOf(); }
step("list-shows-item", !!row0, row0);
if (!row0) await bye();

const js = `(() => { const c=document.querySelector('[data-vdel]'); const tr=[...(c?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes(${MK})); const b=[...(tr?.querySelectorAll('button')||[])].find(x=>x.textContent.trim()===${JSON.stringify(BTN)}); if(!b) return false; b.click(); return true; })()`;
rawSend("Runtime.evaluate", { expression: js });
await sleep(2500);
await reload();
if (!(await mark())) { step("reload-after", false); await bye(); }
let gone = false;
for (let i = 0; i < 10 && !gone; i++) { await sleep(700); gone = !(await rowOf()); }
step("delete-removes-row", gone, await evl(`(document.querySelector('[data-vdel]')?.innerText||'').slice(0,140)`));

await bye();

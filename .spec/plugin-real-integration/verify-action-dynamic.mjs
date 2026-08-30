// 阶段 8 · panel-dynamic-plugins 动作面浏览器校验：列表→启用(running)→停止(confirm 自动应答)→卸载(confirm)→空态。
// 前置：serve 带 --dynamic-plugins-dir（夹具 target/web/dynamic-plugins/hello）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = "http://127.0.0.1:60890/";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9357;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vd-${Date.now()}`);
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
setTimeout(async () => { R.why = "TIMEOUT"; await bye(); }, 180000);

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

const mark = async () => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes('动态插件')); if(!c) return false; c.setAttribute('data-vd','1'); return true; })()`);
    if (ok) return true;
  }
  return false;
};
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
// hello 行全文（含动作按钮文本无所谓，状态取第二/三列）
const helloRow = () => evl(`(() => { const c=document.querySelector('[data-vd]'); const tr=[...(c?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes('hello')); return tr?(tr.innerText||'').replace(/\\n/g,' | '):""; })()`);
// 在 hello 行内按按钮文本点击（confirm 类动作走 rawSend：click 被 confirm 阻塞，evaluate 不能等）
const clickRowBtn = (label, blocking) => {
  const js = `(() => { const c=document.querySelector('[data-vd]'); const tr=[...(c?.querySelectorAll('tbody tr')||[])].find(t=>(t.innerText||'').includes('hello')); const b=[...(tr?.querySelectorAll('button')||[])].find(x=>x.textContent.trim()===${JSON.stringify(label)}); if(!b) return false; b.click(); return true; })()`;
  if (blocking) { rawSend("Runtime.evaluate", { expression: js }); return Promise.resolve(true); }
  return evl(js);
};

if (!(await mark())) { step("mount", false); await bye(); }
let row0 = "";
for (let i = 0; i < 8 && !row0; i++) { await sleep(700); row0 = await helloRow(); }
step("list-shows-hello", !!row0, row0);
if (!row0) await bye();

// 启用（无 confirm）
let clicked = await clickRowBtn("启用", false);
await sleep(1800);
await reload();
if (!(await mark())) { step("reload-activate", false); await bye(); }
let row1 = "";
for (let i = 0; i < 10 && (!row1 || row1 === row0); i++) { await sleep(700); row1 = await helloRow(); }
step("activate-changes-state", clicked && row1 !== "" && row1 !== row0, { before: row0, after: row1 });

// 停止（confirm 自动应答；click 阻塞 → rawSend；fiber 协作停可能慢，30s 窗）
await clickRowBtn("停止", true);
await sleep(2500);
await reload();
if (!(await mark())) { step("reload-stop", false); await bye(); }
let row2 = "";
for (let i = 0; i < 30; i++) { await sleep(1000); row2 = await helloRow(); if (row2 === row0) break; }
step("stop-restores-state", row2 === row0, row2);

// 卸载（confirm；行消失→卡显空态）
await clickRowBtn("卸载", true);
await sleep(2000);
await reload();
if (!(await mark())) { step("reload-undefine", false); await bye(); }
let empty = "";
for (let i = 0; i < 8 && !empty.includes("没有已定义的动态插件"); i++) {
  await sleep(700);
  empty = await evl(`(document.querySelector('[data-vd]')?.innerText||'')`);
}
step("undefine-empties-list", empty.includes("没有已定义的动态插件"), empty.slice(0, 120));

await bye();

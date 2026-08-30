// DOM 取证：删除/启用类按钮的 class 与表格 class（解释 .ltable 规则未命中）
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const prof = path.join(os.tmpdir(), `dsh-dom-${Date.now()}`);
const proc = spawn("C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  ["--headless=new", "--remote-debugging-port=9375", `--user-data-dir=${prof}`,
   "--no-first-run", "--no-default-browser-check", "--window-size=1400,900", "about:blank"], { stdio: "ignore" });
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await new Promise(r => setTimeout(r, 400)); try { const r = await fetch("http://127.0.0.1:9375/json/version"); if (r.ok) ver = await r.json(); } catch {} }
const tgt = await (await fetch("http://127.0.0.1:9375/json/new?about:blank", { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const send = (m, p = {}) => new Promise(res => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
await send("Page.enable"); await send("Runtime.enable");
await send("Page.navigate", { url: "http://127.0.0.1:60890/" });
await new Promise(r => setTimeout(r, 7000));
const expr = `(() => {
  const btns = [...document.querySelectorAll('button')].map(b => ({ t: b.textContent.trim().slice(0, 6), cls: b.className }));
  const tbls = [...document.querySelectorAll('table')].map(t => t.className);
  const th = document.querySelector('th');
  const thStyle = th ? getComputedStyle(th).backgroundColor : null;
  return JSON.stringify({ btnSample: btns.filter(b => ['删除','启用','停止','卸载','允许','拒绝'].includes(b.t)).slice(0, 6),
    tbls, thStyle, divTable: !!document.querySelector('div[role=table]'), listSample: (() => { const t = document.querySelector('table'); return t ? t.outerHTML.slice(0, 160) : null; })() });
})()`;
const out = await send("Runtime.evaluate", { returnByValue: true, expression: expr });
console.log("DOM:", out.result?.result?.value);
proc.kill();
await new Promise(r => setTimeout(r, 400));
fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 });
process.exit(0);

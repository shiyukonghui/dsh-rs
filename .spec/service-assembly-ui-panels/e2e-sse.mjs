// SSE 传输层隔离实验：页面自开 /plugins/events EventSource，期间 node 侧 rename 单元，
// 看环回帧是否到达（区分「服务端没广播」vs「shell 监听器问题」）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9343;
const UNIT = path.join("wasm-plugins", "panel-locale-edit");
const OFF = UNIT + ".off";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-sse-profile")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "about:blank"], { stdio: "ignore" });
const bye = (c) => { try { if (fs.existsSync(OFF)) fs.renameSync(OFF, UNIT); } catch {} try { proc.kill(); } catch {} process.exit(c); };
setTimeout(() => { console.log("TIMEOUT"); bye(1); }, 90000);
let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); bye(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?http://127.0.0.1:60890/canvas/rust`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = (expression) => send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
await send("Page.enable"); await send("Runtime.enable");
await sleep(5000);
// 页面自开第二条 EventSource，收集 12 秒内所有帧
const p = evl(`new Promise(res=>{const got=[];const es=new EventSource('/plugins/events');
  es.onmessage=(e)=>got.push('f:'+e.data.slice(0,200));
  es.onerror=()=>got.push('ERR');
  setTimeout(()=>{es.close();res(JSON.stringify(got.slice(0,6)))},12000);})`);
await sleep(2000);
const t0 = Date.now();
fs.renameSync(UNIT, OFF);
const first = await p;
let m1 = -1; try { m1 = (await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json()).result?.value?.cards?.length; } catch {}
const elapsed = Date.now() - t0;
fs.renameSync(OFF, UNIT);
console.log(JSON.stringify({ frames: first?.result?.result?.value ?? String(first).slice(0, 120), m1, elapsed }));
bye(0);

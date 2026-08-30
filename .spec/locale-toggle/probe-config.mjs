// config 板卡数隔离实验：审计后立即加载 #board=config，3s/6s/10s 三拍计数。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9384;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-cfg3-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${prof}`,
  "--no-first-run", "--no-default-browser-check", "--window-size=1600,1000", "about:blank"], { stdio: "ignore" });
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await sleep(400); try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent("http://127.0.0.1:60890/#board=config")}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
await send("Page.enable"); await send("Runtime.enable");
const evl = async (x) => { const m = await send("Runtime.evaluate", { expression: x, returnByValue: true }); return m.result?.result?.value; };
await sleep(3000);
const a = await evl(`document.querySelectorAll('#workbench .card').length`);
await sleep(3000);
const b = await evl(`document.querySelectorAll('#workbench .card').length`);
await sleep(4000);
const c = await evl(`JSON.stringify({n:document.querySelectorAll('#workbench .card').length, titles:[...document.querySelectorAll('#workbench .card .cap')].map(x=>x.textContent.slice(0,18))})`);
console.log("t=3s:", a, " t=6s:", b, " t=10s:", c);
proc.kill();
await sleep(400); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 });
process.exit(0);

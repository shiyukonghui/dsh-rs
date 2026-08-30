// 诊断：为何画布页 bodyText 为空。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9383;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-dx-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${prof}`,
  "--no-first-run", "--no-default-browser-check", "--window-size=1600,1000", "about:blank"], { stdio: "ignore" });
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await sleep(400); try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map(); const net = [];
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Network.responseReceived") net.push(m.params.response.status + " " + m.params.response.url.slice(-60)); };
await send("Page.enable"); await send("Runtime.enable"); await send("Network.enable");
await send("Page.navigate", { url: "http://127.0.0.1:60890/" });
await sleep(8000);
const out = await send("Runtime.evaluate", { returnByValue: true, expression: `JSON.stringify({href:location.href, htmlLen:document.documentElement.outerHTML.length, cards:document.querySelectorAll('#workbench .card').length, body:(document.body?document.body.innerText.slice(0,160):'NO-BODY'), ready:document.readyState})` });
console.log("PAGE:", out.result?.result?.value);
console.log("NET:", net.slice(0, 12).join("\n"));
proc.kill();
await sleep(400); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 });
process.exit(0);

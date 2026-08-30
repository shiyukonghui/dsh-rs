// config 板补拍（hash 直达 + 等待渲染）
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9378;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-cfg-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${prof}`,
  "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--window-size=1680,1050", "about:blank"], { stdio: "ignore" });
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await sleep(400); try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); process.exit(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent("http://127.0.0.1:60890/#board=config")}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
await send("Page.enable"); await send("Runtime.enable");
await sleep(12000);
const cards = await send("Runtime.evaluate", { returnByValue: true, expression: `JSON.stringify([...document.querySelectorAll('#workbench .card')].map(c=>(c.innerText||'').split('\\n')[0]))` });
console.log("cards:", cards.result?.result?.value);
const r = await send("Page.captureScreenshot", { format: "png" });
if (r.result?.data) fs.writeFileSync("target/ui-ref/cards/board-config-v2.png", Buffer.from(r.result.data, "base64"));
console.log("shot", r.result?.data ? "OK" : "FAIL");
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
process.exit(0);

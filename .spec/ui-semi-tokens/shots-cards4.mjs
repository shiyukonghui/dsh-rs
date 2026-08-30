// 收口补拍：剩余未单独目检卡（工作区文件/动态插件/创建调度/设置概览/运行时状态）浅色。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const URL_ = "http://127.0.0.1:60890/";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9376;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-cards4-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${prof}`,
  "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding", "--window-size=1680,1050", "about:blank"], { stdio: "ignore" });
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await sleep(400); try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); process.exit(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
await send("Page.enable"); await send("Runtime.enable");
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true }); return m.result?.result?.value; };
const shotClip = async (file, title, hmax = 520) => {
  const b = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes(${JSON.stringify(title)})); if(!c) return null; const wb=document.getElementById('workbench'); wb.scrollTop = c.offsetTop - 10; const r=c.getBoundingClientRect(); return JSON.stringify({x:Math.round(r.x),y:Math.round(r.y),width:Math.round(r.width),height:Math.round(Math.min(r.height,${hmax}))}); })()`);
  await sleep(350);
  if (!b) { console.log(file, "NO-CARD"); return; }
  const r = await send("Page.captureScreenshot", { format: "png", clip: { ...JSON.parse(b), scale: 1.5 }, captureBeyondViewport: true });
  if (r.result?.data) fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
  console.log(file, r.result?.data ? "OK" : "FAIL");
};
await send("Page.navigate", { url: URL_ });
await sleep(6500);
await shotClip("target/ui-ref/cards/card-wfiles-light.png", "工作区文件");
await shotClip("target/ui-ref/cards/card-dyn-light.png", "动态插件");
await shotClip("target/ui-ref/cards/card-create-light.png", "创建调度");
await shotClip("target/ui-ref/cards/card-soverview-light.png", "设置概览");
await shotClip("target/ui-ref/cards/card-status-light.png", "运行时状态", 300);
await shotClip("target/ui-ref/cards/card-inventory-light.png", "插件清单", 420);
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
console.log("done");
process.exit(0);

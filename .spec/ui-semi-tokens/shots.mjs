// UI 令牌化验收：浅色默认 + 深色开关 + 关键视图卡截图（含 console 错误门槛）。
// 用法：node shots.mjs（对 60890；输出 target/ui-ref/*.png）
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const URL_ = "http://127.0.0.1:60890/";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9371;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-shots-${Date.now()}`);
fs.mkdirSync("target/ui-ref", { recursive: true });
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${prof}`,
  "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding", "--window-size=1680,1050", "about:blank"], { stdio: "ignore" });
const consoleErrs = [];
let ver = null;
for (let i = 0; i < 30 && !ver; i++) { await sleep(400); try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); process.exit(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") consoleErrs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 120));
  if (m.method === "Runtime.exceptionThrown") consoleErrs.push("EX " + String(m.params.exceptionDetails?.exception?.description ?? "").slice(0, 120));
};
await send("Page.enable"); await send("Runtime.enable");
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true }); return m.result?.result?.value; };
const shot = async (file) => {
  const r = await send("Page.captureScreenshot", { format: "png" });
  if (r.result?.data) fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
  console.log(file, r.result?.data ? "OK" : "FAIL");
};
await send("Page.navigate", { url: URL_ });
await sleep(6500);
await shot("target/ui-ref/new-light.png");
// 深色开关（body[theme-mode=dark]）
await evl(`document.body.setAttribute('theme-mode','dark'); true`);
await sleep(800);
await shot("target/ui-ref/new-dark.png");
// 复位浅色 + 展开表单卡与聊天卡特写（滚动到 DeepSeek Provider 卡）
await evl(`document.body.removeAttribute('theme-mode'); true`);
await sleep(600);
const box = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes('DeepSeek Provider')); if(!c) return null; const r=c.getBoundingClientRect(); return JSON.stringify({x:Math.max(0,r.x-6),y:Math.max(0,r.y-6),w:r.width+12,h:Math.min(r.height+12,1000)}); })()`);
if (box) {
  const b = JSON.parse(box);
  const r = await send("Page.captureScreenshot", { format: "png", clip: { x: b.x, y: b.y, width: b.w, height: b.h, scale: 1.6 } });
  if (r.result?.data) fs.writeFileSync("target/ui-ref/new-form-card.png", Buffer.from(r.result.data, "base64"));
  console.log("target/ui-ref/new-form-card.png", r.result?.data ? "OK" : "FAIL");
}
console.log("consoleErrs:", JSON.stringify(consoleErrs));
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
process.exit(consoleErrs.length ? 2 : 0);

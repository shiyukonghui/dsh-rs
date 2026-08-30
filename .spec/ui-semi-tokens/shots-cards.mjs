// 逐卡收口取证：①原版 3080 会话视图参考 ②现壳三板×双主题整板 ③关键视图卡特写（chat/list/status）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9372;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-cards-${Date.now()}`);
fs.mkdirSync("target/ui-ref/cards", { recursive: true });
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
const shot = async (file, clip) => {
  const r = await send("Page.captureScreenshot", clip ? { format: "png", clip: { ...clip, scale: 1.5 } } : { format: "png" });
  if (r.result?.data) fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
  console.log(file, r.result?.data ? "OK" : "FAIL");
};
const cardClip = async (title) => {
  const b = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes(${JSON.stringify(title)})); if(!c) return null; c.scrollIntoView({block:'center'}); const r=c.getBoundingClientRect(); return JSON.stringify({x:Math.max(0,r.x),y:Math.max(0,Math.min(r.y, 700)),width:Math.min(r.width,1660),height:Math.min(r.height,1000)}); })()`);
  return b ? JSON.parse(b) : null;
};
// —— ① 原版 3080：点进最近会话看对话视图 ——
await send("Page.navigate", { url: "http://127.0.0.1:3080/" });
await sleep(6000);
// 点第一条历史会话（dsh-rs 工作区下）
const clicked = await evl(`(() => { const items=[...document.querySelectorAll('*')].filter(e=>e.childElementCount===0 && /检查这个项目的rust服务装|根据docs\\/SERVICE-ASSEMBLY/.test(e.textContent||'')); if(!items.length) return 'no-item'; const el=items[0]; el.click(); return 'clicked:'+(el.textContent||'').slice(0,14); })()`);
console.log("orig click:", clicked);
await sleep(5000);
await shot("target/ui-ref/cards/orig-chat.png");
// —— ② 现壳：默认板（全部13卡）双主题整板 ——
await send("Page.navigate", { url: "http://127.0.0.1:60890/" });
await sleep(6000);
await shot("target/ui-ref/cards/board-base-light.png");
// 关键视图特写（浅色）：状态卡 + 列表卡（有行）+ 聊天卡
for (const [t, f] of [["运行时状态", "card-status-light"], ["设置概览", "card-list-light"], ["聊天", "card-chat-light"], ["插件清单", "card-inventory-light"]]) {
  const clip = await cardClip(t);
  if (clip) await shot(`target/ui-ref/cards/${f}.png`, clip); else console.log(f, "NO-CARD");
}
// config 板 + model 板
await evl(`location.hash='#board=config'; true`);
await sleep(2500);
await shot("target/ui-ref/cards/board-config-light.png");
// 深色整板
await evl(`location.hash=''; document.body.setAttribute('theme-mode','dark'); true`);
await sleep(2000);
await shot("target/ui-ref/cards/board-base-dark.png");
console.log("done");
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
process.exit(0);

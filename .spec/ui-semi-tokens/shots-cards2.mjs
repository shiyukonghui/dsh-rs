// 逐卡收口取证 v2：带示例数据（调度行+聊天泡），稳健截取（captureBeyondViewport）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const URL_ = "http://127.0.0.1:60890/";
const API = URL_ + "api/";
const TS = Date.now();
const MARK = `UI-OK-${TS}`;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9373;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-cards2-${TS}`);
fs.mkdirSync("target/ui-ref/cards", { recursive: true });
const rpc = (m, payload, ms = 120000) => fetch(API + m, { method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ type: "client-request", rpcId: "r", method: m, payload }), signal: AbortSignal.timeout(ms) })
  .then((r) => r.json()).then((j) => j.result);
// 种子：聊天真回复 + 调度行（1h 后触发，脚本尾部删除）
await rpc("session/prompt", { args: { sessionId: "default", text: `Reply with exactly: ${MARK}` } });
let replied = false;
for (let i = 0; i < 30 && !replied; i++) { await sleep(1500);
  const h = await rpc("session/history", { args: { sessionId: "default", limit: 200 } });
  replied = (h?.value?.events || []).some((e) => { const ev = e.event || e; return ev.type === "assistant/message" && JSON.stringify(ev.data).includes(MARK); }); }
console.log("chat seeded:", replied);
const sched = await rpc("schedule/create", { args: { kind: "after", prompt: "ui-shot-probe", afterSeconds: 3600 } });
console.log("sched seeded:", JSON.stringify(sched).slice(0, 60));
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
const shotClip = async (file, title) => {
  const b = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes(${JSON.stringify(title)})); if(!c) return null; const wb=document.getElementById('workbench'); wb.scrollTop = c.offsetTop - 10; const r=c.getBoundingClientRect(); return JSON.stringify({x:Math.round(r.x),y:Math.round(r.y),width:Math.round(r.width),height:Math.round(Math.min(r.height,900))}); })()`);
  await sleep(400);
  if (!b) { console.log(file, "NO-CARD"); return; }
  const clip = JSON.parse(b);
  const r = await send("Page.captureScreenshot", { format: "png", clip: { ...clip, scale: 1.6 }, captureBeyondViewport: true });
  if (r.result?.data) fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
  console.log(file, r.result?.data ? "OK " + clip.width + "x" + clip.height : "FAIL");
};
await send("Page.navigate", { url: URL_ });
await sleep(6500);
await shotClip("target/ui-ref/cards/card-chat-light.png", "聊天");
await shotClip("target/ui-ref/cards/card-sched-light.png", "调度任务");
await shotClip("target/ui-ref/cards/card-status-light.png", "运行时状态");
await shotClip("target/ui-ref/cards/card-session-light.png", "会话清单");
await evl(`document.body.setAttribute('theme-mode','dark'); true`);
await sleep(700);
await shotClip("target/ui-ref/cards/card-chat-dark.png", "聊天");
await shotClip("target/ui-ref/cards/card-sched-dark.png", "调度任务");
// 清理调度探针
try { const l = await rpc("schedule/list", { args: {} }, 8000);
  const pr = (l?.value?.items || []).find((x) => x.prompt === "ui-shot-probe");
  if (pr) console.log("probe deleted:", JSON.stringify(await rpc("schedule/delete", { args: { row: { id: pr.id } } }, 8000)).slice(0, 50));
} catch (e) { console.log("probe clean ERR", String(e).slice(0, 40)); }
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
console.log("done");
process.exit(0);

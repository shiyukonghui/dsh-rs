// D-216 P2 调试探针：真 rename 卸载/复原周期中，SSE 帧与页面 DOM 的双向观察。
// 判定矩阵：帧到了+DOM 跟 → 正常；帧到了+DOM 不跟 → shell 处理问题；帧没到 → 宿主广播问题。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9347;
const BASE = "http://127.0.0.1:60890";
const UNIT = path.join("wasm-plugins", "panel-locale-edit");
const OFF = path.join(".off-store", "panel-locale-edit");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(".off-store", { recursive: true });
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), process.env.PROFILE || "dsh-readd-profile")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "about:blank"], { stdio: "ignore" });
const bye = async (c) => { try { if (fs.existsSync(OFF)) fs.renameSync(OFF, UNIT); } catch {} try { proc.kill(); } catch {} process.exit(c); };
setTimeout(() => { console.log("TIMEOUT"); bye(1); }, 120000);
let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); bye(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(BASE + "/")}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map(); const hooks = [];
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === "Console.messageAdded" && hooks.length) hooks.forEach(h => h(m.params.message)); };
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Page.enable"); await send("Runtime.enable"); await send("Log.enable");
// 种子：复现审计 T7 时段的 localStorage 态（T12 遗留 config 板关闭 + locale-edit 恰被关）。
if (process.env.SEED) {
  await evl(`localStorage.setItem("dsh.canvas.closed.v2", JSON.stringify({config:["panel-locale-edit.edit"]})); "seeded"`);
  await send("Page.navigate", { url: "about:blank" });
  await sleep(400);
  await send("Page.navigate", { url: BASE + "/" });
}
await sleep(6000);
const cards = () => evl(`document.querySelectorAll('#workbench .card').length`);
const dom0 = await cards();
// 旁路探针：页面自开 EventSource 记录全部 /plugins/events 帧（含 rev 与时间）
await evl(`window.__frames=[]; const es=new EventSource("${BASE}/plugins/events"); es.onmessage=(e)=>{try{window.__frames.push(JSON.parse(e.data));}catch{window.__frames.push({raw:e.data.slice(0,80)});}}; "ok"`);
const hostCards = async () => (await (await fetch(BASE + "/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "p", method: "uiManifest/list", payload: {} }) })).json()).result?.value?.cards?.length ?? -1;
const b0 = await evl(`window.__frames.length`);
// ① 卸载（真 rename 出树）
fs.renameSync(UNIT, OFF);
for (let i = 0; i < 14 && (await cards()) === dom0; i++) await sleep(700);
const domDown = await cards();
// ② 复原（rename 回树）
fs.renameSync(OFF, UNIT);
let hostBack = -1;
for (let i = 0; i < 14; i++) { await sleep(700); hostBack = await hostCards(); if (hostBack === dom0) break; }
let domUp = domDown; let waitedMs = 0;
for (let i = 0; i < 44; i++) { await sleep(700); waitedMs += 700; domUp = await cards(); if (domUp === dom0) break; }
const frames = await evl(`JSON.stringify(window.__frames.slice(${b0}))`);
const ls = await evl(`JSON.stringify({closed:localStorage.getItem("dsh.canvas.closed.v2"),pos:localStorage.getItem("dsh.canvas.pos")})`);
console.log(JSON.stringify({ dom0, domDown, hostBack, domUp, waitedMs, ls: JSON.parse(ls || "{}"), frameTypes: JSON.parse(frames || "[]").map(f => f.type + ":" + String(f.rev ?? "").slice(0, 8)) }));
bye(0);

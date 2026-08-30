// T7 隔离探针：干净页 + rename→DOM 即时降→restore→DOM 复原（不跑审计全套，零调度副作用）。
// 用法：node e2e-hotplug.mjs <url> [期望秒数=10]
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = process.argv[2] || "http://127.0.0.1:60890/canvas/rust";
const SECS = Number(process.argv[3] || 10);
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9341;
const UNIT = path.join("wasm-plugins", "panel-locale-edit");
// 关键修正：rename-.off 仍在扫描树内会被 mount-sync 重新计回（假卸载）；
// 必须整目录移出 wasm-plugins 才是真卸载（同盘移树外，跨盘 rename 会 EPERM）。
const OFF = path.join(".off-store", "panel-locale-edit");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-hotplug-profile")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "--window-size=1400,900", "about:blank"], { stdio: "ignore" });
const bye = (c) => { try { if (fs.existsSync(OFF)) fs.renameSync(OFF, UNIT); } catch {} try { proc.kill(); } catch {} process.exit(c); };
setTimeout(() => { console.log("TIMEOUT"); bye(1); }, 120000);

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("NO CDP"); bye(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } };
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result?.result?.value;
const mfc = async () => (await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json()).result?.value?.cards?.length ?? -1;

await send("Page.enable"); await send("Runtime.enable");
await send("Page.navigate", { url: URL_ });
await sleep(4500);
const base = await evl(`document.querySelectorAll('#workbench .card').length`);
// rename 卸载（整目录移出扫描树）
fs.mkdirSync(".off-store", { recursive: true });
fs.renameSync(UNIT, OFF);
// 细粒度时间线：每 200ms 采 (rpc卡数, dom卡数)，8 秒
const tl = [];
for (let i = 0; i < 40; i++) {
  const r = await mfc();
  const d = await evl(`document.querySelectorAll('#workbench .card').length`);
  tl.push(`${i * 200}:${r}/${d}`);
  if (d <= base - 1) break;
  await sleep(200);
}
console.log("TIMELINE " + tl.join(" "));
await sleep(1500);
const m1 = await mfc();
const d1 = await evl(`document.querySelectorAll('#workbench .card').length`);
// restore
fs.renameSync(OFF, UNIT);
let d2 = d1; const t2 = Date.now();
for (let i = 0; i < 20 && d2 !== base; i++) { await sleep(500); d2 = await evl(`document.querySelectorAll('#workbench .card').length`); }
console.log(JSON.stringify({ base, m1, d1, d2, rSec: Date.now() - t2 }));
bye(0);

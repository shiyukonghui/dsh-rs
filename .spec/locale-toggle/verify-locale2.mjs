// D-225 终验：卡声明双语——点 EN 后卡标题/列名/动作英文断言（zh 复原收尾）。
const URL_ = "http://127.0.0.1:60890/";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9382;
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-loc2-${Date.now()}`);
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
let mid = 0; const pend = new Map(); const consoleErrs = [];
const send = (m, p = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method: m, params: p })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") consoleErrs.push(JSON.stringify(m.params.args).slice(0, 120));
  if (m.method === "Runtime.exceptionThrown") consoleErrs.push((m.params.exceptionDetails?.exception?.description || "").slice(0, 160)); };
await send("Page.enable"); await send("Runtime.enable");
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true }); return m.result?.result?.value; };
const bodyText = () => evl(`document.body.innerText.slice(0, 6000)`);
const click = (sel) => evl(`(() => { const b=document.querySelector(${JSON.stringify(sel)}); if(!b) return false; b.click(); return true; })()`);
const R = {};
// 确保起点 zh
await send("Page.navigate", { url: URL_ });
await sleep(8000);
let t = await bodyText();
R.zh_titles = ["待审批", "聊天", "调度任务", "动态插件"].filter((x) => t.includes(x)).length;
// 若当前 en（残留），先切回 zh
if (R.zh_titles === 0 && t.includes("Pending Approvals")) { await click("#lang-toggle"); await sleep(2500); t = await bodyText(); R.zh_titles = ["待审批", "聊天", "调度任务", "动态插件"].filter((x) => t.includes(x)).length; }
// 点 EN → 卡标题英文（ltext 解析双声明即时切换，无需刷新）
await click("#lang-toggle");
await sleep(2500);
t = await bodyText();
R.en_card_titles = ["Pending Approvals", "Chat", "Scheduled Tasks", "Dynamic Plugins", "Workspace Files", "Settings Overview"].filter((x) => t.includes(x)).length;
R.en_cols = ["Planned At", "Package", "Plugin Inventory"].filter((x) => t.includes(x)).length;
R.en_actions = ["Start", "Stop", "Unload"].filter((x) => t.includes(x)).length;
R.no_zh_card_title = !/待审批|调度任务|动态插件|工作区文件/.test(t);
// 复原 zh
await click("#lang-toggle");
await sleep(2500);
t = await bodyText();
R.back_zh = ["待审批", "聊天", "调度任务"].filter((x) => t.includes(x)).length;
R.consoleErrs = consoleErrs.slice(0, 4);
fs.writeFileSync("target/ui-ref/locale-verify2.json", JSON.stringify(R, null, 1));
console.log(JSON.stringify(R));
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
const ok = R.zh_titles === 4 && R.en_card_titles >= 5 && R.en_cols >= 2 && R.en_actions >= 2 && R.no_zh_card_title && R.back_zh === 3 && R.consoleErrs.length === 0;
console.log(ok ? "PASS" : "FAIL");
process.exit(ok ? 0 : 1);

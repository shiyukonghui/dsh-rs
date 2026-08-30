// D-225 验证：顶栏点击切换 zh↔en + 持久化（DOM 断言，非目检）。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const URL_ = "http://127.0.0.1:60890/";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9381;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-loc-${Date.now()}`);
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
const bodyText = () => evl(`document.body.innerText.slice(0, 4000)`);
const click = (sel) => evl(`(() => { const b=document.querySelector(${JSON.stringify(sel)}); if(!b) return false; b.click(); return true; })()`);
const R = {};
await send("Page.navigate", { url: URL_ });
await sleep(7000);
let t = await bodyText();
R.zh_h1 = t.includes("服务装配单元");
R.zh_all = t.includes("全部（13）");
R.toggle_exists = await evl(`!!document.querySelector('#lang-toggle')`);
R.toggle_label = await evl(`(document.querySelector('#lang-toggle')||{}).textContent`);
// 点击 → EN
R.clicked = await click("#lang-toggle");
await sleep(2500);
t = await bodyText();
R.en_h1 = t.includes("Service Assembly Canvas");
R.en_all = /All（13）|All \(/.test(t);
R.en_chat_btn = t.includes("Send") && t.includes("Stop");
// 重置钮仅在有钉位时渲染（has_pins 守卫）——干净板下缺席=预期，断言改双态。
R.en_reset = t.includes("Reset layout") || !t.includes("重置摆位");
// 刷新 → 持久化（settings 权威）
await send("Page.navigate", { url: URL_ });
await sleep(7000);
t = await bodyText();
R.persist_en = t.includes("Service Assembly Canvas");
// 点「中」→ 复原
await click("#lang-toggle");
await sleep(2500);
t = await bodyText();
R.back_zh = t.includes("服务装配单元") && t.includes("全部（13）");
R.consoleErrs = consoleErrs;
fs.writeFileSync("target/ui-ref/locale-verify.json", JSON.stringify(R, null, 1));
console.log(JSON.stringify(R, null, 1));
proc.kill();
try { await sleep(500); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
process.exit(Object.entries(R).filter(([k, v]) => k !== "toggle_label" && k !== "clicked" && k !== "toggle_exists" && v !== true).length === 0 ? 0 : 1);

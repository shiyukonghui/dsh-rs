// S6 交互级双壳对打审计：同一交互序列在给定 URL 上执行，输出逐项 PASS/FAIL。
// 用法：node e2e-audit.mjs <url>（对 /canvas 与 /canvas/rust 各跑一次即为对打）。
// 测试面：关闭/重开、fieldsFrom+nsSelect 重投影、表单动作诚实错误、chat 乐观气泡、
// 热插拔 SSE DOM 即时更新（rename→12→restore→13）。全程自恢复现场。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = process.argv[2] || "http://127.0.0.1:60890/canvas";
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9337;
const UNIT = path.join("wasm-plugins", "panel-locale-edit");
const OFF = UNIT + ".off";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-audit-profile")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "--window-size=1600,1000", "about:blank"], { stdio: "ignore" });
const bye = (c) => { try { console.log("PARTIAL " + JSON.stringify(R)); } catch {} try { proc.kill(); } catch {} try { if (fs.existsSync(OFF)) fs.renameSync(OFF, UNIT); } catch {} process.exit(c); };
setTimeout(() => { console.log("AUDIT TIMEOUT"); bye(1); }, 240000);

let ver = null;
for (let i = 0; i < 60 && !ver; i++) { await sleep(500);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { console.log("FAIL CDP"); bye(1); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("ws")); });
let mid = 0; const pend = new Map(); const consoleErrs = [];
ws.onmessage = (e) => { const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); return; }
  if (m.method === "Runtime.exceptionThrown")
    consoleErrs.push(String(m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text || "").slice(0, 120));
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") {
    const t = (m.params.args || []).map(a => a.value ?? a.description).join(" ");
    if (!t.includes("/_dioxus")) consoleErrs.push("CE:" + t.slice(0, 120));
  }
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  return r.result?.result?.value ?? ("EVALERR:" + JSON.stringify(r.result ?? r).slice(0, 160));
};
await send("Page.enable"); await send("Runtime.enable");
await send("Page.navigate", { url: URL_ });
await sleep(4500);
// Preflight：共享 profile 可能被上一轮污染——先全量重开灰显卡再测（自愈）。
for (let i = 0; i < 3; i++) {
  const shut = await evl(`document.querySelectorAll('#sidebar .name.shut').length`);
  if (!shut) break;
  await evl(`[...document.querySelectorAll('#sidebar .name.shut')].forEach(n=>n.click()); 'r'`);
  await sleep(800);
}

const R = {};
// T0 基线
R.T0_cards = await evl(`document.querySelectorAll('#workbench .card').length`);
// T10 几何不变式：开放卡两两矩形零重叠（实测布局硬约束，1px 容差）
R.T10_overlap = await evl(`(()=>{
  const rs=[...document.querySelectorAll('#workbench .card')].map(c=>{const r=c.getBoundingClientRect();return {l:r.left,t:r.top,r:r.right,b:r.bottom};});
  let ov=0;
  for(let i=0;i<rs.length;i++)for(let j=i+1;j<rs.length;j++){const a=rs[i],b=rs[j];
    if(a.l<b.r-1&&b.l<a.r-1&&a.t<b.b-1&&b.t<a.b-1)ov++;}
  return JSON.stringify({cards:rs.length,overlaps:ov});
})()`);
// T2 ✕ 关闭
R.T2_close = await evl(`(async()=>{
  const b=document.querySelector('#workbench .card .card-close'); if(!b) return 'NO-BTN';
  b.click(); await new Promise(r=>setTimeout(r,400));
  return JSON.stringify({cards:document.querySelectorAll('#workbench .card').length, shut:document.querySelectorAll('#sidebar .name.shut').length, ls:(JSON.parse(localStorage.getItem('dsh.canvas.closed')||'[]')).length});
})()`);
// T3 侧栏重开
R.T3_reopen = await evl(`(async()=>{
  const n=document.querySelector('#sidebar .name.shut'); if(!n) return 'NO-SHUT';
  n.click(); await new Promise(r=>setTimeout(r,500));
  return JSON.stringify({cards:document.querySelectorAll('#workbench .card').length, shut:document.querySelectorAll('#sidebar .name.shut').length});
})()`);
// T4 fieldsFrom + nsSelect：切到 llm 后字段面应现 provider 域（apiKey/baseURL/model/provider）
R.T4_nsSelect = await evl(`(async()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>[...c.querySelectorAll('select')].some(s=>[...s.options].some(o=>o.value==='llm')));
  if(!card) return 'NO-CARD';
  const sel=[...card.querySelectorAll('select')].find(s=>[...s.options].some(o=>o.value==='llm'));
  sel.value='llm'; sel.dispatchEvent(new Event('change',{bubbles:true}));
  await new Promise(r=>setTimeout(r,1400));
  const t=card.textContent;
  return JSON.stringify({hasProvider:t.includes('provider'), hasBaseURL:t.includes('baseURL')||t.includes('Base URL')});
})()`);
// T5 表单动作→宿主诚实错误（schedule host 未装配）
R.T5_honestErr = await evl(`(async()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>c.textContent.includes('创建调度'));
  if(!card) return 'NO-CARD';
  const inp=card.querySelector('input[name=prompt]'); if(!inp) return 'NO-INPUT';
  inp.value='e2e-audit-probe';
  const go=[...card.querySelectorAll('button')].find(b=>b.textContent.includes('创建'));
  if(!go) return 'NO-BTN'; go.click();
  await new Promise(r=>setTimeout(r,1400));
  return JSON.stringify({honest:/no-schedule-host|not assembled|取消|失败|✗/.test(card.textContent)});
})()`);
// T6 chat 乐观气泡
R.T6_chat = await evl(`(async()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>c.textContent.includes('聊天')&&c.querySelector('.chat-send,input'));
  if(!card) return 'NO-CARD';
  const inp=card.querySelector('input'); if(!inp) return 'NO-INPUT';
  inp.value='e2e-audit';
  const go=[...card.querySelectorAll('button')].find(b=>b.textContent.includes('发送'));
  if(!go) return 'NO-BTN'; go.click();
  await new Promise(r=>setTimeout(r,1200));
  return JSON.stringify({bubbles:card.querySelectorAll('.chat-bubble').length});
})()`);
// T7 热插拔：rename → manifest=12 → DOM 即时降 → restore → DOM 回 13
let t7 = { note: "skipped" };
try {
  const m0 = await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json().then(j => j.result?.value?.cards?.length ?? -1);
  fs.renameSync(UNIT, OFF);
  let m1 = m0; for (let i = 0; i < 14 && m1 === m0; i++) { await sleep(600); m1 = await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json().then(j => j.result?.value?.cards?.length ?? -1); }
  let dom1 = m0; for (let i = 0; i < 10 && dom1 === m0; i++) { await sleep(700); dom1 = await evl(`document.querySelectorAll('#workbench .card').length`); }
  fs.renameSync(OFF, UNIT);
  let dom2 = dom1; for (let i = 0; i < 14 && dom2 !== m0; i++) { await sleep(700); dom2 = await evl(`document.querySelectorAll('#workbench .card').length`); }
  t7 = { m1, dom1, dom2 };
} catch (e) { t7 = { err: String(e).slice(0, 120) }; }
R.T7_hotplug = JSON.stringify(t7);
// T9 关闭持久化：关一卡 → reload → 仍闭（ls 恢复）→ 重开复原
R.T9_reloadPersist = "FAIL";
try {
  await evl(`(()=>{const b=document.querySelector('#workbench .card .card-close'); b&&b.click(); return 'c';})()`);
  await sleep(500);
  const closedCnt = await evl(`JSON.parse(localStorage.getItem('dsh.canvas.closed')||'[]').length`);
  await send("Page.navigate", { url: URL_ });
  await sleep(4500);
  const afterCards = await evl(`document.querySelectorAll('#workbench .card').length`);
  const shutCnt = await evl(`document.querySelectorAll('#sidebar .name.shut').length`);
  await evl(`(()=>{const n=document.querySelector('#sidebar .name.shut'); n&&n.click(); return 'r';})()`);
  await sleep(600);
  const backCards = await evl(`document.querySelectorAll('#workbench .card').length`);
  R.T9_reloadPersist = JSON.stringify({ closedCnt, afterCards, shutCnt, backCards });
} catch (e) { R.T9_reloadPersist = "ERR:" + String(e).slice(0, 80); }
// T8 清场：closed 全恢复 + localStorage 干净
R.T8_cleanup = await evl(`(()=>{localStorage.setItem('dsh.canvas.closed','[]');return 'ok'})()`);
R.consoleErrs = consoleErrs.slice(0, 5);

console.log("AUDIT " + URL_);
console.log(JSON.stringify(R, null, 1));
try { ws.close(); } catch {}
bye(0);

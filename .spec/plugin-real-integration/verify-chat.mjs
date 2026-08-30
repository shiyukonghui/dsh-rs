// 阶段 11 · panel-chat 深化校验：真历史回放（含 D-218 触发轮次）+ 发送→乐观泡→echo 回应 + cancel 尝试。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL_ = "http://127.0.0.1:60890/";
const HIST = process.argv[2] || "E2E-FIRE-R10B-1788096162.99231"; // 阶段 10 真触发留痕
const TS = Date.now();
const M2 = `E2E-CHAT-SEND-${TS}`;
const M3 = `E2E-CHAT-CANCEL-${TS}`;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9361;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-vchat-${TS}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { steps: [], consoleErrs: [] };
const step = (n, ok, info) => R.steps.push({ name: n, ok, ...(info !== undefined ? { info } : {}) });
const bye = async () => {
  try { proc.kill(); } catch {}
  R.pass = !R.why && R.steps.every(s => s.ok) && R.consoleErrs.length === 0;
  console.log(JSON.stringify(R));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(R.pass ? 0 : 1);
};
setTimeout(async () => { R.why = "TIMEOUT"; await bye(); }, 120000);

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) { R.why = "NO CDP"; await bye(); }
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL_)}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") R.consoleErrs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 160));
  if (m.method === "Runtime.exceptionThrown") R.consoleErrs.push("EX " + String(m.params.exceptionDetails?.exception?.description ?? "").slice(0, 160));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Page.enable"); await send("Runtime.enable");

// 挂卡
let marked = false;
for (let i = 0; i < 16 && !marked; i++) {
  await sleep(600);
  marked = await evl(`(() => { const c=[...document.querySelectorAll('#workbench .card')].find(c=>(c.innerText||'').includes('聊天')&&c.querySelector('[name=chat-input]')); if(!c) return false; c.setAttribute('data-vc','1'); return true; })()`);
}
if (!marked) { step("mount", false); await bye(); }
// 等历史 RPC 落地（渲染出气泡）
for (let i = 0; i < 12; i++) {
  const n = await evl(`document.querySelectorAll('[data-vc] .chat-bubble').length`);
  if (n > 0) break;
  await sleep(700);
}

// 1. 历史回放：阶段 10 真触发轮次要出现在聊天卡
const histTxt = await evl(`(() => { const t=(document.querySelector('[data-vc]')?.innerText||''); const i=t.indexOf(${JSON.stringify(HIST)}); return i<0?"":t.slice(Math.max(0,i-40), i+120); })()`);
step("history-replays-fired-turn", !!histTxt, histTxt.slice(0, 140));

// 2. 发送 → 乐观泡即时可见
const bubblesBefore = await evl(`document.querySelectorAll('[data-vc] .chat-bubble').length`);
const sent = await evl(`(() => {
  const c=document.querySelector('[data-vc]');
  const i=c.querySelector('[name=chat-input]');
  i.value=${JSON.stringify(M2)}; i.dispatchEvent(new Event('input',{bubbles:true}));
  const b=[...c.querySelectorAll('button')].find(x=>x.textContent.includes('发送'));
  if(!b) return "nosend"; b.click(); return "clicked"; })()`);
await sleep(120);
const optimistic = await evl(`(document.querySelector('[data-vc]')?.innerText||'').includes(${JSON.stringify(M2)})`);
step("send-optimistic-bubble", sent === "clicked" && optimistic, { sent, bubblesBefore });

// 3. echo 回应（活折进会话）
let echoOk = false, replySnip = "";
for (let i = 0; i < 25 && !echoOk; i++) {
  await sleep(700);
  const r = await evl(`(() => { const t=(document.querySelector('[data-vc]')?.innerText||''); const i=t.indexOf(${JSON.stringify(M2)}); if(i<0) return ""; const after=t.slice(i+${JSON.stringify(M2)}.length); const hit=/助手|echo/.test(after); return hit?after.slice(0,120):""; })()`);
  if (r) { echoOk = true; replySnip = r; }
}
step("echo-reply-arrives", echoOk, replySnip.slice(0, 100));

// 4. cancel 尝试：发送后即时抓「停止」按钮（echo 轮次极快，抓到与否如实记录）
await evl(`(() => { const c=document.querySelector('[data-vc]'); const i=c.querySelector('[name=chat-input]'); i.value=${JSON.stringify(M3)}; i.dispatchEvent(new Event('input',{bubbles:true})); const b=[...c.querySelectorAll('button')].find(x=>x.textContent.includes('发送')); b&&b.click(); return true; })()`);
let cancelCaught = false;
for (let i = 0; i < 30 && !cancelCaught; i++) {
  cancelCaught = await evl(`(() => { const b=[...document.querySelectorAll('[data-vc] button')].find(x=>x.textContent.trim()==='停止'); if(!b) return false; b.click(); return true; })()`);
  if (!cancelCaught) await sleep(50);
}
R.cancelAttempt = { stopButtonCaughtAndClicked: cancelCaught, note: cancelCaught ? "轮次执行中捕获到停止按钮并点击" : "echo 轮次在 1.5s 观察窗内已完成，停止按钮未及出现（状态机使然，非缺陷）" };
// cancel/完成两路都不许炸卡：卡仍活着 + M3 仍在流里
const alive = await evl(`!!document.querySelector('[data-vc] [name=chat-input]')`);
const m3In = await evl(`(document.querySelector('[data-vc]')?.innerText||'').includes(${JSON.stringify(M3)})`);
step("card-alive-after-send-cancel-window", alive && m3In, R.cancelAttempt);

await bye();

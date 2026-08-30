// 动作面（写侧）浏览器校验器（模板自阶段 6；阶段 6/7/13 等表单卡复用）。
// 流程：设值→保存(✓)→重载→概览卡显新值→回存原值(✓)→不重载再存(✗ 冲突=乐观锁证)→重载→概览复原。
// 用法: node verify-action-form.mjs [--url URL] [--field preference] [--edit-title 设置编辑]
//       [--row-ns ui-theme]   --row-ns=概览行首列精确锁 ns（防字段名子串误配，如 en⊂preference）
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf("--" + k); return i >= 0 ? args[i + 1] : d; };
const URL_ = opt("url", "http://127.0.0.1:60890/");
const FIELD = opt("field", "preference");
const EDIT_TITLE = opt("edit-title", "设置编辑");
const ROW_NS = opt("row-ns", null);

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = Number(opt("port", "9354"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-va-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu",
  "--window-size=1600,1000", "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const R = { steps: [], consoleErrs: [] };
const bye = async () => {
  try { proc.kill(); } catch {}
  R.pass = R.steps.every(s => s.ok) && R.consoleErrs.length === 0;
  console.log(JSON.stringify(R));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(R.pass ? 0 : 1);
};
const step = (name, ok, info) => R.steps.push({ name, ok, ...(info ? { info } : {}) });
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
await send("Runtime.enable"); await send("Page.enable");

const mark = async () => {
  for (let i = 0; i < 14; i++) {
    await sleep(600);
    const ok = await evl(`(() => {
      const cards=[...document.querySelectorAll('#workbench .card')];
      const e=cards.find(c=>(c.innerText||'').includes(${JSON.stringify(EDIT_TITLE)}));
      const s=cards.find(c=>(c.innerText||'').includes('设置概览'));
      if(!e||!s) return false;
      e.setAttribute('data-va','edit'); s.setAttribute('data-va','summary'); return true; })()`);
    if (ok) return true;
  }
  return false;
};
const reload = async () => { await send("Page.navigate", { url: "about:blank" }); await sleep(400); await send("Page.navigate", { url: URL_ }); };
const actOf = () => evl(`(() => { const c=document.querySelector('[data-va=edit]'); const t=(c?.innerText||''); const l=t.split('\\n').find(l=>l.trim().startsWith('✓')||l.trim().startsWith('✗')); return l||""; })()`);

// 1. 进场 + 读当前值 + 选备选值
if (!(await mark())) { step("mount", false); await bye(); }
const fieldsJson = await evl(`JSON.stringify([...document.querySelectorAll('[data-va=edit] input,[data-va=edit] select,[data-va=edit] textarea')].map(e=>({n:e.name,t:e.tagName.toLowerCase()==='select'?'select':(e.type||'text'),v:e.value,opts:e.tagName.toLowerCase()==='select'?[...e.options].map(o=>o.value):undefined})))`);
const fields = JSON.parse(fieldsJson || "[]");
const f = fields.find(x => x.n === FIELD);
if (!f) { step("field-found", false, fieldsJson); await bye(); }
const cur = f.v;
const alt = (f.opts && f.opts.find(o => o !== cur)) || (cur === "dark" ? "light" : "dark");
step("field-found", true, { cur, alt, kind: f.t });

// 2. 设 alt → 保存 → act ✓
let set = await evl(`(() => { const i=document.querySelector('[data-va=edit] [name=${JSON.stringify(FIELD)}]'); if(!i) return 'noinput'; i.value=${JSON.stringify(alt)}; i.dispatchEvent(new Event('input',{bubbles:true})); i.dispatchEvent(new Event('change',{bubbles:true})); return i.value; })()`);
step("set-value", set === alt, set);
let clicked = await evl(`(() => { const b=[...document.querySelectorAll('[data-va=edit] button')].find(x=>x.textContent.includes('保存')); if(!b) return false; b.click(); return true; })()`);
await sleep(1600);
let act = await actOf();
step("save1", clicked && act.includes("✓"), act);

// 3. 重载 → 概览卡显新值（浏览器二确；数据面 RPC 异步到卡，轮询抗渲染竞态）
await reload();
if (!(await mark())) { step("reload1-mount", false); await bye(); }
let row1 = "";
const rowQ = (val) => `(() => { const c=document.querySelector('[data-va=summary]'); return ((c?.innerText||'').split('\\n').find(l=>l.includes(${JSON.stringify(FIELD)})&&l.includes(${JSON.stringify(val)})${ROW_NS ? `&&l.startsWith(${JSON.stringify(ROW_NS + "\t")})` : ""})||""); })()`;
for (let i = 0; i < 8 && !row1; i++) {
  await sleep(700);
  row1 = await evl(rowQ(alt));
}
step("summary-shows-alt", !!row1, row1);

// 4. 回存原值 ✓ → 不重载立刻再存 → 期望 ✗（乐观锁 stale revision）
set = await evl(`(() => { const i=document.querySelector('[data-va=edit] [name=${JSON.stringify(FIELD)}]'); if(!i) return 'noinput'; i.value=${JSON.stringify(cur)}; i.dispatchEvent(new Event('input',{bubbles:true})); return i.value; })()`);
await evl(`(() => { const b=[...document.querySelectorAll('[data-va=edit] button')].find(x=>x.textContent.includes('保存')); b&&b.click(); return true; })()`);
await sleep(1600);
act = await actOf();
step("save-restore", set === cur && act.includes("✓"), act);
await evl(`(() => { const b=[...document.querySelectorAll('[data-va=edit] button')].find(x=>x.textContent.includes('保存')); b&&b.click(); return true; })()`);
await sleep(1600);
let act2 = await actOf();
step("conflict-on-stale", act2.includes("✗"), act2);

// 5. 重载 → 概览复原（轮询抗渲染竞态）
await reload();
if (!(await mark())) { step("reload2-mount", false); await bye(); }
let row2 = "";
for (let i = 0; i < 8 && !row2; i++) {
  await sleep(700);
  row2 = await evl(rowQ(cur));
}
step("summary-restored", !!row2, row2);

await bye();

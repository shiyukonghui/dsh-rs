// E2E #1: interaction-level browser smoke (raw CDP, zero-dep). Builds on e2e-cdp.mjs.
// Flows: nsSelect repaint -> create-schedule form honest roundtrip -> sidebar filter.
import { spawn } from "node:child_process";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9334;
const URL_ = "http://127.0.0.1:60890/canvas";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try {
  const r = await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ type: "client-request", rpcId: "e", method: "uiManifest/list", payload: {} }) });
  const j = await r.json();
  console.log("manifest cards=" + (j.result?.value?.cards?.length ?? "?"));
} catch (e) { console.log("FAIL server: " + e.message); process.exit(1); }

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-e2e-profile2")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "--window-size=1600,1000", "about:blank"], { stdio: "ignore" });
const bye = (c) => { try { proc.kill(); } catch {} process.exit(c); };
setTimeout(() => { console.log("FAIL timeout"); bye(1); }, 90000);

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
    consoleErrs.push("EX:" + (m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text).slice(0, 150));
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error")
    consoleErrs.push("CE:" + (m.params.args || []).map(a => a.value ?? a.description).join(" ").slice(0, 150));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  return r.result?.result?.value ?? ("EVALERR:" + JSON.stringify(r).slice(0, 200));
};

await send("Page.enable"); await send("Runtime.enable");
await send("Page.addScriptToEvaluateOnNewDocument", {
  source: 'window.__errs=[];window.addEventListener("error",e=>window.__errs.push(String(e.message)));' });
await send("Page.navigate", { url: URL_ });
await sleep(4000);

// T1: nsSelect repaint on the settings-edit card (pick -> locale).
const t1 = await evl(`(async () => {
  const card = [...document.querySelectorAll('.card')].find(c => c.textContent.includes('设置编辑')
    && [...c.querySelectorAll('select')].some(s => [...s.options].some(o => o.value === 'ui-theme')));
  if (!card) return 'NO-CARD';
  const sel = [...card.querySelectorAll('select')].find(s => [...s.options].some(o => o.value === 'ui-theme'));
  const before = [...card.querySelectorAll('label span')].map(e => e.textContent).join('|');
  if (![...sel.options].some(o => o.value === 'shell')) return 'NO-shell-opt:' + [...sel.options].map(o=>o.value).join(',');
  sel.value = 'shell';
  sel.dispatchEvent(new Event('change', { bubbles: true }));
  await new Promise(r => setTimeout(r, 1500));
  const after = [...card.querySelectorAll('label span')].map(e => e.textContent).join('|');
  return JSON.stringify({ opts: sel.options.length, changed: before !== after, before: before.slice(0, 60), after: after.slice(0, 60) });
})()`);
console.log("T1-nsSelect " + JSON.stringify(t1));

// T2: create-schedule form honest roundtrip (agent loop off -> honest error, no fake ok).
const t2 = await evl(`(async () => {
  const card = [...document.querySelectorAll('.card')].find(c => c.textContent.includes('创建调度'));
  if (!card) return 'NO-CARD';
  const inp = card.querySelector('input[type=text]') || [...card.querySelectorAll('input')].find(i => i.type !== 'checkbox' && i.type !== 'password');
  if (!inp) return 'NO-INPUT';
  inp.value = 'e2e-probe-task';
  const go = [...card.querySelectorAll('button')].find(b => b.textContent.includes('创建'));
  if (!go) return 'NO-BUTTON';
  go.click();
  await new Promise(r => setTimeout(r, 1200));
  const st = [...card.querySelectorAll('.stat,.note,div')].map(e => e.textContent).join(' ');
  return JSON.stringify({ honest: /no-schedule-host|not assembled|✗/i.test(st), snippet: st.slice(-120) });
})()`);
console.log("T2-form " + JSON.stringify(t2));

// T3: sidebar category filter (runtime) then back to all.
const t3 = await evl(`(async () => {
  const all0 = document.querySelectorAll('#workbench .card').length;
  const btns = [...document.querySelectorAll('#sidebar button')];
  const rt = btns.find(b => b.textContent.includes('runtime'));
  if (!rt) return 'NO-RUNTIME-BTN';
  rt.click();
  await new Promise(r => setTimeout(r, 500));
  const shown = document.querySelectorAll('#workbench .card').length;
  const allBtn = [...document.querySelectorAll('#sidebar button')].find(b => b.textContent.includes('全部'));
  allBtn.click();
  await new Promise(r => setTimeout(r, 500));
  const back = document.querySelectorAll('#workbench .card').length;
  return JSON.stringify({ all0, runtime: shown, back });
})()`);
console.log("T3-filter " + JSON.stringify(t3));

const shot = await send("Page.captureScreenshot", { format: "png" });
const fs = await import("node:fs");
const out = path.join(".spec", "service-assembly-ui-panels", "e2e-shots", "canvas-02-interact.png");
fs.writeFileSync(out, Buffer.from(shot.result.data, "base64"));
console.log("SHOT " + out);
console.log("CONSOLE " + JSON.stringify(consoleErrs.slice(0, 10)));
try { ws.close(); } catch {}
bye(0);

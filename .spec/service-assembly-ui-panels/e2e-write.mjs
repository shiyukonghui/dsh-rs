// E2E #2: write-path browser flows (raw CDP, zero-dep).
// W1 settings save ok -> W2 double-save SETTINGS_CONFLICT -> W3 password present+empty-save
// -> W4 hot-plug rename/restore (manifest + live DOM without reload).
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9335;
const URL_ = "http://127.0.0.1:60890/canvas";
const UNIT = path.join("wasm-plugins", "panel-locale-edit");
const OFF = UNIT + ".off";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const manifestCount = async () => {
  const r = await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ type: "client-request", rpcId: "e", method: "uiManifest/list", payload: {} }) });
  const j = await r.json();
  return j.result?.value?.cards?.length ?? -1;
};
try { console.log("manifest=" + (await manifestCount())); } catch (e) { console.log("FAIL server: " + e.message); process.exit(1); }

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-e2e-profile3")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "--window-size=1600,1000", "about:blank"], { stdio: "ignore" });
const bye = (c) => { try { proc.kill(); } catch {} try { if (fs.existsSync(OFF)) fs.renameSync(OFF, UNIT); } catch {} process.exit(c); };
setTimeout(() => { console.log("FAIL timeout"); bye(1); }, 120000);

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
    consoleErrs.push("EX:" + String(m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text).slice(0, 150));
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error")
    consoleErrs.push("CE:" + (m.params.args || []).map(a => a.value ?? a.description).join(" ").slice(0, 150));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  return r.result?.result?.value ?? ("EVALERR:" + JSON.stringify(r).slice(0, 200));
};
await send("Page.enable"); await send("Runtime.enable");
await send("Page.navigate", { url: URL_ });
await sleep(4000);
// SSE 隔离探针：页面自开一条 /plugins/events 抓帧（与渲染器同款默认 onmessage）。
await evl(`(()=>{ window.__sse=[]; const es=new EventSource("/plugins/events");
  es.onmessage=(e)=>window.__sse.push(String(e.data).slice(0,80)); return "armed"; })()`);

// Pick the generic settings-edit card (has ns picker containing 'llm').
const CARD = `(document.querySelector('#workbench'))`;
const openNs = async (ns) => evl(`(async () => {
  const card=[...document.querySelectorAll('.card')].find(c=>[...c.querySelectorAll('select')].some(s=>[...s.options].some(o=>o.value==='llm')));
  if(!card) return 'NO-CARD';
  const sel=[...card.querySelectorAll('select')].find(s=>[...s.options].some(o=>o.value==='llm'));
  sel.value=${JSON.stringify(ns)}; sel.dispatchEvent(new Event('change',{bubbles:true}));
  await new Promise(r=>setTimeout(r,1200));
  return 'OK:'+card.textContent.slice(0,40);
})()`);
const clickSave = async () => evl(`(async () => {
  const card=[...document.querySelectorAll('.card')].find(c=>[...c.querySelectorAll('select')].some(s=>[...s.options].some(o=>o.value==='llm')));
  if(!card) return 'NO-CARD';
  const go=[...card.querySelectorAll('button')].find(b=>b.textContent.includes('保存'));
  if(!go) return 'NO-SAVE';
  go.click();
  await new Promise(r=>setTimeout(r,1200));
  return JSON.stringify([...card.querySelectorAll('.cstat')].map(e=>e.textContent).slice(0,3));
})()`);
const pwProbe = async () => evl(`(()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>[...c.querySelectorAll('select')].some(s=>[...s.options].some(o=>o.value==='llm')));
  const p=card&&card.querySelector('input[type=password]');
  return p?('PW placeholder='+p.placeholder+' value='+JSON.stringify(p.value)):'NO-PASSWORD';
})()`);

console.log("W1-open-llm " + JSON.stringify(await openNs("llm")));
console.log("W1b-password-on-llm " + JSON.stringify(await evl(`(()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>[...c.querySelectorAll('select')].some(s=>[...s.options].some(o=>o.value==='llm')));
  const p=card&&card.querySelector('input[type=password]');
  return p?('PW placeholder='+p.placeholder+' value='+JSON.stringify(p.value)):'NO-PASSWORD';
})()`)));
console.log("W2-save1 " + JSON.stringify(await clickSave()));
console.log("W3-save2-conflict " + JSON.stringify(await clickSave()));
console.log("W4-open-llm-deepseek " + JSON.stringify(await openNs("llm-deepseek")));
console.log("W5-password " + JSON.stringify(await pwProbe()));
console.log("W6-save-empty-secret " + JSON.stringify(await clickSave()));

// W7 hot-plug: rename unit dir -> manifest drops -> DOM drops WITHOUT reload -> restore.
const m0 = await manifestCount();
fs.renameSync(UNIT, OFF);
let m1 = m0; for (let i = 0; i < 12 && m1 === m0; i++) { await sleep(600); m1 = await manifestCount(); }
let dom1 = 13; for (let i = 0; i < 12 && dom1 === 13; i++) { await sleep(700); dom1 = await evl(`document.querySelectorAll('#workbench .card').length`); }
fs.renameSync(OFF, UNIT);
let m2 = m1; for (let i = 0; i < 12 && m2 === m1; i++) { await sleep(600); m2 = await manifestCount(); }
await sleep(2500);
const dom2 = await evl(`document.querySelectorAll('#workbench .card').length`);
console.log("W7-hotplug " + JSON.stringify({ m0, m1, dom1, m2, dom2 }));
console.log("W8-sse-frames " + JSON.stringify(await evl(`JSON.stringify(window.__sse)`)));

const shot = await send("Page.captureScreenshot", { format: "png" });
fs.mkdirSync(path.join(".spec", "service-assembly-ui-panels", "e2e-shots"), { recursive: true });
const out = path.join(".spec", "service-assembly-ui-panels", "e2e-shots", "canvas-03-write.png");
fs.writeFileSync(out, Buffer.from(shot.result.data, "base64"));
console.log("SHOT " + out);
console.log("CONSOLE " + JSON.stringify(consoleErrs.slice(0, 10)));
try { ws.close(); } catch {}
bye(0);

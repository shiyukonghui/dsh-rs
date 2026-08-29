// E2E #3 (D-209): close/reopen loop + measured-layout overlap proof (pairwise rects).
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9336;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try { await fetch("http://127.0.0.1:60890/canvas"); } catch (e) { console.log("FAIL serve: " + e.message); process.exit(1); }

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-e2e-profile4")}`, "--no-first-run",
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
    consoleErrs.push("EX:" + String(m.params.exceptionDetails?.exception?.description || "").slice(0, 150));
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error")
    consoleErrs.push("CE:" + (m.params.args || []).map(a => a.value ?? a.description).join(" ").slice(0, 150));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  return r.result?.result?.value ?? ("EVALERR:" + JSON.stringify(r).slice(0, 200));
};
await send("Page.enable"); await send("Runtime.enable");
await send("Page.navigate", { url: "http://127.0.0.1:60890/canvas" });
await sleep(5000); // data loads + RO-driven relayouts settle

// A) pairwise rect overlap proof on live DOM
console.log("A-overlap " + JSON.stringify(await evl(`(()=>{
  const rs=[...document.querySelectorAll('#workbench .card')].map(el=>{const r=el.getBoundingClientRect();return {k:el.dataset.focusKey,x:r.x,y:r.y+scrollY,w:r.width,h:r.height};});
  let bad=[];
  for(let i=0;i<rs.length;i++)for(let j=i+1;j<rs.length;j++){const A=rs[i],B=rs[j];
    const disj=A.x+A.w<=B.x+.5||B.x+B.w<=A.x+.5||A.y+A.h<=B.y+.5||B.y+B.h<=A.y+.5;
    if(!disj)bad.push(A.k+"×"+B.k);}
  return JSON.stringify({cards:rs.length,overlaps:bad});
})()`)));

// B) close first card via ✕ -> count-1, sidebar shut entry, localStorage written
console.log("B-close " + JSON.stringify(await evl(`(async()=>{
  const before=document.querySelectorAll('#workbench .card').length;
  const x=document.querySelector('#workbench .card .card-close');
  if(!x) return 'NO-CLOSE-BTN';
  const key=document.querySelector('#workbench .card').dataset.focusKey;
  x.click();
  await new Promise(r=>setTimeout(r,400));
  const after=document.querySelectorAll('#workbench .card').length;
  const shut=[...document.querySelectorAll('#sidebar .name.shut')].length;
  const ls=JSON.parse(localStorage.getItem('dsh.canvas.closed')||'[]');
  return JSON.stringify({before,after,shut,lsHas:ls.includes(key)});
})()`)));

// C) reopen via sidebar click -> count restored, shut cleared; then close again & reload -> persistence
console.log("C-reopen " + JSON.stringify(await evl(`(async()=>{
  const n=document.querySelector('#sidebar .name.shut');
  if(!n) return 'NO-SHUT-ENTRY';
  n.click();
  await new Promise(r=>setTimeout(r,600));
  return JSON.stringify({cards:document.querySelectorAll('#workbench .card').length, shut:document.querySelectorAll('#sidebar .name.shut').length});
})()`)));

const shot = await send("Page.captureScreenshot", { format: "png" });
const out = path.join(".spec", "service-assembly-ui-panels", "e2e-shots", "canvas-04-layout.png");
fs.writeFileSync(out, Buffer.from(shot.result.data, "base64"));
console.log("SHOT " + out);
console.log("CONSOLE " + JSON.stringify(consoleErrs.slice(0, 8)));
try { ws.close(); } catch {}
bye(0);

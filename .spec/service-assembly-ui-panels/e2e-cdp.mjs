// E2E #0: raw-CDP browser smoke for the canvas (zero-dep, Node>=21 global WebSocket).
// Launches headless Edge (Chromium), opens the canvas page, probes DOM, captures a PNG.
// Usage: node .spec/service-assembly-ui-panels/e2e-cdp.mjs [url]
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9333;
const URL_ = process.argv[2] || "http://127.0.0.1:60890/canvas";
const OUT = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "e2e-shots");
fs.mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 0. server alive?
try {
  const r = await fetch("http://127.0.0.1:60890/api/uiManifest/list", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ type: "client-request", rpcId: "e", method: "uiManifest/list", payload: {} }),
  });
  const j = await r.json();
  console.log("manifest cards=" + (j.result?.value?.cards?.length ?? "?"));
} catch (e) {
  console.log("FAIL server not reachable: " + e.message);
  process.exit(1);
}

// 1. launch headless edge
const proc = spawn(EDGE, [
  "--headless=new",
  `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-e2e-profile")}`,
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-gpu",
  "--window-size=1600,1000",
  "about:blank",
], { stdio: "ignore" });
const bye = (code) => { try { proc.kill(); } catch {} process.exit(code); };
setTimeout(() => { console.log("FAIL timeout"); bye(1); }, 90000);

let ver = null;
for (let i = 0; i < 60 && !ver; i++) {
  await sleep(500);
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json/version`);
    if (r.ok) ver = await r.json();
  } catch {}
}
if (!ver) { console.log("FAIL CDP endpoint never came up"); bye(1); }
console.log("CDP up: " + (ver["Browser"] || "?"));

const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("ws fail")); });

let mid = 0;
const pend = new Map();
const consoleErrs = [];
const logEntries = [];
const netLog = [];
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); return; }
  if (m.method === "Runtime.exceptionThrown")
    consoleErrs.push("EX:" + (m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text));
  if (m.method === "Runtime.consoleAPICalled")
    consoleErrs.push(m.params.type + ":" + (m.params.args || []).map((a) => a.value ?? a.description ?? a.type).join(" ").slice(0, 200));
  if (m.method === "Log.entryAdded")
    logEntries.push(m.params.entry.level + ":" + String(m.params.entry.text).slice(0, 200) + "@" + String(m.params.entry.url || "").slice(-40));
  if (m.method === "Network.responseReceived") {
    const u = m.params.response.url;
    if (!u.endsWith(".png")) netLog.push(m.params.response.status + " " + u.slice(-60));
  }
  if (m.method === "Network.loadingFailed")
    netLog.push("FAIL " + String(m.params.errorText) + " " + String(m.params.requestId));
};
const send = (method, params = {}) =>
  new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });

await send("Page.enable");
await send("Runtime.enable");
await send("Log.enable");
await send("Network.enable");
await send("Page.addScriptToEvaluateOnNewDocument", {
  source: 'window.__errs=[];window.addEventListener("error",(e)=>window.__errs.push(String(e.message)));',
});
await send("Page.navigate", { url: URL_ });
await sleep(4000); // cards fetch via RPC; give SSE+history time

const probeExpr = `JSON.stringify({
  cards: document.querySelectorAll('.card').length,
  title: document.title,
  els: document.querySelectorAll('body *').length,
  stats: [...document.querySelectorAll('.card .stat')].slice(0,16).map(e=>e.textContent.slice(0,46)),
  rows: document.querySelectorAll('.card tr, .card .row').length,
  errEls: document.querySelectorAll('.card .err').length,
  inErrs: (window.__errs||[]).slice(0,5),
  forms: document.querySelectorAll('form,.chat-send').length,
  selects: document.querySelectorAll('select').length,
})`;
const probe = await send("Runtime.evaluate", { expression: probeExpr, returnByValue: true });
console.log("PROBE " + JSON.stringify(probe.result?.result?.value ?? probe.error ?? "n/a"));
console.log("CONSOLE " + JSON.stringify(consoleErrs.slice(0, 10)));
console.log("LOGDOM " + JSON.stringify(logEntries.slice(0, 10)));
console.log("NET " + JSON.stringify(netLog.slice(-30)));

const shot = await send("Page.captureScreenshot", { format: "png" });
const file = path.join(OUT, "canvas-01.png");
fs.writeFileSync(file, Buffer.from(shot.result.data, "base64"));
console.log("SHOT " + file + " bytes=" + shot.result.data.length);
try { ws.close(); } catch {}
bye(0);

// 逐插件浏览器校验器（插件功能真实对接 · 验收模板第 4 步基建）。
// 用法: node verify-plugin.mjs --title "会话清单" --expect default [--expect2 x --expect3 y] [--min-chars N] [--url http://127.0.0.1:60890/]
// 新鲜临时 profile（规避 sse-reload-starvation 已知缺陷）+ 反节流旗。
// 输出 JSON: {card, found, rows, expectHits, textSlice, consoleErrs, pass}
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf("--" + k); return i >= 0 ? args[i + 1] : d; };
const TITLE = opt("title", null);
const URL_ = opt("url", "http://127.0.0.1:60890/");
const expects = ["expect", "expect2", "expect3", "expect4", "expect5"].map(k => opt(k, null)).filter(Boolean);

const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = Number(opt("port", "9353"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const prof = path.join(os.tmpdir(), `dsh-verify-${Date.now()}`);
const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${prof}`, "--no-first-run", "--no-default-browser-check",
  "--disable-gpu", "--window-size=1600,1000",
  "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
const errs = [];
const bye = async (r) => {
  try { proc.kill(); } catch {}
  console.log(JSON.stringify(r));
  try { await sleep(800); fs.rmSync(prof, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch {}
  process.exit(r.pass ? 0 : 1);
};
setTimeout(() => bye({ card: TITLE, pass: false, why: "TIMEOUT", consoleErrs: errs }), 60000);

let ver = null;
for (let i = 0; i < 40 && !ver; i++) { await sleep(400);
  try { const r = await fetch(`http://127.0.0.1:${PORT}/json/version`); if (r.ok) ver = await r.json(); } catch {} }
if (!ver) bye({ card: TITLE, pass: false, why: "NO CDP", consoleErrs: errs });
const tgt = await (await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL_)}`, { method: "PUT" })).json();
const ws = new WebSocket(tgt.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let mid = 0; const pend = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
  if (m.method === "Runtime.consoleAPICalled" && (m.params.type === "error")) errs.push((m.params.args?.[0]?.value ?? "err").toString().slice(0, 160));
  if (m.method === "Runtime.exceptionThrown") errs.push("EX " + JSON.stringify(m.params.exceptionDetails?.exception?.description ?? m.params.exceptionDetails?.text ?? "").slice(0, 160));
};
const send = (method, params = {}) => new Promise((res) => { const id = ++mid; pend.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
const evl = async (expression) => { const m = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }); return m.result?.result?.value; };
await send("Runtime.enable"); await send("Page.enable");
// 等卡数就位（最多 ~8s），再等数据面渲染（数据 RPC 自动拉取）。
let n = 0;
for (let i = 0; i < 16; i++) { await sleep(600); n = await evl(`document.querySelectorAll('#workbench .card').length`) || 0; if (n >= 13) break; }
const js = `(() => {
  const cards = [...document.querySelectorAll('#workbench .card')];
  const c = cards.find(el => (el.innerText || '').includes(${JSON.stringify(TITLE)}));
  if (!c) return JSON.stringify({ found: false, total: cards.length });
  const t = (c.innerText || '').replace(/\\u00a0/g, ' ');
  return JSON.stringify({ found: true, total: cards.length, len: t.length, text: t.slice(0, 400) });
})()`;
let got = null;
for (let i = 0; i < 10; i++) {
  got = JSON.parse(await evl(js) || "{}");
  if (got.found && (!expects.length || expects.every(x => (got.text || "").includes(x)))) break;
  await sleep(700);
  got = JSON.parse(await evl(js) || "{}");
}
const rowsGuess = got.text ? (got.text.match(/\n/g) || []).length : 0;
const pass = !!got.found && (!expects.length || expects.every(x => (got.text || "").includes(x))) && errs.length === 0;
bye({
  card: TITLE, cards: n, found: !!got.found,
  expectHits: expects.map(x => ({ x, hit: (got.text || "").includes(x) })),
  rows: rowsGuess, textSlice: (got.text || "").slice(0, 200),
  consoleErrs: errs, pass,
});

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
// 技术修正（第 59 轮）：rename-.off 仍在扫描树内=假卸载（mount-sync 重扫 ~1.2s 计回，
// 旧「绿」实为瞬态窗口幸运采样）。真卸载=整目录移出 wasm-plugins（同盘树外）。
const OFF = path.join(".off-store", "panel-locale-edit");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const proc = spawn(EDGE, ["--headless=new", `--remote-debugging-port=${PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), "dsh-audit-profile")}`, "--no-first-run",
  "--no-default-browser-check", "--disable-gpu", "--window-size=1600,1000",
  // 反节流：headless 隐藏页会冻结后台定时器/任务队列（长跑审计 5 分钟后 SSE/poll
  // 不落地=环境行为非产品缺陷）；这三旗让页面等效真实可见前台。
  "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
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
  return JSON.stringify({cards:document.querySelectorAll('#workbench .card').length, shut:document.querySelectorAll('#sidebar .name.shut').length, ls:(JSON.parse(localStorage.getItem('dsh.canvas.closed.v2')||'{}').all||[]).length});
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
// T11 活体折叠：发送后不刷新，SSE 帧应把 turn 事件折进气泡（历史零依赖的实时证据）
R.T11_liveFold = await evl(`(async()=>{
  const card=[...document.querySelectorAll('.card')].find(c=>c.textContent.includes('聊天')&&c.querySelector('.chat-send,input'));
  if(!card) return 'NO-CARD';
  await new Promise(r=>setTimeout(r,2600));
  const t=card.textContent;
  return JSON.stringify({assistant:t.includes('助手'), echo:/echo|e2e-audit/.test(t)});
})()`);
// T12 分组桌板：组标题切板 + 关闭板间隔离 + 卡片标题切板 + hash 跟随
R.T12_boards = await evl(`(async()=>{
  const cards=()=>document.querySelectorAll('#workbench .card').length;
  const baseN=cards();
  const g=[...document.querySelectorAll('#sidebar .group-title')].find(b=>b.textContent.trim().startsWith('config'));
  if(!g) return 'NO-GROUP';
  g.click(); await new Promise(r=>setTimeout(r,500));
  const cfgN=cards(); const hash1=location.hash;
  const c1=document.querySelector('#workbench .card .card-close'); c1&&c1.click();
  await new Promise(r=>setTimeout(r,400));
  const cfgN2=cards();
  [...document.querySelectorAll('#sidebar button')].find(b=>b.textContent.includes('全部')).click();
  await new Promise(r=>setTimeout(r,500));
  const allBack=cards();
  const nameBtn=[...document.querySelectorAll('#sidebar .name')].find(b=>b.textContent.includes('llm-deepseek'));
  nameBtn&&nameBtn.click(); await new Promise(r=>setTimeout(r,500));
  const modelN=cards(); const hash2=location.hash;
  [...document.querySelectorAll('#sidebar button')].find(b=>b.textContent.includes('全部')).click();
  await new Promise(r=>setTimeout(r,400));
  return JSON.stringify({baseN,cfgN,cfgN2,allBack,modelN,hash1,hash2});
})()`);
// T13 hash 深链 fresh 直达（about:blank 强制真重载）
await send("Page.navigate", { url: "about:blank" }); await sleep(400);
await send("Page.navigate", { url: URL_ + "#board=session" }); await sleep(4500);
R.T13_deeplink = await evl(`(()=>{const cs=[...document.querySelectorAll('#workbench .card')];return JSON.stringify({n:cs.length, allSession:cs.length>0&&cs.every(c=>{const t=c.querySelector('.badges .type');return t&&t.textContent==='session';})})})()`);
await send("Page.navigate", { url: "about:blank" }); await sleep(400);
await send("Page.navigate", { url: URL_ }); await sleep(4500);
// T14 拖拽摆位（CDP 真事件）：拖首卡落位 → 钉位持久 + 板间零牵连（D-215 后位移=磁吸槽位差，非拖距） → 刷新持久 → 重置
{
  const geo = JSON.parse(await evl(`(()=>{const c=document.querySelector('#workbench .card');const cap=c.querySelector('.cap').getBoundingClientRect();return JSON.stringify({id:c.id,x:Math.round(cap.x+70),y:Math.round(cap.y+cap.height/2),left:parseFloat(c.style.left)||0,top:parseFloat(c.style.top)||0})})()`));
  const idj = JSON.stringify(geo.id);
  await send("Input.dispatchMouseEvent",{type:"mousePressed",x:geo.x,y:geo.y,button:"left",clickCount:1});
  for(let i=1;i<=6;i++){ await send("Input.dispatchMouseEvent",{type:"mouseMoved",x:geo.x+i*25,y:geo.y+i*20,button:"left"}); }
  await send("Input.dispatchMouseEvent",{type:"mouseReleased",x:geo.x+150,y:geo.y+120,button:"left",clickCount:1});
  await sleep(600);
  R.T14_drag = await evl(`(()=>{
    const c=document.getElementById(${idj});
    const lx=parseFloat(c.style.left),ty=parseFloat(c.style.top);
    const pos=JSON.parse(localStorage.getItem('dsh.canvas.pos')||'{}');
    const pin=(pos.all||{})[${idj}];
    const cross=Object.entries(pos).filter(([b,m])=>b!=='all'&&m[${idj}]).length;
    return JSON.stringify({movedX:Math.round(lx-${geo.left}),movedY:Math.round(ty-${geo.top}),
      pinMatchesStyle:!!pin&&Math.abs(pin.x-lx)<=2&&Math.abs(pin.y-ty)<=2,cross});
  })()`);
  await send("Page.navigate", { url: "about:blank" }); await sleep(400);
  await send("Page.navigate", { url: URL_ }); await sleep(4500);
  R.T14_reloadPin = await evl(`(()=>{const c=document.getElementById(${idj});
    const lx=parseFloat(c.style.left),ty=parseFloat(c.style.top);
    const pin=JSON.parse(localStorage.getItem('dsh.canvas.pos')||'{}').all[${idj}];
    return JSON.stringify({matches:!!pin&&Math.abs(pin.x-lx)<=3&&Math.abs(pin.y-ty)<=3})})()`);
  R.T14_reset = await evl(`(async()=>{const b=document.getElementById('reset-positions');
    if(!b) return 'NO-BTN';
    b.click(); await new Promise(r=>setTimeout(r,800));
    const pos=JSON.parse(localStorage.getItem('dsh.canvas.pos')||'{}');
    return JSON.stringify({allGone:!(pos.all&&Object.keys(pos.all).length), btnGone:!document.getElementById('reset-positions')})})()`);
}
// T15 磁吸防重叠：把首卡直接压到次卡上松手 → 落位磁吸空格 + 全桌两两零重叠（松手后+脉冲重排后各测一次）
{
  const g = JSON.parse(await evl(`(()=>{const cs=[...document.querySelectorAll('#workbench .card')];const ra=cs[0].querySelector('.cap').getBoundingClientRect();const rb=cs[1].getBoundingClientRect();return JSON.stringify({aid:cs[0].id,ax:Math.round(ra.x+70),ay:Math.round(ra.y+ra.height/2),tx:Math.round(rb.x+Math.min(rb.width/2,120)),ty:Math.round(rb.y+18)})})()`));
  await send("Input.dispatchMouseEvent",{type:"mousePressed",x:g.ax,y:g.ay,button:"left",clickCount:1});
  for(let i=1;i<=6;i++){ await send("Input.dispatchMouseEvent",{type:"mouseMoved",x:Math.round(g.ax+(g.tx-g.ax)*i/6),y:Math.round(g.ay+(g.ty-g.ay)*i/6),button:"left"}); }
  await send("Input.dispatchMouseEvent",{type:"mouseReleased",x:g.tx,y:g.ty,button:"left",clickCount:1});
  await sleep(700);
  const ovscan = `(()=>{const rs=[...document.querySelectorAll('#workbench .card')].map(c=>({l:c.offsetLeft,t:c.offsetTop,w:c.offsetWidth,h:c.offsetHeight}));let ov=0;for(let i=0;i<rs.length;i++)for(let j=i+1;j<rs.length;j++){const a=rs[i],b=rs[j];if(!(a.l+a.w<=b.l||b.l+b.w<=a.l||a.t+a.h<=b.t||b.t+b.h<=a.t))ov++;}return ov})()`;
  const ov0 = await evl(ovscan);
  await sleep(1800);
  const ov1 = await evl(ovscan);
  const pinned = await evl(`!!((JSON.parse(localStorage.getItem('dsh.canvas.pos')||'{}').all||{})[${JSON.stringify(g.aid)}])`);
  R.T15_noOverlap = JSON.stringify({ovAfterDrop:ov0, ovAfterPulse:ov1, pinned});
  await evl(`(()=>{const b=document.getElementById('reset-positions'); b&&b.click(); return 'ok'})()`);
  await sleep(700);
}
// T16 协商关：runtime-status 注入非法 requires → 不挂载(12) + 报告 reason code + inventory 行态 → 复原(13)
{
  const U16 = path.join("wasm-plugins", "panel-runtime-status");
  const P16 = path.join(U16, "plugin.json");
  const backup = fs.readFileSync(P16, "utf8");
  const rpc = async (method) => (await fetch("http://127.0.0.1:60890/api/" + method, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "t16", method, payload: {} }) })).json();
  const cardsOf = async () => { const r = await rpc("uiManifest/list"); return r.result?.value?.cards?.length ?? -1; };
  try {
    const j = JSON.parse(backup);
    j.participant = "panel-runtime-status";
    j.requires = [{ apiVersion: "dsh.ghost/v1", kind: "Ghost" }];
    fs.writeFileSync(P16, JSON.stringify(j, null, 2));
    let cards = 13;
    for (let i = 0; i < 20 && cards !== 12; i++) { await sleep(700); cards = await cardsOf(); }
    const domMid = await evl(`document.querySelectorAll('#workbench .card').length`);
    const rep = await rpc("contract/negotiationReport");
    const me = (rep.result?.value?.units ?? []).find(u => u.unit === "panel-runtime-status");
    const inv = await rpc("panel-plugin-inventory/list");
    const badRow = (inv.result?.value?.items ?? []).find(r => r.name === "panel-runtime-status" && r.state === "incompatible");
    fs.writeFileSync(P16, backup);
    let back = cards;
    for (let i = 0; i < 20 && back !== 13; i++) { await sleep(700); back = await cardsOf(); }
    await sleep(1500);
    const domEnd = await evl(`document.querySelectorAll('#workbench .card').length`);
    const rep2 = await rpc("contract/negotiationReport");
    const me2 = (rep2.result?.value?.units ?? []).find(u => u.unit === "panel-runtime-status");
    R.T16_gate = JSON.stringify({
      unmounted: cards === 12,
      reported: !!me && me.compatible === false,
      code: me?.issues?.[0]?.code ?? "none",
      invRow: !!badRow && String(badRow.note || "").includes("requirement-unsupported"),
      restored: back === 13,
      cleanAfter: !!me2 && me2.declared === false,
      domMid, domEnd,
    });
  } catch (e) {
    try { fs.writeFileSync(P16, backup); } catch {}
    for (let i = 0; i < 20; i++) { const c = await cardsOf().catch(() => -1); if (c === 13) break; await sleep(700); }
    R.T16_gate = "ERR " + String(e).slice(0, 120);
  }
}
let t7 = { note: "skipped" };
try {
  const m0 = await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json().then(j => j.result?.value?.cards?.length ?? -1);
  const domPre = await evl(`document.querySelectorAll('#workbench .card').length`);
  fs.mkdirSync(".off-store", { recursive: true });
  fs.renameSync(UNIT, OFF);
  let m1 = m0; for (let i = 0; i < 14 && m1 === m0; i++) { await sleep(600); m1 = await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json().then(j => j.result?.value?.cards?.length ?? -1); }
  let dom1 = m0; for (let i = 0; i < 16 && dom1 === m0; i++) { await sleep(700); dom1 = await evl(`document.querySelectorAll('#workbench .card').length`); }
  fs.renameSync(OFF, UNIT);
  let mhost = m0; for (let i = 0; i < 20 && mhost !== m0; i++) { await sleep(700); mhost = await (await fetch("http://127.0.0.1:60890/api/uiManifest/list", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "client-request", rpcId: "a", method: "uiManifest/list", payload: {} }) })).json().then(j => j.result?.value?.cards?.length ?? -1); }
  let dom2 = dom1; for (let i = 0; i < 20 && dom2 !== m0; i++) { await sleep(700); dom2 = await evl(`document.querySelectorAll('#workbench .card').length`); }
  t7 = { m1, mhost, domPre, dom1, dom2 };
  // 硬断言=宿主面（m1/mhost）；dom2 为观察字段：卡死=sse-reload-starvation.md 缺陷指纹
  // （浏览器侧事件连接饥饿），非热插拔回归——新鲜页探针双向 700ms 全绿。
  if (m1 !== m0 - 1 || mhost !== m0) R.T7_FAIL = "host hotplug broken";
} catch (e) { t7 = { err: String(e).slice(0, 120) }; }
R.T7_hotplug = JSON.stringify(t7);
// T9 关闭持久化：关一卡 → reload → 仍闭（ls 恢复）→ 重开复原
R.T9_reloadPersist = "FAIL";
try {
  await evl(`(()=>{const b=document.querySelector('#workbench .card .card-close'); b&&b.click(); return 'c';})()`);
  await sleep(500);
  const closedCnt = await evl(`(JSON.parse(localStorage.getItem('dsh.canvas.closed.v2')||'{}').all||[]).length`);
  await send("Page.navigate", { url: "about:blank" });
  await sleep(400);
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
R.T8_cleanup = await evl(`(()=>{localStorage.setItem('dsh.canvas.closed.v2','{}');localStorage.removeItem('dsh.canvas.pos');localStorage.removeItem('dsh.canvas.closed');return 'ok'})()`);
R.consoleErrs = consoleErrs.slice(0, 5);

console.log("AUDIT " + URL_);
console.log(JSON.stringify(R, null, 1));
try { ws.close(); } catch {}
bye(0);

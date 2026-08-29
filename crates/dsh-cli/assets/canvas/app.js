// 桌布壳粘合层（C3）。刻意保持薄：一切可证逻辑在 core.js（node --test 钉死）；
// 这里只做 DOM 渲染 / fetch / 定时器。诚实规则贯穿：绝不伪造成功、绝不白屏、
// 坏的一侧显式可见（§7 fail-loud 表）。
import {
  buildModel,
  layoutGrid,
  columnsForWidth,
  validateDeclaration,
  collectValues,
  rpcEnvelope,
  pollDecision,
  focusKey,
  listRows,
  statusItems,
  GRID,
} from "./core.js";

const POLL_MS = 10000; // rev 轮询兜底（D-186 起 SSE 为主通道；unchanged 协商让兜底几乎免费）
const state = { model: null, selectedType: null, rid: 0, polling: false };

const $ = (id) => document.getElementById(id);

function status(text, cls) {
  const s = $("status");
  s.textContent = text;
  s.className = cls || "";
}

function rpc(method, args) {
  state.rid += 1;
  return fetch("/api/" + method, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(rpcEnvelope(method, args, "canvas-" + state.rid)),
  }).then((r) => r.json());
}

// ---------- 清单：首次加载 + rev 轮询 ----------

async function loadManifest() {
  const args = state.model ? { rev: state.model.rev } : {};
  let value;
  try {
    value = await rpc("uiManifest/list", args);
  } catch (e) {
    status("✗ 清单拉取失败：" + e.message + "（保留现状）", "err");
    return;
  }
  if (value && value.ok === false) {
    status("✗ 清单错误：" + ((value.error && value.error.message) || "?"), "err");
    return;
  }
  const decision = pollDecision(state.model, value);
  if (decision.action === "keep") return;
  render({ rev: decision.rev, cards: decision.cards });
}

// ---------- 渲染 ----------

function render(manifest) {
  state.model = buildModel(manifest);
  // 分类被清空 → 诚实回「全部」（不空转选中态）
  if (state.selectedType && !state.model.groups.some((g) => g.type === state.selectedType)) {
    state.selectedType = null;
  }
  $("rev").textContent = "rev " + String(state.model.rev).slice(0, 12) + "…";
  status("✓ 清单 " + state.model.cards.length + " 卡", "ok");
  renderSidebar();
  renderWorkbench();
}

function renderSidebar() {
  const nav = $("sidebar");
  nav.innerHTML = "";
  const all = button("全部（" + state.model.cards.length + "）", "all", () => {
    state.selectedType = null;
    renderSidebar();
    renderWorkbench();
  });
  if (!state.selectedType) all.classList.add("active");
  nav.appendChild(all);
  for (const g of state.model.groups) {
    const t = button(g.type, "group-title", () => {
      state.selectedType = state.selectedType === g.type ? null : g.type;
      renderSidebar();
      renderWorkbench();
    });
    const count = document.createElement("span");
    count.className = "count";
    count.textContent = String(g.count);
    t.appendChild(count);
    if (state.selectedType === g.type) t.classList.add("active");
    nav.appendChild(t);
    for (const c of g.cards) {
      const n = button(c.bad ? "✗ " + c.pluginName : c.pluginName, "name", () => focusCard(c));
      nav.appendChild(n);
    }
  }
}

function button(text, cls, onClick) {
  const b = document.createElement("button");
  b.className = cls;
  b.textContent = text;
  b.addEventListener("click", onClick);
  return b;
}

function renderWorkbench() {
  const wb = $("workbench");
  wb.innerHTML = "";
  const cards = state.selectedType
    ? ((state.model.groups.find((g) => g.type === state.selectedType) || {}).cards || [])
    : state.model.cards;
  if (cards.length === 0) {
    const e = document.createElement("div");
    e.className = "empty";
    e.textContent = "还没有服务装配单元声明 UI——向 wasm-plugins/<name>/web/ui.json 提交 v2 卡片声明即自动出现。";
    wb.appendChild(e);
    return;
  }
  const columns = columnsForWidth(wb.clientWidth);
  const grid = layoutGrid(
    cards.map((c) => ({ key: focusKey(c), w: (c.size && c.size.w) || 2, h: (c.size && c.size.h) || 3 })),
    columns
  );
  wb.style.minHeight = grid.totalRows * (GRID.row + GRID.gap) + 28 + "px";
  const byKey = new Map(cards.map((c) => [focusKey(c), c]));
  for (const p of grid.positions) {
    const card = byKey.get(p.key);
    wb.appendChild(cardEl(card, p));
  }
}

/** 焦点：滚动 + 高亮。**不调用任何重排**——布局输入不变（S6）。 */
function focusCard(card) {
  const el = document.querySelector('[data-focus-key="' + CSS.escape(focusKey(card)) + '"]');
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: "center" });
  el.classList.add("focus-hl");
  setTimeout(() => el.classList.remove("focus-hl"), 1600);
}

function cardEl(card, pos) {
  const el = document.createElement("section");
  el.className = "card" + (card.bad ? " fail" : "");
  el.dataset.focusKey = focusKey(card);
  el.style.left = pos.col * (GRID.col + GRID.gap) + "px";
  el.style.top = pos.row * (GRID.row + GRID.gap) + "px";
  el.style.width = pos.w * GRID.col + (pos.w - 1) * GRID.gap + "px";
  el.style.minHeight = pos.h * GRID.row + (pos.h - 1) * GRID.gap + "px";

  const cap = document.createElement("div");
  cap.className = "cap";
  cap.textContent = card.title || card.pluginName;
  el.appendChild(cap);
  const badges = document.createElement("div");
  badges.className = "badges";
  badges.innerHTML =
    '<span class="type"></span><span class="plugin"></span><span class="size"></span>';
  badges.querySelector(".type").textContent = card.type;
  badges.querySelector(".plugin").textContent = card.pluginName;
  badges.querySelector(".size").textContent = "格 " + (card.size ? card.size.w + "×" + card.size.h : "?");
  el.appendChild(badges);
  if (card.declaredType || card.declaredSize) {
    const n = document.createElement("div");
    n.className = "note";
    n.textContent =
      "ℹ " +
      (card.declaredType ? "type \"" + card.declaredType + "\" 未识别，落 misc；" : "") +
      (card.declaredSize ? "size " + card.declaredSize.w + "×" + card.declaredSize.h + " 越上限已裁剪（size-clamped）" : "");
    el.appendChild(n);
  }

  if (card.bad) {
    // 清单已判死刑：不发 fetch，直接 fail-loud（装了但坏了必须可见）。
    const m = document.createElement("div");
    m.className = "fail-msg";
    m.textContent = "✗ " + card.error.message;
    el.appendChild(m);
    const c = document.createElement("div");
    c.className = "code";
    c.textContent = "code=" + card.error.code + " · " + card.declPath;
    el.appendChild(c);
    return el;
  }
  loadBody(el, card);
  return el;
}

async function loadBody(el, card) {
  let decl;
  try {
    const r = await fetch(card.declPath, { cache: "no-store" });
    if (!r.ok) throw { code: "declaration-unfetchable", message: "GET " + card.declPath + " HTTP " + r.status };
    decl = await r.json();
  } catch (e) {
    failLoud(el, null, (e && e.code) || "declaration-unparseable", (e && e.message) || String(e));
    return;
  }
  const bad = validateDeclaration(decl);
  if (bad) {
    failLoud(el, decl, bad.code, bad.message);
    return;
  }
  if (decl.view.kind === "form") {
    renderForm(el, decl);
  } else {
    renderDataBody(el, decl); // status / list（C4）
  }
}

/** fail-loud 元数据卡：契约缺陷显式呈现，但**卡级动作仍可用**（§4.2 明令）。 */
function failLoud(el, decl, code, message) {
  el.classList.add("fail");
  const m = document.createElement("div");
  m.className = "fail-msg";
  m.textContent = "✗ " + message;
  el.appendChild(m);
  const c = document.createElement("div");
  c.className = "code";
  c.textContent = "code=" + code;
  el.appendChild(c);
  const actions = decl && decl.view && Array.isArray(decl.view.actions) ? decl.view.actions : [];
  if (actions.length > 0) {
    const box = document.createElement("div");
    box.className = "actions";
    const stat = statLine(el);
    actions.forEach((a) => {
      if (!Array.isArray(a.rpc) || a.rpc.length !== 2) return;
      box.appendChild(button(a.label, a.primary ? "primary" : "", () => {
        stat("→ " + a.rpc.join("/") + " …", "");
        rpc(a.rpc.join("/"), {}).then((res) => report(stat, res));
      }));
    });
    el.appendChild(box);
  }
}

function statLine(el) {
  const s = document.createElement("div");
  s.className = "cstat";
  el.appendChild(s);
  return (text, cls) => {
    s.textContent = text;
    s.className = "cstat " + (cls || "");
  };
}

function report(stat, res) {
  if (res && res.ok !== false) {
    stat("✓ " + JSON.stringify(res.value || {}), "ok");
  } else {
    const err = (res && res.error) || {};
    stat("✗ " + (err.message || "操作失败") + "（code=" + (err.code || "?") + "）", "err");
  }
}

// ---------- form 渲染器（C3 实现档） ----------

function renderForm(el, decl) {
  const view = decl.view;
  const inputs = {};
  const stat = statLine(el);

  const finish = (prefill) => {
    view.fields.forEach((f) => {
      const label = document.createElement("label");
      const span = document.createElement("span");
      span.textContent = f.label + (f.required ? " *" : "");
      label.appendChild(span);
      const input = fieldInput(f, prefill[f.name] !== undefined ? prefill[f.name] : f.default);
      inputs[f.name] = input;
      label.appendChild(input);
      el.appendChild(label);
    });
    renderActions(el, decl, inputs, stat);
  };

  // dataRpc 预填；拉不到用声明默认值（诚实，不伪造）
  const rpc2 = view.dataRpc;
  if (Array.isArray(rpc2) && rpc2.length === 2) {
    stat("载入当前值…", "");
    rpc(rpc2.join("/"), {})
      .then((res) => finish((res && res.ok !== false && res.value && res.value.values) || {}))
      .catch(() => finish({}));
  } else {
    finish({});
  }
}

function fieldInput(f, value) {
  if (f.type === "select") {
    const sel = document.createElement("select");
    (f.options || []).forEach((opt) => {
      const o = document.createElement("option");
      o.value = String(opt);
      o.textContent = String(opt);
      if (String(opt) === String(value)) o.selected = true;
      sel.appendChild(o);
    });
    return sel;
  }
  const input = document.createElement(f.type === "list" ? "textarea" : "input");
  if (f.type === "number") {
    input.type = "number";
    if (f.min !== undefined) input.min = f.min;
  } else if (f.type !== "list") {
    input.type = "text";
    if (f.role === "credential-ref") input.placeholder = "环境变量名（凭证引用）";
  }
  if (f.type === "list") {
    input.value = JSON.stringify(Array.isArray(value) ? value : [], null, 1);
  } else if (value !== undefined && value !== null) {
    input.value = value;
  }
  return input;
}

function renderActions(el, decl, inputs, stat) {
  const box = document.createElement("div");
  box.className = "actions";
  (decl.view.actions || []).forEach((a) => {
    if (!Array.isArray(a.rpc) || a.rpc.length !== 2) return;
    box.appendChild(button(a.label, a.primary ? "primary" : "", () => {
      let values;
      try {
        values = collectValues(decl.view, (name) => inputs[name].value);
      } catch (e) {
        stat("✗ " + e.message, "err");
        return; // fail-loud：动作不发
      }
      stat("→ " + a.rpc.join("/") + " …", "");
      rpc(a.rpc.join("/"), { values: values }).then((res) => report(stat, res));
    }));
  });
  el.appendChild(box);
}

// ---------- status / list 渲染器（C4 实现档） ----------

/**
 * status/list 卡体：dataRpc 拉真实数据（拉失败 = 诚实错误行 + 静态兜底 view.items/rows）；
 * 有 dataRpc 的卡给「刷新」affordance（重放 dataRpc——渲染器便利，不是契约动作）。
 */
function renderDataBody(el, decl) {
  const view = decl.view;
  const body = document.createElement("div");
  el.appendChild(body);
  const stat = statLine(el);

  const paint = (dataValue) => {
    body.innerHTML = "";
    if (view.kind === "status") paintStatus(body, view, dataValue);
    else paintList(body, view, dataValue);
  };

  const load = () => {
    const rpc2 = view.dataRpc;
    if (!Array.isArray(rpc2) || rpc2.length !== 2) {
      paint(null); // 无数据面：静态兜底（契约 §4.1 允许）
      return;
    }
    stat("载入数据…", "");
    rpc(rpc2.join("/"), {})
      .then((res) => {
        if (res && res.ok === false) {
          const err = res.error || {};
          stat("✗ 数据面失败：" + (err.message || err.code || "?") + "（静态兜底）", "err");
          paint(null);
          return;
        }
        stat("✓ 数据已更新", "ok");
        paint((res && res.value) || null);
      })
      .catch((e) => {
        stat("✗ 数据面不可达：" + e.message + "（静态兜底）", "err");
        paint(null);
      });
  };

  if (Array.isArray(view.dataRpc) && view.dataRpc.length === 2) {
    const box = document.createElement("div");
    box.className = "actions";
    box.appendChild(button("刷新", "", load));
    el.appendChild(box);
  }
  load();
}

function paintStatus(body, view, dataValue) {
  const items = statusItems(view, dataValue);
  if (items.length === 0) {
    const e = document.createElement("div");
    e.className = "note";
    e.textContent = "暂无状态项";
    body.appendChild(e);
    return;
  }
  for (const it of items) {
    const row = document.createElement("div");
    row.className = "srow";
    const l = document.createElement("span");
    l.className = "slabel";
    l.textContent = String(it.label ?? "");
    const v = document.createElement("span");
    v.className = "svalue" + (it.tone ? " tone-" + it.tone : "");
    v.textContent = it.value === null || it.value === undefined
      ? "—"
      : typeof it.value === "object" ? JSON.stringify(it.value) : String(it.value);
    row.appendChild(l);
    row.appendChild(v);
    body.appendChild(row);
  }
}

function paintList(body, view, dataValue) {
  const { rows, columns, emptyText } = listRows(view, dataValue);
  if (rows.length === 0) {
    const e = document.createElement("div");
    e.className = "note";
    e.textContent = emptyText;
    body.appendChild(e);
    return;
  }
  const table = document.createElement("table");
  table.className = "ltable";
  const thead = document.createElement("tr");
  (columns.length > 0
    ? columns
    : Object.keys(rows[0]).map((k) => ({ key: k, label: k }))
  ).forEach((c) => {
    const th = document.createElement("th");
    th.textContent = String(c.label || c.key);
    thead.appendChild(th);
  });
  table.appendChild(thead);
  for (const r of rows) {
    const tr = document.createElement("tr");
    (columns.length > 0
      ? columns
      : Object.keys(r).map((k) => ({ key: k, label: k }))
    ).forEach((c) => {
      const td = document.createElement("td");
      const cell = r[c.key];
      td.textContent = cell === null || cell === undefined
        ? "—"
        : typeof cell === "object" ? JSON.stringify(cell) : String(cell);
      tr.appendChild(td);
    });
    table.appendChild(tr);
  }
  body.appendChild(table);
}

// ---------- 启动 ----------

loadManifest();
setInterval(() => {
  if (state.polling) return;
  state.polling = true;
  loadManifest().finally(() => { state.polling = false; });
}, POLL_MS);
window.addEventListener("resize", () => { if (state.model) renderWorkbench(); });

// D-186：热插拔主通道——`/plugins/events` SSE 收 ui-manifest-changed 即重取清单
// （pollDecision 的 keep/replace 语义对重复/乱序帧天然安全）。graph/rebuilt 帧属
// harness HMR 链路，本壳忽略。
try {
  const es = new EventSource("/plugins/events");
  es.onmessage = (ev) => {
    let frame;
    try { frame = JSON.parse(ev.data); } catch { return; }
    if (frame && frame.type === "ui-manifest-changed") loadManifest();
  };
  es.onerror = () => { /* 断线静默——轮询兜底在跑，浏览器会自动重连 */ };
} catch { /* EventSource 不可用环境：轮询兜底即主通道 */ }

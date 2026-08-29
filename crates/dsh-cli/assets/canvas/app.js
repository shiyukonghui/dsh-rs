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
  rowActionBody,
  needsConfirm,
  chatFoldFrame,
  chatOptions,
  schemaFields,
  nsSelectModel,
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
  } else if (decl.view.kind === "chat") {
    renderChat(el, decl); // C8-3（D-193）
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

  const finish = (prefill, meta, host) => {
    const boxHost = host || el;
    view.fields.forEach((f) => {
      const label = document.createElement("label");
      const span = document.createElement("span");
      span.textContent = f.label + (f.required ? " *" : "");
      label.appendChild(span);
      const input = fieldInput(f, prefill[f.name] !== undefined ? prefill[f.name] : f.default);
      inputs[f.name] = input;
      label.appendChild(input);
      boxHost.appendChild(label);
    });
    if (meta) {
      // S3（D-194）：不可编辑面如实列出（嵌套只读 / secrets 仅存在性——不伪造控件）。
      (meta.readonly || []).forEach((r) => {
        const d = document.createElement("div");
        d.className = "note";
        d.textContent = "· " + r.key + "：" + r.note;
        boxHost.appendChild(d);
      });
      (meta.secrets || []).forEach((s) => {
        const d = document.createElement("div");
        d.className = "note";
        d.textContent = "· " + s.path + "：" + (s.set ? "已设" : "未设") + "（secrets 不可在桌布编辑）";
        boxHost.appendChild(d);
      });
    }
    renderActions(boxHost, decl, inputs, stat, meta);
  };

  // fieldsFrom（D-194/S3）：fields 运行时从数据面投影（设置域 = 宿主既表面，S2 别名面）。
  // nsSelect（D-201）：带命名空间下拉——一卡通用编辑全部 ns（终结每 ns 一卡的机械复制）。
  const ff = view.fieldsFrom;
  if (ff && Array.isArray(ff.rpc) && ff.rpc.length === 2 && typeof ff.pick === "string") {
    const body = document.createElement("div");
    el.appendChild(body);
    let allValue = null;
    const paint = (nsName) => {
      Object.keys(inputs).forEach((k) => delete inputs[k]);
      body.innerHTML = "";
      const nss = (allValue && allValue.namespaces) || [];
      const nsView = nss.find((n) => n && n.ns === nsName);
      if (!nsView) {
        stat("✗ 命名空间不存在：" + nsName + "（不猜字段）", "err");
        return;
      }
      const proj = schemaFields(nsView);
      view.fields = proj.fields.map((f) => ({
        name: f.key, label: f.label, type: f.type, options: f.options, default: f.value,
      }));
      if (ff.nsSelect === true) {
        const m = nsSelectModel(allValue, nsName);
        const bar = document.createElement("div");
        bar.className = "note";
        const cap = document.createElement("span");
        cap.textContent = "命名空间 ";
        const picker = document.createElement("select");
        m.options.forEach((o) => {
          const op = document.createElement("option");
          op.value = o;
          op.textContent = o;
          op.selected = o === m.current;
          picker.appendChild(op);
        });
        picker.addEventListener("change", () => paint(picker.value));
        bar.appendChild(cap);
        bar.appendChild(picker);
        body.appendChild(bar);
      }
      finish({}, {
        ns: nsName, revision: proj.revision, applies: proj.applies,
        readonly: proj.readonly, secrets: proj.secrets,
      }, body);
    };
    stat("载入设置面…", "");
    rpc(ff.rpc.join("/"), {})
      .then((res) => {
        if (!res || res.ok === false) {
          stat("✗ 设置面：" + ((res && res.error && res.error.message) || "?"), "err");
          return;
        }
        allValue = res.value || {};
        paint(nsSelectModel(allValue, ff.pick).current);
      })
      .catch((e) => stat("✗ 设置面载入失败：" + e.message, "err"));
    return;
  }

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
  if (f.type === "checkbox") {
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = value === true;
    return cb;
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

function renderActions(el, decl, inputs, stat, meta) {
  const box = document.createElement("div");
  box.className = "actions";
  (decl.view.actions || []).forEach((a) => {
    if (!Array.isArray(a.rpc) || a.rpc.length !== 2) return;
    box.appendChild(button(a.label, a.primary ? "primary" : "", () => {
      let values;
      try {
        values = collectValues(decl.view, (name) => {
          const i = inputs[name];
          return i && i.type === "checkbox" ? i.checked : i.value;
        });
      } catch (e) {
        stat("✗ " + e.message, "err");
        return; // fail-loud：动作不发
      }
      stat("→ " + a.rpc.join("/") + " …", "");
      // fieldsFrom（D-194）：保存体 = 乐观锁形 {ns, patch, expectedRevision}。
      const body = meta
        ? { ns: meta.ns, patch: values, expectedRevision: meta.revision }
        : { values: values };
      rpc(a.rpc.join("/"), body).then((res) => {
        report(stat, res);
        if (meta && res && res.ok !== false && meta.applies === "restart") {
          stat("✓ 已保存——需重启生效", "ok");
        }
      });
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
    else paintList(body, view, dataValue, act);
  };

  /** 行动作（C6/D-189）：confirm 未确认**不发 RPC**；成功刷新列表（行数据随动作变）。 */
  const act = (action, row) => {
    if (needsConfirm(action) &&
        !window.confirm("对「" + String(row.name ?? row.pluginId ?? row.label ?? "?") +
          "」执行 " + String(action.label ?? action.name) + "？")) {
      return;
    }
    stat("→ " + action.rpc.join("/") + " …", "");
    rpc(action.rpc.join("/"), rowActionBody(row, action)).then((res) => {
      report(stat, res);
      if (res && res.ok !== false) load();
    });
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

function paintList(body, view, dataValue, act) {
  const { rows, columns, emptyText } = listRows(view, dataValue);
  if (rows.length === 0) {
    const e = document.createElement("div");
    e.className = "note";
    e.textContent = emptyText;
    body.appendChild(e);
    return;
  }
  const rowActions = Array.isArray(view.rowActions) ? view.rowActions : [];
  const cols = columns.length > 0
    ? columns
    : Object.keys(rows[0]).map((k) => ({ key: k, label: k }));
  const table = document.createElement("table");
  table.className = "ltable";
  const thead = document.createElement("tr");
  cols.forEach((c) => {
    const th = document.createElement("th");
    th.textContent = String(c.label || c.key);
    thead.appendChild(th);
  });
  if (rowActions.length > 0) {
    const th = document.createElement("th");
    th.textContent = "操作";
    thead.appendChild(th);
  }
  table.appendChild(thead);
  for (const r of rows) {
    const tr = document.createElement("tr");
    cols.forEach((c) => {
      const td = document.createElement("td");
      const cell = r[c.key];
      td.textContent = cell === null || cell === undefined
        ? "—"
        : typeof cell === "object" ? JSON.stringify(cell) : String(cell);
      tr.appendChild(td);
    });
    if (rowActions.length > 0) {
      const td = document.createElement("td");
      rowActions.forEach((a) => {
        const btn = button(a.label ?? a.name, "", () => act(a, r));
        btn.className = "row-action";
        td.appendChild(btn);
      });
      tr.appendChild(td);
    }
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

// C8-3（D-193）：chat 渲染器——会话选择器 + 历史折叠（复用 session.history 事件面，
// 喂 core chatFoldFrame）+ 发送（乐观气泡，失败标注）+ 轮询刷新（stream SSE 直订
// 待宿主帧形状取证，轮询与 SSE 同一折叠事实源，无第二权威）。
function renderChat(el, decl) {
  const view = decl.view;
  const stat = statLine(el);
  const bar = document.createElement("div");
  bar.className = "chat-bar";
  const sel = document.createElement("select");
  bar.appendChild(sel);
  bar.appendChild(button("↻", "", () => loadHistory()));
  el.appendChild(bar);
  const msgs = document.createElement("div");
  msgs.className = "chat-msgs";
  el.appendChild(msgs);
  const form = document.createElement("form");
  form.className = "chat-send";
  const input = document.createElement("input");
  input.placeholder = "发消息…";
  input.autocomplete = "off";
  const go = button("发送", "primary", null);
  go.type = "submit";
  form.appendChild(input);
  form.appendChild(go);
  // 停止按钮（D-203）：仅当声明 cancelRpc 时绘制；语义 = 取消当前会话驱动
  // （宿主 session.cancel 幂等臂，turn 中可并发送达），不删历史。
  if (Array.isArray(view.cancelRpc) && view.cancelRpc.length === 2) {
    form.appendChild(button("停止", "", () => {
      if (!sid) { stat("✗ 当前无会话", "err"); return; }
      stat("→ 取消 " + sid + " …", "");
      rpc(view.cancelRpc.join("/"), { sessionId: sid })
        .then((res) => report(stat, res))
        .catch((e) => stat("✗ 取消：" + e.message, "err"));
    }));
  }
  el.appendChild(form);

  let sid = null;
  let chat = { sessionId: null, busy: false, messages: [] };

  // 事件 data 形状（user `{content}`、assistant 或嵌 `message.content`，content 为
  // 串或 text 块数组）→ 归一成 core 折叠契约的 `{text}`（传输适配在 DOM 层；
  // 历史与 SSE 帧共用同一归一，无第二权威）。
  const frameText = (d) => {
    if (!d) return "";
    const c = d.content !== undefined
      ? d.content
      : d.message && d.message.content !== undefined ? d.message.content : d.text;
    if (typeof c === "string") return c;
    if (Array.isArray(c)) {
      return c.filter((b) => b && b.type === "text").map((b) => b.text || "").join("");
    }
    return "";
  };

  const paint = () => {
    msgs.innerHTML = "";
    for (const m of chat.messages) {
      const d = document.createElement("div");
      d.className = "chat-bubble " + m.role;
      const who = m.role === "user" ? "我: " : m.role === "assistant" ? "助手: " : "· ";
      d.textContent = who + m.text + (m.pending ? " …" : "");
      msgs.appendChild(d);
    }
    msgs.scrollTop = msgs.scrollHeight;
  };

  const loadHistory = () => {
    if (!sid) return;
    rpc(view.historyRpc.join("/"), { sessionId: sid })
      .then((res) => {
        if (!res || res.ok === false) {
          report(stat, res);
          return;
        }
        let s = { sessionId: sid, busy: false, messages: [] };
        for (const wrap of (res.value && res.value.events) || []) {
          const ev = wrap && wrap.event;
          if (!ev) continue;
          s = chatFoldFrame(s, {
            sessionId: sid,
            kind: ev.type,
            data: { text: frameText(ev.data) },
            time: ev.time,
          });
        }
        chat = s;
        paint();
      })
      .catch((e) => stat("✗ 历史拉取：" + e.message, "err"));
  };

  sel.onchange = () => {
    sid = sel.value;
    loadHistory();
  };

  form.onsubmit = (e) => {
    e.preventDefault();
    const text = input.value.trim();
    if (!sid || !text) return;
    chat = {
      sessionId: sid,
      busy: chat.busy,
      messages: chat.messages.concat([{ role: "user", text, pending: true, ts: Date.now() }]),
    };
    paint();
    input.value = "";
    rpc(view.sendRpc.join("/"), { sessionId: sid, text })
      .then((res) => {
        report(stat, res);
        if (res && res.ok === false) {
          const copy = chat.messages.slice();
          const last = copy.length - 1;
          if (copy[last] && copy[last].pending) {
            copy[last] = { ...copy[last], text: copy[last].text + "（发送失败）" };
          }
          chat = { ...chat, messages: copy };
          paint();
        }
      })
      .catch((e) => stat("✗ 发送：" + e.message, "err"));
  };

  rpc(view.sessionSource.join("/"), {})
    .then((res) => {
      if (!res || res.ok === false) {
        stat("✗ 会话列表：" + ((res && res.error && res.error.message) || "?"), "err");
        return;
      }
      const opts = chatOptions((res.value && res.value.items) || []);
      if (opts.length === 0) {
        stat("没有可选会话", "warn");
        return;
      }
      for (const o of opts) {
        const op = document.createElement("option");
        op.value = o.value;
        op.textContent = o.label;
        sel.appendChild(op);
      }
      sid = opts.some((o) => o.value === "default") ? "default" : opts[0].value;
      sel.value = sid;
      loadHistory();
    })
    .catch((e) => stat("✗ 会话列表拉取：" + e.message, "err"));

  // C8-3 契约字段 `stream:"session-events"` 的直订接入（C8-3b）：
  // **只订 events.mux**——`session/event` 帧仅 mux 通道携带（D-113 实证：host 通道
  // 下推即被前端 zod 判 malformed 丢弃）。折叠与轮询**同一事实源**（frameText 归一 +
  // chatFoldFrame 引用差重绘）；轮询保留为断线兜底。
  if (view.stream === "session-events") {
    try {
      const es = new EventSource("/api/events.mux");
      es.onmessage = (msg) => {
        let frame;
        try { frame = JSON.parse(msg.data); } catch { return; }
        const p = frame && frame.payload;
        if (!frame || frame.method !== "session/event" || !p || p.sessionId !== sid || !p.event) return;
        const next = chatFoldFrame(chat, {
          sessionId: sid,
          kind: p.event.type,
          data: { text: frameText(p.event.data) },
          time: p.event.time,
        });
        if (next !== chat) {
          chat = next;
          paint();
        }
      };
      es.onerror = () => { /* 断线静默：EventSource 自动重连，轮询兜底在跑 */ };
    } catch { /* 无 EventSource 环境：轮询仍是完整兜底 */ }
  }

  setInterval(() => loadHistory(), 5000);
}

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

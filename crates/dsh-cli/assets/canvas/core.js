// 桌布壳核心纯函数（C3，D-184）。零 DOM / 零 fetch / 零 eval——一切可证的都在这一层证
// （测试：tests/core.test.mjs；契约：.spec/service-assembly-ui-c3/design.md §2）。
// 不变式（D-179/D-181）：声明是数据不是代码——这里只消费数据，永不执行插件产物。

"use strict";

/** 分类轴闭集序（D-181 §3；misc 恒末）。加值走 DECISIONS，不需渲染器。 */
export const TYPE_ORDER = ["model", "config", "capability", "runtime", "resource", "session", "misc"];

const SCHEMA = "dsh/plugin-ui/v2";
const REJECTED = ["board"]; // 画布本身；卡内嵌画布 = 递归陷阱
const RESERVED = ["chat", "chart", "table"]; // 契约预留：签名已定，渲染器未建
// C4 点亮 form/status/list（§4.1 三档「实现档」齐）；预留档落 renderer-unimplemented。
const IMPLEMENTED = ["form", "status", "list"];
const UNIMPLEMENTED = RESERVED;

/** 默认网格几何（CSS 变量可覆写；格距 10px 是契约值，非打印 pt）。 */
export const GRID = { col: 260, row: 100, gap: 10 };

/**
 * 清单值 → 展示模型：按 type 分组（TYPE_ORDER 序，只含有卡的组，组内保声明序）。
 * error 条目 = 「装了但坏了」→ 归 misc 组坏卡（bad:true，app 层不发 fetch 直接 fail-loud）。
 * 归一（type/size）只信清单——壳不重做（双权威禁令）。
 */
export function buildModel(manifest) {
  const cards = (manifest && Array.isArray(manifest.cards) ? manifest.cards : []).map((c) =>
    c && c.error
      ? Object.assign({ bad: true, type: "misc", size: { w: 2, h: 3 }, title: c.pluginName + "（声明损坏）" }, c)
      : Object.assign({ bad: false }, c)
  );
  const groups = [];
  for (const type of TYPE_ORDER) {
    const inGroup = cards.filter((c) => (c.type || "misc") === type);
    if (inGroup.length > 0) groups.push({ type, cards: inGroup, count: inGroup.length });
  }
  return { rev: (manifest && manifest.rev) || "", cards, groups };
}

/** 列数 = floor((容器宽 + 格距) / (格宽 + 格距))，窄屏保底 1 列（canvas design §5.2）。 */
export function columnsForWidth(widthPx, geom) {
  const g = geom || GRID;
  return Math.max(1, Math.floor((widthPx + g.gap) / (g.col + g.gap)));
}

/**
 * 瀑布流 first-fit（D-184 契约）：w=min(w,C) 收窄；卡顶 = 跨列当前高的最大值；
 * 平手取最左（严格 < 扫描 = 声明序直觉）；heights[span] = top + h。
 * 性质（tests 钉死）：无重叠、不出界、totalRows 覆盖全部。坐标在此算完，CSS 只消费。
 */
export function layoutGrid(cards, columns) {
  const C = Math.max(1, columns | 0);
  const heights = new Array(C).fill(0);
  const positions = [];
  for (const c of cards) {
    const w = Math.max(1, Math.min(c.w, C));
    let bestCol = 0;
    let bestTop = Infinity;
    for (let s = 0; s + w <= C; s++) {
      let top = 0;
      for (let k = s; k < s + w; k++) top = Math.max(top, heights[k]);
      if (top < bestTop) {
        bestTop = top;
        bestCol = s;
      }
    }
    for (let k = bestCol; k < bestCol + w; k++) heights[k] = bestTop + c.h;
    positions.push({ key: c.key, col: bestCol, row: bestTop, w, h: c.h });
  }
  return { positions, totalRows: heights.reduce((a, b) => Math.max(a, b), 0) };
}

/**
 * 声明校验（§7 fail-loud 表，画不画得出一问）；返回 null = 可画。
 * 清单已判的死刑（error 条目）不会走到这里——那是 app 层的短路。
 */
export function validateDeclaration(d) {
  if (!d || typeof d !== "object" || Array.isArray(d)) {
    return { code: "declaration-unparseable", message: "声明不是 JSON 对象" };
  }
  if (d.$schema !== SCHEMA) {
    return {
      code: "schema-version-unsupported",
      message: "仅支持 " + SCHEMA + "，收到 " + JSON.stringify(d.$schema) + "（不做静默兼容）",
    };
  }
  if (d.kind !== "card") {
    return { code: "card-kind-unknown", message: "顶层唯一容器是 kind:\"card\"，收到 " + JSON.stringify(d.kind) };
  }
  if (!d.view || typeof d.view !== "object" || !d.view.kind) {
    return { code: "view-malformed", message: "卡片缺 view 或 view.kind" };
  }
  const k = d.view.kind;
  if (REJECTED.indexOf(k) >= 0) {
    return { code: "view-kind-rejected", message: "view.kind=\"" + k + "\" 被契约否决（画布本身，卡内嵌画布为递归陷阱）" };
  }
  if (k === "chat") {
    // C8-1（D-193）：chat 体校验**先于**渲染器保留档——声明缺陷优先于渲染器进度。
    const pair = (v) =>
      Array.isArray(v) && v.length === 2 && v.every((x) => typeof x === "string");
    const v = d.view;
    if (!pair(v.sessionSource) || !pair(v.historyRpc) || !pair(v.sendRpc)) {
      return {
        code: "view-malformed",
        message: "chat 视图须有 sessionSource/historyRpc/sendRpc 三个 [ns,method] 面",
      };
    }
    if (v.stream !== "session-events") {
      return { code: "view-malformed", message: 'chat.stream 必须恰为 "session-events"（闭集）' };
    }
  }
  if (UNIMPLEMENTED.indexOf(k) >= 0) {
    return { code: "renderer-unimplemented", message: "view.kind=\"" + k + "\" 渲染器尚未实现（契约已定档）" };
  }
  if (IMPLEMENTED.indexOf(k) < 0) {
    return { code: "view-kind-unknown", message: "未定义的 view.kind=\"" + k + "\"" };
  }
  if (k === "form" && (!Array.isArray(d.view.fields) || !Array.isArray(d.view.actions))) {
    return { code: "view-malformed", message: "form 视图缺 fields/actions 数组" };
  }
  if (k === "list" && typeof d.view.rowsPath !== "string") {
    return { code: "view-malformed", message: "list 视图缺 rowsPath（数据面位置必须显式）" };
  }
  if (k === "list" && d.view.rowActions !== undefined) {
    if (!Array.isArray(d.view.rowActions)) {
      return { code: "view-malformed", message: "list.rowActions 必须是数组" };
    }
    for (const ra of d.view.rowActions) {
      const rpcOk =
        Array.isArray(ra?.rpc) &&
        ra.rpc.length === 2 &&
        ra.rpc.every((x) => typeof x === "string");
      if (!ra || typeof ra.name !== "string" || !rpcOk) {
        return { code: "view-malformed", message: "rowActions 项须含 name 与 [ns,method] rpc" };
      }
    }
  }
  return null;
}

/**
 * 收集表单值（read(name) -> 原始字符串）。number → 数值；list → JSON.parse（失败
 * fail-loud 抛 {field,message}——动作不得发出）。语义承 D-180。
 */
export function collectValues(view, read) {
  const data = {};
  for (const f of view.fields) {
    const raw = read(f.name);
    if (f.type === "list") {
      try {
        data[f.name] = JSON.parse(raw || "[]");
      } catch (e) {
        const err = new Error("字段 " + f.name + " 不是合法 JSON: " + e.message);
        err.field = f.name;
        throw err;
      }
    } else if (f.type === "number") {
      data[f.name] = Number(raw);
    } else {
      data[f.name] = raw;
    }
  }
  return data;
}

/**
 * RPC wire = client-request 信封（与前端 rpc 通道 / handle_rpc_host 的
 * rpc_envelope_ok 对齐）。历史教训：裸 {args} 经真实 HTTP 必 400（D-184）。
 */
export function rpcEnvelope(method, args, rpcId) {
  return { type: "client-request", rpcId: rpcId, method: method, payload: { args: args || {} } };
}

/** 轮询决策：`unchanged` → 保留现状；rev 变 → 整模型替换（C5 SSE 落地前的实时通道）。 */
export function pollDecision(current, value) {
  if (!value || value.unchanged === true) return { action: "keep" };
  return { action: "replace", rev: value.rev, cards: value.cards };
}

/** 焦点身份（点侧栏名 → 该卡滚动+高亮；不改布局输入）。 */
export function focusKey(card) {
  return card.pluginName + "/" + card.cardId;
}

// ---- C4：list/status 数据面（行语义只信单元数据——双权威禁令；永不伪造） ----

/** 点路径提取（"items" / "a.b"）；任何一段不是对象 → undefined。 */
export function extractPath(obj, dotted) {
  if (!obj || typeof obj !== "object" || typeof dotted !== "string" || dotted === "") return undefined;
  let cur = obj;
  for (const seg of dotted.split(".")) {
    if (cur === null || typeof cur !== "object") return undefined;
    cur = cur[seg];
  }
  return cur;
}

/**
 * list 行提取（优先级：dataRpc 值[rowsPath] > 静态 view.rows > 诚实空）。
 * 非数组一律视同「没有数据」——**绝不把对象/字符串拼成行**（诚实）。
 */
export function listRows(view, dataValue) {
  const fromData = Array.isArray(extractPath(dataValue, view.rowsPath))
    ? extractPath(dataValue, view.rowsPath)
    : null;
  const rows = fromData ?? (Array.isArray(view.rows) ? view.rows : []);
  return {
    rows,
    columns: Array.isArray(view.columns) ? view.columns : [],
    emptyText: view.emptyText || "暂无条目",
  };
}

/** status 项提取（优先级：dataRpc 值.items > 静态 view.items > 诚实空）。 */
export function statusItems(view, dataValue) {
  const fromData = Array.isArray(extractPath(dataValue, "items")) ? extractPath(dataValue, "items") : null;
  return fromData ?? (Array.isArray(view.items) ? view.items : []);
}

// ---- C6（D-189）：行动作（rowActions）----

/** 行动作线形状：整行原样入 `row`（渲染器不挑字段；单元自己校验身份——渲染器不是安全边界）。 */
export function rowActionBody(row) {
  return { row: row };
}

/** `confirm` 语义：只认严格 true（缺省/其他值 = 直接执行，向后兼容，无静默强制）。 */
export function needsConfirm(action) {
  return !!action && action.confirm === true;
}

// ---- C8-1（D-193）：chat 折叠/选择器（纯函数，DOM 只做接线） ----

/** EventKind 帧折叠进会话视图状态。
 *  state = {sessionId, busy:boolean, messages:[{role:"user"|"assistant"|"system", text, ts, pending?}]}
 *  frame = {sessionId, kind, data, time}（kind = dsh-session EventKind 规范串）。
 *  规则：非所选会话/未列举 kind → **原样返回同一引用**（渲染器以引用差决定是否重绘）；
 *  user/message 对齐 pending 乐观气泡；assistant/message|chunk 合并进当前 assistant 气泡；
 *  turn/start|end → busy；command/run|done → 系统行。绝不改动传入 state。 */
export function chatFoldFrame(state, frame) {
  if (!frame || frame.sessionId !== state.sessionId) return state;
  const kind = frame.kind;
  const msgs = state.messages;
  const last = msgs.length > 0 ? msgs[msgs.length - 1] : null;
  const next = (arr, busy) => ({
    sessionId: state.sessionId,
    busy: busy === undefined ? state.busy : busy,
    messages: arr,
  });
  const textOf = (f) =>
    f.data && f.data.text != null ? String(f.data.text) : "";
  if (kind === "user/message") {
    if (last && last.role === "user" && last.pending) {
      const merged = Object.assign({}, last, { text: textOf(frame), pending: false, ts: frame.time != null ? frame.time : last.ts });
      return next(msgs.slice(0, -1).concat([merged]));
    }
    return next(msgs.concat([{ role: "user", text: textOf(frame), ts: frame.time }]));
  }
  if (kind === "assistant/message" || kind === "assistant/chunk") {
    if (last && last.role === "assistant") {
      const merged = Object.assign({}, last, { text: last.text + textOf(frame) });
      return next(msgs.slice(0, -1).concat([merged]));
    }
    return next(msgs.concat([{ role: "assistant", text: textOf(frame), ts: frame.time }]));
  }
  if (kind === "turn/start") return next(msgs, true);
  if (kind === "turn/end") return next(msgs, false);
  if (kind === "command/run" || kind === "command/done") {
    const verb = kind === "command/run" ? "命令运行" : "命令完成";
    const name = frame.data && frame.data.name != null ? " " + String(frame.data.name) : "";
    return next(msgs.concat([{ role: "system", text: verb + name, ts: frame.time }]));
  }
  return state;
}

/** 会话选择器选项：list 行 → [{value,label}]（脏行跳过；running → 忙/闲 标记）。 */
export function chatOptions(rows) {
  const out = [];
  for (const r of Array.isArray(rows) ? rows : []) {
    if (!r || typeof r.sessionId !== "string") continue;
    out.push({ value: r.sessionId, label: r.sessionId + (r.running ? "·忙" : "·闲") });
  }
  return out;
}

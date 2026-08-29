// 桌布壳核心纯函数（C3，D-184）。零 DOM / 零 fetch / 零 eval——一切可证的都在这一层证
// （测试：tests/core.test.mjs；契约：.spec/service-assembly-ui-c3/design.md §2）。
// 不变式（D-179/D-181）：声明是数据不是代码——这里只消费数据，永不执行插件产物。

"use strict";

/** 分类轴闭集序（D-181 §3；misc 恒末）。加值走 DECISIONS，不需渲染器。 */
export const TYPE_ORDER = ["model", "config", "capability", "runtime", "resource", "session", "misc"];

const SCHEMA = "dsh/plugin-ui/v2";
const REJECTED = ["board"]; // 画布本身；卡内嵌画布 = 递归陷阱
const RESERVED = ["chat", "chart", "table"]; // 契约预留：签名已定，渲染器未建
// C3 点亮 form；status/list 属 C4（本轮落 renderer-unimplemented——三档制回落，不虚报）。
const IMPLEMENTED = ["form"];
const UNIMPLEMENTED = RESERVED.concat(["status", "list"]);

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
  if (UNIMPLEMENTED.indexOf(k) >= 0) {
    return { code: "renderer-unimplemented", message: "view.kind=\"" + k + "\" 渲染器尚未实现（契约已定档）" };
  }
  if (IMPLEMENTED.indexOf(k) < 0) {
    return { code: "view-kind-unknown", message: "未定义的 view.kind=\"" + k + "\"" };
  }
  if (k === "form" && (!Array.isArray(d.view.fields) || !Array.isArray(d.view.actions))) {
    return { code: "view-malformed", message: "form 视图缺 fields/actions 数组" };
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

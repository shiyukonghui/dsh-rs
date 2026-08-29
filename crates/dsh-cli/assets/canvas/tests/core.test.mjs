// 桌布壳核心（core.js）测试规范——C3 的 TDD 主战场。
// 运行：node --test crates/dsh-cli/assets/canvas/tests/
// 原则：一切可证的东西都在纯函数层证明（排布无重叠、§7 逐行、wire 形状、轮询语义）。
import test from "node:test";
import assert from "node:assert/strict";
import {
  TYPE_ORDER,
  buildModel,
  layoutGrid,
  validateDeclaration,
  columnsForWidth,
  collectValues,
  rpcEnvelope,
  pollDecision,
  focusKey,
  extractPath,
  listRows,
  statusItems,
  rowActionBody,
  needsConfirm,
  chatFoldFrame,
  chatOptions,
} from "../core.js";

function card(pluginName, cardId, type, w, h) {
  return {
    pluginName,
    cardId,
    type,
    title: pluginName + " 标题",
    size: { w, h },
    declPath: "/plugins/" + pluginName + "/ui.json",
  };
}

// ---- buildModel：侧栏分组 + 计数 + 空组跳过 + 坏卡归 misc ----

test("buildModel groups by type in closed-set order, keeps declaration order, skips empty groups", () => {
  const m = buildModel({
    rev: "r1",
    cards: [
      card("a", "a.s", "model", 2, 3),
      card("b", "b.s", "runtime", 1, 1),
      card("c", "c.s", "model", 2, 2),
    ],
  });
  assert.equal(m.rev, "r1");
  assert.deepEqual(m.groups.map((g) => g.type), ["model", "runtime"]); // 枚举序；config 等空组不出
  assert.deepEqual(m.groups[0].cards.map((c) => c.pluginName), ["a", "c"]); // 组内保声明序
  assert.equal(m.groups[0].count, 2);
  assert.equal(m.groups[1].count, 1);
  // 侧栏只含有卡分类（未知 type 归一在清单层已完成，壳只按值分组）
  assert.deepEqual(TYPE_ORDER, ["model", "config", "capability", "runtime", "resource", "session", "misc"]);
});

test("buildModel keeps error entries as misc bad cards (装了但坏了必须可见)", () => {
  const m = buildModel({
    rev: "r2",
    cards: [
      card("good", "good.s", "config", 2, 3),
      { pluginName: "broken", declPath: "/plugins/broken/ui.json",
        error: { code: "schema-version-unsupported", message: "旧声明" } },
    ],
  });
  assert.deepEqual(m.groups.map((g) => g.type), ["config", "misc"]); // config(1) 在 misc(6) 前
  const bad = m.groups[1].cards[0];
  assert.equal(bad.bad, true, "error 条目必须标记坏卡");
  assert.equal(bad.error.code, "schema-version-unsupported");
  assert.equal(bad.pluginName, "broken");
});

// ---- layoutGrid：可证无重叠的 first-fit 瀑布流 ----

test("layoutGrid deterministic first-fit positions (two columns)", () => {
  const cards = [
    { key: "a", w: 2, h: 2 },
    { key: "b", w: 2, h: 1 },
    { key: "c", w: 1, h: 1 },
  ];
  const g = layoutGrid(cards, 2);
  assert.deepEqual(g.positions, [
    { key: "a", col: 0, row: 0, w: 2, h: 2 },
    { key: "b", col: 0, row: 2, w: 2, h: 1 },
    { key: "c", col: 0, row: 3, w: 1, h: 1 }, // 平手取最左
  ]);
  assert.equal(g.totalRows, 4);
});

test("layoutGrid fills the gap column before next row", () => {
  const cards = [
    { key: "a", w: 2, h: 2 },
    { key: "b", w: 1, h: 1 },
  ];
  const g = layoutGrid(cards, 3);
  assert.deepEqual(g.positions[1], { key: "b", col: 2, row: 0, w: 1, h: 1 });
  assert.equal(g.totalRows, 2);
});

test("layoutGrid clamps card width to available columns", () => {
  const g = layoutGrid([{ key: "wide", w: 5, h: 2 }], 2);
  assert.equal(g.positions[0].w, 2, "w>C 必须收为 C（设计 §5.2.1）");
  assert.equal(g.positions[0].col, 0);
});

test("layoutGrid property: no overlap / in bounds for seeded cards (narrow and wide)", () => {
  let seed = 20260905;
  const rnd = (n) => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % n;
  };
  for (const C of [1, 3, 6]) {
    const cards = [];
    for (let i = 0; i < 40; i++) cards.push({ key: "k" + i, w: 1 + rnd(4), h: 1 + rnd(8) });
    const g = layoutGrid(cards, C);
    assert.equal(g.positions.length, cards.length, "每卡都有坐标");
    const occ = [];
    for (const p of g.positions) {
      assert.ok(p.col >= 0 && p.col + p.w <= C, `不出界: col=${p.col} w=${p.w} C=${C}`);
      assert.ok(p.row >= 0 && p.h >= 1);
      assert.ok(p.row + p.h <= g.totalRows, "不出总高界");
      for (const q of occ) {
        const overlap =
          p.col < q.col + q.w && q.col < p.col + p.w && p.row < q.row + q.h && q.row < p.row + p.h;
        assert.ok(!overlap, `重叠: ${p.key} × ${q.key} (C=${C})`);
      }
      occ.push(p);
    }
  }
});

test("columnsForWidth derives columns from container width (10px 格距契约)", () => {
  // C = floor((W+gap)/(col+gap))，gap=10, col=260（默认几何）
  assert.equal(columnsForWidth(260), 1);
  assert.equal(columnsForWidth(540), 2);
  assert.equal(columnsForWidth(820), 3);
  assert.equal(columnsForWidth(50), 1, "窄到不足一列也要保证 ≥1");
});

// ---- validateDeclaration：§7 fail-loud 表逐行 ----

const goodForm = () => ({
  $schema: "dsh/plugin-ui/v2",
  kind: "card",
  cardId: "x.s",
  type: "model",
  title: "X",
  size: { w: 2, h: 3 },
  view: { kind: "form", fields: [], actions: [] },
});

test("validateDeclaration covers all nine fail-loud rows", () => {
  assert.equal(validateDeclaration(goodForm()), null, "好 form 直通");
  // 1 声明整体非 JSON 对象
  for (const bad of [null, "not json", 42, []]) {
    assert.equal(validateDeclaration(bad).code, "declaration-unparseable");
  }
  // 2 $schema 非 v2 → 不静默兼容
  const v1 = goodForm();
  v1.$schema = "dsh/plugin-ui/v1";
  assert.equal(validateDeclaration(v1).code, "schema-version-unsupported");
  const noSchema = goodForm();
  delete noSchema.$schema;
  assert.equal(validateDeclaration(noSchema).code, "schema-version-unsupported");
  // 3 顶层 kind ≠ card
  const topForm = goodForm();
  topForm.kind = "form";
  assert.equal(validateDeclaration(topForm).code, "card-kind-unknown");
  // 4 view 缺失 / view.kind 缺失 → view-malformed
  const noView = goodForm();
  delete noView.view;
  assert.equal(validateDeclaration(noView).code, "view-malformed");
  const noKind = goodForm();
  delete noKind.view.kind;
  assert.equal(validateDeclaration(noKind).code, "view-malformed");
  // 5 board → 显式否决（不降级成 list）
  const board = goodForm();
  board.view = { kind: "board" };
  assert.equal(validateDeclaration(board).code, "view-kind-rejected");
  // 6 契约预留 → renderer-unimplemented（C4 起 status/list **已实现**，只剩三员预留）。
  // C8-1（D-193）迁移注记：chat 有形状要求——裸 chat 体先落 view-malformed（形状校验
  // 先于保留档）；齐形 chat → renderer-unimplemented 由 C8 专测覆盖。此处留无要求两员。
  for (const k of ["chart", "table"]) {
    const d = goodForm();
    d.view = { kind: k };
    assert.equal(validateDeclaration(d).code, "renderer-unimplemented", k);
  }
  for (const k of ["status", "list"]) {
    const d = goodForm();
    d.view = { kind: k, ...(k === "list" ? { rowsPath: "items" } : {}) };
    assert.equal(validateDeclaration(d), null, k + " 已点亮（C4）");
  }
  // 6b list 缺 rowsPath → view-malformed（数据面位置必须显式）
  const listNoPath = goodForm();
  listNoPath.view = { kind: "list" };
  assert.equal(validateDeclaration(listNoPath).code, "view-malformed");
  // 7 未定义 kind
  const weird = goodForm();
  weird.view = { kind: "widget" };
  assert.equal(validateDeclaration(weird).code, "view-kind-unknown");
  // 8 form 体不合契约（缺 fields / actions）
  const noFields = goodForm();
  delete noFields.view.fields;
  assert.equal(validateDeclaration(noFields).code, "view-malformed");
  const noActions = goodForm();
  noActions.view.actions = "nope";
  assert.equal(validateDeclaration(noActions).code, "view-malformed");
});

// ---- form 数据面：collect 语义 + fail-loud ----

test("collectValues converts number/select/list and fails loud on bad list JSON", () => {
  const view = {
    kind: "form",
    fields: [
      { name: "n", type: "number" },
      { name: "t", type: "text" },
      { name: "s", type: "select" },
      { name: "l", type: "list" },
    ],
    actions: [],
  };
  const raw = { n: "42", t: "x", s: "enabled", l: '[{"id":"m1"}]' };
  assert.deepEqual(collectValues(view, (name) => raw[name]), {
    n: 42,
    t: "x",
    s: "enabled",
    l: [{ id: "m1" }],
  });
  const bad = { n: "1", t: "", s: "", l: "not json" };
  assert.throws(
    () => collectValues(view, (name) => bad[name]),
    (e) => e.field === "l" && typeof e.message === "string",
    "list 解析失败必须 fail-loud 且指名字段（动作不得发出）"
  );
});

// ---- wire：client-request 信封（demo renderer 裸 {args} 缺陷的同形探针） ----

test("rpcEnvelope emits exact client-request wire shape", () => {
  assert.deepEqual(rpcEnvelope("llm-deepseek/save", { values: { a: 1 } }, "r9"), {
    type: "client-request",
    rpcId: "r9",
    method: "llm-deepseek/save",
    payload: { args: { values: { a: 1 } } },
  });
});

// ---- 轮询：rev 协商语义 ----

test("pollDecision keeps on unchanged, replaces on changed rev", () => {
  const cur = { rev: "r1" };
  assert.deepEqual(pollDecision(cur, { rev: "r1", unchanged: true }), { action: "keep" });
  const next = pollDecision(cur, { rev: "r2", cards: [{ pluginName: "x" }] });
  assert.equal(next.action, "replace");
  assert.equal(next.rev, "r2");
  assert.equal(next.cards.length, 1);
});

// ---- 焦点：不改布局（S6 模型级证明） ----

test("focusKey is stable card identity; layout has no hidden state", () => {
  assert.equal(focusKey({ pluginName: "a", cardId: "a.s" }), "a/a.s");
  const cards = [
    { key: "a", w: 2, h: 2 },
    { key: "b", w: 1, h: 1 },
  ];
  const g1 = layoutGrid(cards, 3);
  const g2 = layoutGrid(cards, 3); // 「点了焦点再来一次」
  assert.deepEqual(g1, g2, "同输入同坐标——focus 只是视图态，布局输入不变");
});

// ---- C4：list/status 数据面纯函数（行语义只信单元数据，永不伪造） ----

test("extractPath walks dotted paths, missing → undefined", () => {
  assert.equal(extractPath({ items: [1] }, "items").length, 1);
  assert.equal(extractPath({ a: { b: 7 } }, "a.b"), 7);
  assert.equal(extractPath({ a: 3 }, "a.b"), undefined, "非对象中段 → undefined");
  assert.equal(extractPath(null, "a"), undefined);
  assert.equal(extractPath({ x: 1 }, ""), undefined);
});

test("listRows: dataRpc value by rowsPath > static view.rows > honest empty", () => {
  const view = { kind: "list", rowsPath: "items", columns: [{ key: "name", label: "插件" }] };
  assert.deepEqual(listRows(view, { items: [{ name: "a" }] }).rows, [{ name: "a" }]);
  assert.deepEqual(listRows(view, { other: 1 }).rows, [], "rowsPath 不中且无静态兜底 → 空");
  const withStatic = { ...view, rows: [{ name: "s" }] };
  assert.deepEqual(listRows(withStatic, null).rows, [{ name: "s" }], "拉不到 → 静态兜底");
  const empty = listRows({ kind: "list", rowsPath: "items" }, {});
  assert.deepEqual(empty.rows, []);
  assert.equal(empty.emptyText, "暂无条目", "诚实空态默认文案");
  assert.equal(listRows({ kind: "list", rowsPath: "i", emptyText: "没有入口" }, {}).emptyText, "没有入口");
  assert.deepEqual(listRows(view, { items: [{ name: "a" }] }).columns, view.columns, "columns 透传");
});

test("listRows never fabricates: non-array rowsPath target falls back honest", () => {
  const view = { kind: "list", rowsPath: "items" };
  assert.deepEqual(listRows(view, { items: "not an array" }).rows, []);
  assert.deepEqual(listRows(view, { items: { 0: "x" } }).rows, [], "对象不是数组 → 不伪造行");
});

test("statusItems: dataRpc items > static view.items > honest empty", () => {
  assert.deepEqual(statusItems({ kind: "status" }, { items: [{ label: "L", value: 1 }] }), [
    { label: "L", value: 1 },
  ]);
  assert.deepEqual(statusItems({ kind: "status", items: [{ label: "S", value: 0 }] }, null), [
    { label: "S", value: 0 },
  ]);
  assert.deepEqual(statusItems({ kind: "status" }, {}), []);
});

// ---- C6（D-189）：行动作线形状 + confirm 语义 ----

test("rowActionBody wraps the full row untouched (wire contract)", () => {
  const row = { pluginId: "hello", name: "Hello v2", state: "running" };
  assert.deepEqual(rowActionBody(row), {
    row: { pluginId: "hello", name: "Hello v2", state: "running" },
  });
});

test("needsConfirm only strict true (no silent enforcement, no silent skip)", () => {
  assert.equal(needsConfirm({ confirm: true }), true);
  assert.equal(needsConfirm({}), false);
  assert.equal(needsConfirm({ confirm: "true" }), false, "字符串 \"true\" 不触发");
  assert.equal(needsConfirm(null), false);
});

test("validateDeclaration rejects malformed rowActions", () => {
  const listView = () => {
    const d = goodForm();
    d.view = { kind: "list", rowsPath: "items" };
    return d;
  };
  const okCard = listView();
  okCard.view.rowActions = [
    { name: "stop", label: "停止", rpc: ["ns", "stop"], scope: "row", confirm: true },
  ];
  assert.equal(validateDeclaration(okCard), null, "合法 rowActions 直通");
  const notArr = listView();
  notArr.view.rowActions = "nope";
  assert.equal(validateDeclaration(notArr).code, "view-malformed");
  const missingName = listView();
  missingName.view.rowActions = [{ rpc: ["ns", "stop"] }];
  assert.equal(validateDeclaration(missingName).code, "view-malformed");
  const badRpc = listView();
  badRpc.view.rowActions = [{ name: "x", rpc: ["ns", "a", "b"] }];
  assert.equal(validateDeclaration(badRpc).code, "view-malformed");
});

// ---- C8-1（D-193）：chat 契约校验 + 折叠/选择器纯函数 ----

const goodChat = () => {
  const d = goodForm();
  d.view = {
    kind: "chat",
    sessionSource: ["session", "list"],
    historyRpc: ["session", "history"],
    sendRpc: ["session", "prompt"],
    stream: "session-events",
  };
  return d;
};

test("chat shape validated ahead of renderer reservation", () => {
  // 形状齐 → 仍 renderer-unimplemented（C8-3 前渲染器保留档，语义如实）。
  assert.equal(validateDeclaration(goodChat()).code, "renderer-unimplemented");
  // 形状缺 → view-malformed 抢在保留档之前（声明缺陷优先于渲染器进度）。
  const noHist = goodChat();
  delete noHist.view.historyRpc;
  assert.equal(validateDeclaration(noHist).code, "view-malformed");
  const badSend = goodChat();
  badSend.view.sendRpc = ["session", "a", "b"];
  assert.equal(validateDeclaration(badSend).code, "view-malformed");
  const badStream = goodChat();
  badStream.view.stream = "sse";
  assert.equal(validateDeclaration(badStream).code, "view-malformed");
  const noStream = goodChat();
  delete noStream.view.stream;
  assert.equal(validateDeclaration(noStream).code, "view-malformed");
});

const chatState = () => ({ sessionId: "s-1", busy: false, messages: [] });

test("chatFoldFrame: foreign session ignored by reference identity", () => {
  const s = chatState();
  const out = chatFoldFrame(s, { sessionId: "other", kind: "user/message", data: { text: "x" }, time: 1 });
  assert.strictEqual(out, s, "非所选会话帧必须原样返回（同一引用）");
});

test("chatFoldFrame: optimistic user bubble aligned by real event", () => {
  const s = { sessionId: "s-1", busy: false, messages: [{ role: "user", text: "echo", pending: true }] };
  const out = chatFoldFrame(s, { sessionId: "s-1", kind: "user/message", data: { text: "echo" }, time: 7 });
  assert.equal(out.messages.length, 1, "对齐而非重复追加");
  assert.equal(out.messages[0].pending, false);
  assert.equal(out.messages[0].ts, 7);
  assert.equal(s.messages[0].pending, true, "原 state 不得被改动（纯函数）");
});

test("chatFoldFrame: user push when no pending bubble; assistant merge and push", () => {
  let s = chatState();
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "user/message", data: { text: "hi" }, time: 1 });
  assert.equal(s.messages.length, 1);
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "assistant/message", data: { text: "Hel" }, time: 2 });
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "assistant/chunk", data: { text: "lo" }, time: 3 });
  assert.equal(s.messages.length, 2, "chunk 延续 assistant 气泡不新开");
  assert.equal(s.messages[1].text, "Hello");
});

test("chatFoldFrame: turn busy flags and system line for command kinds", () => {
  let s = chatState();
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "turn/start", data: {}, time: 1 });
  assert.equal(s.busy, true);
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "command/run", data: { name: "plan" }, time: 2 });
  const sys = s.messages[s.messages.length - 1];
  assert.equal(sys.role, "system");
  assert.ok(String(sys.text).includes("plan"), "系统行带命令名");
  s = chatFoldFrame(s, { sessionId: "s-1", kind: "turn/end", data: {}, time: 3 });
  assert.equal(s.busy, false);
});

test("chatFoldFrame: unknown kinds ignored by reference identity", () => {
  const s = chatState();
  const out = chatFoldFrame(s, { sessionId: "s-1", kind: "hook/invoked", data: {}, time: 1 });
  assert.strictEqual(out, s, "未列举 kind 原样返回（不产生系统噪音）");
});

test("chatOptions: rows to selector options, junk rows skipped", () => {
  const opts = chatOptions([
    { sessionId: "a", running: true },
    { sessionId: "b", running: false },
    { running: true },
    { sessionId: 5 },
    null,
  ]);
  assert.deepEqual(opts, [
    { value: "a", label: "a·忙" },
    { value: "b", label: "b·闲" },
  ]);
  assert.deepEqual(chatOptions(null), []);
});

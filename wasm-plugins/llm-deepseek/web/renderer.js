// 桌布渲染契约最小实现（C1）：消费 dsh/plugin-ui/v2「卡片 + 视图」声明。
// 原则：只读声明（数据）、无任意 JS 执行；未实现 / 被否决 / 坏声明一律 fail-loud 元数据卡，
// 绝不白屏、绝不伪造（契约见 .spec/service-assembly-ui-canvas/design.md §7）。
(function () {
  "use strict";

  var SCHEMA = "dsh/plugin-ui/v2";
  var SIZE_CAP = { w: 4, h: 8 };                    // 契约封顶：超出裁剪 + 记录（降级非失败）
  var IMPLEMENTED = ["form"];                       // C1 实现（status/list 属 C4）
  var RESERVED = ["chat", "chart", "table"];        // 契约预留：渲染器未建
  var REJECTED = ["board"];                         // 否决：画布本身，卡内嵌画布 = 递归

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }
  function status(text, ok) {
    var s = document.getElementById("status");
    s.textContent = text;
    s.className = ok ? "ok" : "err";
  }

  // fail-loud 元数据回落卡：显式呈现契约缺陷，但卡级动作仍可用
  // （能用的功能不因渲染器缺失而消失）。
  function failCard(decl, code, message) {
    var form = document.getElementById("ui");
    form.innerHTML = "";
    var badge = (decl && decl.type ? decl.type : "?") + " · " +
      (decl && decl.view && decl.view.kind ? decl.view.kind : "?");
    form.appendChild(el("div", "err", "✗ " + message + "（code=" + code + "）"));
    form.appendChild(el("div", "", "卡片：" + badge));
    renderActions(decl);
  }

  // 声明校验（契约 §7 fail-loud 表；返回 {code,message} 或 null）
  function validate(d) {
    if (!d || typeof d !== "object" || Array.isArray(d)) {
      return { code: "declaration-unparseable", message: "声明不是 JSON 对象" };
    }
    if (d.$schema !== SCHEMA) {
      return { code: "schema-version-unsupported",
        message: "仅支持 " + SCHEMA + "，收到 " + d.$schema + "（不做静默兼容）" };
    }
    if (d.kind !== "card") {
      return { code: "card-kind-unknown", message: "顶层容器只支持 kind:\"card\"，收到 " + d.kind };
    }
    if (!d.view || typeof d.view !== "object" || !d.view.kind) {
      return { code: "view-malformed", message: "卡片缺 view 或 view.kind" };
    }
    var k = d.view.kind;
    if (REJECTED.indexOf(k) >= 0) {
      return { code: "view-kind-rejected",
        message: "view.kind=\"" + k + "\" 被契约否决（画布本身，卡内嵌画布为递归陷阱）" };
    }
    if (RESERVED.indexOf(k) >= 0) {
      return { code: "renderer-unimplemented",
        message: "view.kind=\"" + k + "\" 契约已预留，渲染器尚未实现" };
    }
    if (IMPLEMENTED.indexOf(k) < 0) {
      return { code: "view-kind-unknown", message: "未定义的 view.kind=\"" + k + "\"" };
    }
    if (k === "form" && (!Array.isArray(d.view.fields) || !Array.isArray(d.view.actions))) {
      return { code: "view-malformed", message: "form 视图缺 fields/actions 数组" };
    }
    return null;
  }

  // size 裁剪（降级 + 记录，不崩）
  function sizeOf(decl) {
    var s = (decl.size && typeof decl.size === "object") ? decl.size : {};
    var w = Math.max(1, Number(s.w) || 1), h = Math.max(1, Number(s.h) || 1);
    var cw = Math.min(w, SIZE_CAP.w), ch = Math.min(h, SIZE_CAP.h);
    if (cw !== w || ch !== h) {
      status("ℹ size 越契约上限，已裁剪为 " + cw + "×" + ch + "（诊断：size-clamped）", true);
    }
    return cw + "×" + ch + " 格";
  }

  function fieldInput(field, value) {
    var v = value === undefined || value === null ? field.default : value;
    switch (field.type) {
      case "select": {
        var sel = document.createElement("select");
        sel.name = field.name;
        (field.options || []).forEach(function (opt) {
          var o = document.createElement("option");
          o.value = String(opt); o.textContent = String(opt);
          if (String(opt) === String(v)) o.selected = true;
          sel.appendChild(o);
        });
        return sel;
      }
      case "number": {
        var n = document.createElement("input");
        n.type = "number"; n.name = field.name;
        if (field.min !== undefined) n.min = field.min;
        if (v !== undefined) n.value = v;
        return n;
      }
      case "list": {
        var box = el("div"); box.dataset.name = field.name;
        box.appendChild(el("span", null, "列表（JSON 数组，试点最小集）："));
        var ta = document.createElement("textarea");
        ta.rows = 5; ta.name = field.name;
        ta.placeholder = "JSON 数组，如 [{\"id\":\"deepseek-v4-flash\"}]";
        ta.value = JSON.stringify(Array.isArray(v) ? v : [], null, 1);
        box.appendChild(document.createElement("br"));
        box.appendChild(ta);
        return box;
      }
      default: {
        var t = document.createElement("input");
        t.type = "text"; t.name = field.name;
        if (field.role === "credential-ref") t.placeholder = "环境变量名（凭证引用）";
        if (v !== undefined) t.value = v;
        return t;
      }
    }
  }

  function renderFields(view, values) {
    var form = document.getElementById("ui");
    form.innerHTML = "";
    view.fields.forEach(function (f) {
      var label = el("label", null, f.label + (f.required ? " *" : ""));
      label.appendChild(fieldInput(f, values ? values[f.name] : undefined));
      form.appendChild(label);
    });
  }

  function collectValues(view) {
    var data = {};
    view.fields.forEach(function (f) {
      var node = document.querySelector(
        "input[name=\"" + f.name + "\"], select[name=\"" + f.name + "\"], textarea[name=\"" + f.name + "\"]");
      if (f.type === "list") {
        try { data[f.name] = JSON.parse(node.value || "[]"); }
        catch (e) { throw new Error("字段 " + f.name + " 不是合法 JSON 数组: " + e.message); }
      } else if (f.type === "number") {
        data[f.name] = Number(node.value);
      } else {
        data[f.name] = node.value;
      }
    });
    return data;
  }

  function callRpc(ns, method, args) {
    // client-request 信封（rpc_envelope_ok 纪律）：裸 {args} 经真实 HTTP 必 400（D-184 修复）。
    var rid = "demo-" + Date.now() + "-" + Math.floor(Math.random() * 1e6);
    return fetch("/api/" + ns + "/" + method, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        type: "client-request",
        rpcId: rid,
        method: ns + "/" + method,
        payload: { args: args || {} },
      }),
    }).then(function (r) { return r.json(); });
  }

  // 动作：契约 rpc = [namespace, method]；save 的 args = {values}
  function renderActions(decl) {
    var box = document.getElementById("actions");
    box.innerHTML = "";
    var actions = (decl && decl.view && Array.isArray(decl.view.actions)) ? decl.view.actions : [];
    actions.forEach(function (a) {
      if (!Array.isArray(a.rpc) || a.rpc.length !== 2) return;
      var btn = el("button", a.primary ? "primary" : null, a.label);
      btn.addEventListener("click", function () {
        var args;
        try { args = { values: collectValues(decl.view) }; }
        catch (e) { status("✗ " + e.message, false); return; }
        status("→ " + a.rpc[0] + "/" + a.rpc[1] + " …", true);
        callRpc(a.rpc[0], a.rpc[1], args).then(function (res) {
          if (res && res.ok !== false) {
            if (a.rpc[1] === "discoverModels") {
              var ms = (res.value && res.value.models) || [];
              status("✓ 发现模型 " + ms.length + " 个：" +
                ms.map(function (m) { return m.id; }).join(", "), true);
            } else {
              status("✓ " + a.rpc.join("/") + "：" + JSON.stringify(res.value || {}), true);
            }
          } else {
            var err = (res && res.error) || {};
            status("✗ " + (err.message || "操作失败") + "（code=" + (err.code || "?") + "）", false);
          }
        }).catch(function (e) { status("✗ 网络/解析错误：" + e.message, false); });
      });
      box.appendChild(btn);
    });
  }

  // 启动：拉静态声明 → 校验（fail-loud）→ 按 view.kind 分派 → dataRpc 预填
  fetch("/plugins/llm-deepseek/ui.json")
    .then(function (r) {
      if (!r.ok) throw { code: "declaration-unfetchable", message: "GET ui.json HTTP " + r.status };
      return r.json();
    })
    .then(function (decl) {
      document.getElementById("title").textContent = (decl && decl.title) || "插件卡片";
      var desc = (decl && decl.description) || "";
      var bad = validate(decl);
      if (bad) { document.getElementById("description").textContent = desc; failCard(decl, bad.code, bad.message); return; }
      document.getElementById("description").textContent = desc + "（" + decl.type + " 卡 · " + sizeOf(decl) + "）";
      renderActions(decl);
      var prefill = {};
      var rpc = decl.view.dataRpc;
      var boot = Array.isArray(rpc) && rpc.length === 2
        ? callRpc(rpc[0], rpc[1], {}).then(function (res) {
            if (res && res.ok !== false && res.value && res.value.values) prefill = res.value.values;
          }).catch(function () { /* 拉不到就用声明默认值，诚实 */ })
        : Promise.resolve();
      return boot.then(function () { renderFields(decl.view, prefill); });
    })
    .catch(function (e) {
      failCard(null, (e && e.code) ? e.code : "declaration-unparseable", (e && e.message) || String(e));
    });
})();
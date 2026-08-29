//! D-183：桌布 C2——宿主实时清单聚合（`uiManifest/list` 的发现面核心）。
//!
//! 契约（`.spec/service-assembly-ui-canvas/design.md` §6，D-181 锁定；wire 形状 D-183）：
//! - **每请求从实时状态计算**（packages + loader entries + `web/ui.json` 实文件），
//!   **禁止启动期快照缓存**——热插拔是第一等要求（D-175/D-177 热更语义的前提）。
//! - 清单只含**元数据六元组** `{pluginName, cardId, type, title, size, declPath}`；
//!   `view` 内容不下发（渲染器按 `declPath` 另拉）。
//! - 归一在清单层完成（宿主 = 单一权威）：type 未知 → `misc`（保留 `declaredType`）；
//!   size 裁剪 `w∈[1,4]`/`h∈[1,8]`（改动记 `declaredSize`）；坐标键零输出（永不外泄）。
//! - **坏声明不静默丢**：`ui.json` 存在但坏 → error 条目（`declaration-unparseable` /
//!   `schema-version-unsupported` / `card-kind-unknown` / `card-id-missing`）；
//!   没有 `ui.json` 的包 = 无 UI，正常跳过（两者语义不同，必须区分）。
//! - `rev` = SHA-256(cards canonical JSON) 小写 hex——**内容哈希非单调计数**
//!   （重启后客户端缓存的 rev 仍有效；error 条目计入 rev）。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::plugin_pkg::PluginPackage;

/// `type` v1 闭集（D-181 §3，面向用户的分类轴）；未知/缺失 → `misc`。
const TYPE_CLOSED: [&str; 7] = [
    "model", "config", "capability", "runtime", "resource", "session", "misc",
];

/// 一次实时清单计算的结果。
pub struct UiManifest {
    /// cards 内容的 sha256 小写 hex（64 字符）。
    pub rev: String,
    /// 卡片条目（含坏声明 error 条目），= packages 声明序（无 priority）。
    pub cards: Vec<Value>,
}

/// 实时计算清单（**每次请求调用；无任何缓存**）。
/// `packages` 序 = 卡声明序；`entries` 来自 `loader.entries()`（group 条目不参与匹配）。
pub fn build_manifest(
    packages: &[PluginPackage],
    entries: &[dsh_loader::EntrySnapshot],
) -> UiManifest {
    let mut cards: Vec<Value> = Vec::new();
    for pkg in packages {
        // 无 web 目录 / 无 ui.json 文件 = **无 UI**，安静跳过（与「坏 UI 必可见」语义区分）。
        let Some(web) = pkg.web.as_ref() else { continue };
        let ui_path = web.join("ui.json");
        if !ui_path.is_file() {
            continue;
        }
        // disabled 交叉（D-183）：同名 entry **全部**禁用 → 排除；group 条目不参与；
        // 无同名 entry = 未 entry 化（试点现状）→ 生效。
        let named: Vec<&dsh_loader::EntrySnapshot> = entries
            .iter()
            .filter(|e| !e.group && e.name == pkg.name)
            .collect();
        if !named.is_empty() && named.iter().all(|e| e.disabled) {
            continue;
        }
        let decl_path = format!("/plugins/{}/ui.json", pkg.name);
        let entry = match std::fs::read_to_string(&ui_path) {
            Err(e) => error_entry(
                &pkg.name,
                &decl_path,
                "declaration-unparseable",
                &format!("read ui.json: {e}"),
            ),
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Err(e) => error_entry(
                    &pkg.name,
                    &decl_path,
                    "declaration-unparseable",
                    &format!("invalid JSON: {e}"),
                ),
                Ok(decl) => card_entry(&pkg.name, &decl_path, &decl),
            },
        };
        cards.push(entry);
    }
    let rev = content_rev(&cards);
    UiManifest { rev, cards }
}

/// `uiManifest/list` 的 wire 结果：取数（boot.packages + loader 生效 entries）→
/// [`build_manifest`] → `{ok, value:{rev, cards}}`；`args.rev` 与当前一致 →
/// `{rev, unchanged:true}`（无 cards，省带宽）。
pub fn ui_manifest_result(boot: &crate::Boot, payload: &Value) -> Value {
    // 与 dispatch_wasm_remote 同参数纪律：`payload.args` 缺失 → 用 payload 本身
    //（curl/直接调用透传）。
    let args = payload.get("args").unwrap_or(payload);
    let client_rev = args.get("rev").and_then(Value::as_str);
    // **每请求实时取数**（禁缓存）：无 loader ≠ 全禁用——空 entries = 全生效（诚实）。
    let entries: Vec<dsh_loader::EntrySnapshot> = boot
        .loader
        .as_ref()
        .map(|l| l.entries())
        .unwrap_or_default();
    let manifest = build_manifest(&boot.packages, &entries);
    if client_rev.is_some_and(|r| r == manifest.rev) {
        return json!({"ok": true, "value": {"rev": manifest.rev, "unchanged": true}});
    }
    json!({"ok": true, "value": {"rev": manifest.rev, "cards": manifest.cards}})
}

/// 坏声明条目：装了但坏了 → **必须可见**（画布据此画 fail-loud 卡）。
fn error_entry(plugin_name: &str, decl_path: &str, code: &str, message: &str) -> Value {
    json!({
        "pluginName": plugin_name,
        "declPath": decl_path,
        "error": { "code": code, "message": message },
    })
}

/// v2 声明 → 清单条目（校验链见 `.spec/service-assembly-ui-c2/design.md` §1.4：
/// unparseable → schema-version → card-kind → card-id → 归一）。
fn card_entry(plugin_name: &str, decl_path: &str, decl: &Value) -> Value {
    if !decl.is_object() {
        return error_entry(
            plugin_name,
            decl_path,
            "declaration-unparseable",
            "declaration is not a JSON object",
        );
    }
    let schema = decl.get("$schema").and_then(Value::as_str);
    if schema != Some("dsh/plugin-ui/v2") {
        return error_entry(
            plugin_name,
            decl_path,
            "schema-version-unsupported",
            &format!(
                "$schema must be \"dsh/plugin-ui/v2\", got {}",
                match schema {
                    Some(s) => format!("{s:?}"),
                    None => "missing".to_string(),
                }
            ),
        );
    }
    if decl.get("kind").and_then(Value::as_str) != Some("card") {
        return error_entry(
            plugin_name,
            decl_path,
            "card-kind-unknown",
            &format!(
                "v2 顶层唯一容器是 kind:\"card\", got {:?}",
                decl.get("kind").and_then(Value::as_str)
            ),
        );
    }
    let Some(card_id) = decl.get("cardId").and_then(Value::as_str).filter(|s| !s.is_empty())
    else {
        return error_entry(
            plugin_name,
            decl_path,
            "card-id-missing",
            "cardId 缺失或为空（卡身份 = (pluginName, cardId)，无法去重/聚焦）",
        );
    };
    // 归一（清单层 = 单一权威，渲染器只信清单）：type / size / title 三处。
    let declared_type = decl.get("type").and_then(Value::as_str);
    let entry_type = match declared_type {
        Some(t) if TYPE_CLOSED.contains(&t) => t.to_string(),
        _ => "misc".to_string(),
    };
    let view_kind = decl
        .get("view")
        .and_then(|v| v.get("kind"))
        .and_then(Value::as_str);
    let (size, declared_size) = resolve_size(decl.get("size"), view_kind);
    let mut entry = json!({
        "pluginName": plugin_name,
        "cardId": card_id,
        "type": entry_type,
        "title": decl
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(card_id),
        "size": size,
        "declPath": decl_path,
    });
    if let Some(dt) = declared_type.filter(|t| !TYPE_CLOSED.contains(t)) {
        entry["declaredType"] = json!(dt);
    }
    if let Some(ds) = declared_size {
        entry["declaredSize"] = ds;
    }
    entry
}

/// size 归一：数字 w/h → 裁剪 `w∈[1,4]`/`h∈[1,8]`（改动记 `declaredSize`，降级不是失败）；
/// 缺失/非法 → 按 `view.kind` 默认（D-183 裁定：status→2×2、list→4×4、其余→2×3）。
/// 输出**永不含坐标键**（x/y 等在此被自然丢弃——坐标永不外泄）。
fn resolve_size(size: Option<&Value>, view_kind: Option<&str>) -> (Value, Option<Value>) {
    let numeric = size
        .and_then(Value::as_object)
        .and_then(|o| Some((o.get("w")?.as_u64()?, o.get("h")?.as_u64()?)));
    let Some((w, h)) = numeric else {
        let (dw, dh) = match view_kind {
            Some("status") => (2, 2),
            Some("list") => (4, 4),
            _ => (2, 3),
        };
        return (json!({"w": dw, "h": dh}), None);
    };
    let (cw, ch) = (w.clamp(1, 4), h.clamp(1, 8));
    let declared = if (cw, ch) != (w, h) {
        Some(json!({"w": w, "h": h}))
    } else {
        None
    };
    (json!({"w": cw, "h": ch}), declared)
}

/// `rev` = SHA-256(cards canonical JSON) 小写 hex。**内容哈希非单调计数**：
/// 同内容同 rev（跨重启稳定，客户端缓存的 rev 仍有效）；error 条目计入 rev
///（坏声明被修好 = 清单内容变化）。输入不含绝对路径/时间戳等不稳定源。
fn content_rev(cards: &[Value]) -> String {
    let canonical = serde_json::to_string(cards).unwrap_or_else(|_| "[]".to_string());
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- C5（D-186）：运行时热插拔 watch（serve 主循环 tick 挂钩，2s 节流） ----

/// watch 节流窗口（毫秒）——tick 每 250ms 跑，重扫只按此窗口。
pub const UI_MANIFEST_WATCH_INTERVAL_MS: u64 = 2000;

/// 热插拔 watch 状态（serve 主循环私有；单线程纪律，无锁）。
pub struct UiManifestWatchState {
    pub last_check_ms: u64,
    pub last_rev: String,
    /// **只含 scan 挂载的**包名（卸载绝不碰 boot manifest 装配的其它 packages）。
    pub mounted: Vec<String>,
}

/// 启动基线：rev 现算（不广播——此刻无客户端）。
pub fn init_watch_state(boot: &crate::Boot) -> UiManifestWatchState {
    let entries = boot.loader.as_ref().map(|l| l.entries()).unwrap_or_default();
    UiManifestWatchState {
        last_check_ms: 0,
        last_rev: build_manifest(&boot.packages, &entries).rev,
        mounted: Vec::new(),
    }
}

/// 主循环 tick：节流重扫（**不构建**）→ 同步 boot（装/卸 scan 挂载的单元）→
/// rev 变则返回 Some(new_rev)（调用方经 `/plugins/events` 广播）。
/// 运行时装载体失败 → eprintln 跳过（不炸 serve、不上死卡；与启动 fail-loud 区分：
/// 启动是装配决策，运行时是热插事件）。卸载**只动 scan 挂载的包**（state.mounted）。
pub fn ui_manifest_watch_tick(
    boot: &mut crate::Boot,
    wasm_base: &std::path::Path,
    now_ms: u64,
    st: &mut UiManifestWatchState,
) -> Option<String> {
    if st.last_check_ms != 0
        && now_ms.saturating_sub(st.last_check_ms) < UI_MANIFEST_WATCH_INTERVAL_MS
    {
        return None;
    }
    st.last_check_ms = now_ms;
    let desired = crate::web::scan_remote_units_opts(wasm_base, false);
    let desired_names: Vec<&str> = desired.iter().map(|p| p.name.as_str()).collect();

    // 装：新出现的合格单元（启动 scan 已挂载的只登记，不重复）。
    for pkg in &desired {
        if st.mounted.contains(&pkg.name) {
            continue;
        }
        if boot.packages.iter().any(|p| p.name == pkg.name) {
            st.mounted.push(pkg.name.clone());
            continue;
        }
        let Ok(bytes) = std::fs::read(&pkg.wasm) else {
            continue; // scan 已保证存在；竞态缺文件 = 下轮再看
        };
        match dsh_wasmrt::WasmRemoteEndpointPlugin::new(
            Box::leak(pkg.name.clone().into_boxed_str()),
            &bytes,
            dsh_wasmrt::Capabilities::default(),
            None,
        ) {
            Ok(carrier) => {
                boot.remote_carriers
                    .push((pkg.name.clone(), std::rc::Rc::new(std::cell::RefCell::new(carrier))));
                boot.packages.push(pkg.clone());
                st.mounted.push(pkg.name.clone());
            }
            Err(e) => eprintln!("dsh web: hot-plug unit {} skipped (carrier load: {e})", pkg.name),
        }
    }

    // 卸：mounted 中已消失的（只动 scan 挂载的——boot manifest 装配的包绝不碰）。
    let gone: Vec<String> = st
        .mounted
        .iter()
        .filter(|n| !desired_names.contains(&n.as_str()))
        .cloned()
        .collect();
    for name in gone {
        boot.packages.retain(|p| p.name != name);
        boot.remote_carriers.retain(|(ns, _)| ns != &name);
        st.mounted.retain(|m| m != &name);
    }

    let entries = boot.loader.as_ref().map(|l| l.entries()).unwrap_or_default();
    let rev = build_manifest(&boot.packages, &entries).rev;
    if rev != st.last_rev {
        st.last_rev = rev.clone();
        Some(rev)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 临时包骨架：`base/<name>/{dummy.wasm, web/}`；返回 (base, 可直接进 manifest 的包)。
    fn tmp_pkg(tag: &str, name: &str) -> (PathBuf, PluginPackage) {
        let base = std::env::temp_dir().join(format!(
            "dsh-ui-manifest-{tag}-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join(name);
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(dir.join("dummy.wasm"), b"wasm").unwrap();
        let pkg = PluginPackage {
            name: name.to_string(),
            dir: dir.clone(),
            wasm: dir.join("dummy.wasm"),
            web: Some(dir.join("web")),
            caps: None,
            world: None,
        };
        (base, pkg)
    }

    fn write_ui(pkg: &PluginPackage, content: &str) {
        std::fs::write(pkg.web.as_ref().unwrap().join("ui.json"), content).unwrap();
    }

    /// 一张合契约的 v2 卡片声明（字段可覆写）。
    fn v2_card(card_id: &str, title: &str) -> Value {
        json!({
            "$schema": "dsh/plugin-ui/v2",
            "kind": "card",
            "cardId": card_id,
            "type": "model",
            "title": title,
            "size": { "w": 2, "h": 3 },
            "view": { "kind": "form", "fields": [], "actions": [] }
        })
    }

    fn snapshot(name: &str, disabled: bool) -> dsh_loader::EntrySnapshot {
        dsh_loader::EntrySnapshot {
            id: format!("e-{name}"),
            name: name.to_string(),
            disabled,
            group: false,
            fiber: None,
        }
    }

    fn group_snapshot(name: &str, disabled: bool) -> dsh_loader::EntrySnapshot {
        dsh_loader::EntrySnapshot { group: true, ..snapshot(name, disabled) }
    }

    /// 测试 1：两个好包 → cards 按声明序、六元组齐、无坐标键、无多余诊断字段。
    #[test]
    fn aggregates_two_good_packages_in_declaration_order() {
        let (base, mut a) = tmp_pkg("agg", "pkg-a");
        let (_, b) = tmp_pkg("agg", "pkg-b");
        write_ui(&a, &v2_card("pkg-a.settings", "A Card").to_string());
        // b 用 status 视图（size 缺省 → 2×2，见 size_defaults_by_view_kind）
        write_ui(
            &b,
            &json!({
                "$schema": "dsh/plugin-ui/v2", "kind": "card",
                "cardId": "pkg-b.status", "type": "runtime", "title": "B Status",
                "size": { "w": 3, "h": 2 },
                "view": { "kind": "status", "items": [] }
            })
            .to_string(),
        );

        let m = build_manifest(&[a.clone(), b.clone()], &[]);
        assert_eq!(m.cards.len(), 2, "两个好包两张卡，得 {:?}", m.cards);
        // 声明序 = packages 序（无 priority）
        assert_eq!(m.cards[0]["pluginName"], "pkg-a");
        assert_eq!(m.cards[1]["pluginName"], "pkg-b");
        let a0 = &m.cards[0];
        assert_eq!(a0["cardId"], "pkg-a.settings");
        assert_eq!(a0["type"], "model");
        assert_eq!(a0["title"], "A Card");
        assert_eq!(a0["size"], json!({"w": 2, "h": 3}));
        assert_eq!(a0["declPath"], "/plugins/pkg-a/ui.json");
        // 归一未触发 → 无诊断字段
        assert!(a0.get("declaredType").is_none());
        assert!(a0.get("declaredSize").is_none());
        // 坐标绝不外泄
        assert!(a0["size"].get("x").is_none() && a0["size"].get("y").is_none());
        assert_eq!(m.cards[1]["type"], "runtime");
        assert_eq!(m.cards[1]["size"], json!({"w": 3, "h": 2}));
        // a 的 web 无 ui.json 之前不该出现——顺手验证 wasm 字段与清单无关
        a.web = None;
        let m2 = build_manifest(&[a, b], &[]);
        assert_eq!(m2.cards.len(), 1, "无 web 目录 → 跳过");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 2：无 `web/` 或 `web/ui.json` 缺失 → 正常跳过（**不是** error 条目）。
    #[test]
    fn skips_packages_without_ui_json() {
        let (base, noweb) = {
            let (base, mut pkg) = tmp_pkg("skips", "pkg-noweb");
            pkg.web = None;
            (base, pkg)
        };
        let (_, nofile) = tmp_pkg("skips", "pkg-nofile");
        // pkg-nofile 有 web/ 但没写 ui.json
        let m = build_manifest(&[noweb, nofile], &[]);
        assert!(m.cards.is_empty(), "无 UI 的包必须安静跳过，得 {:?}", m.cards);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 3：坏声明四种坏法 → 各带 code 的 error 条目；同批好包照常出卡（坏不连坐）。
    #[test]
    fn broken_declarations_become_error_entries() {
        let (base, unparse) = tmp_pkg("bad", "pkg-unparse");
        let (_, v1) = tmp_pkg("bad", "pkg-v1");
        let (_, kindbad) = tmp_pkg("bad", "pkg-kindbad");
        let (_, nocard) = tmp_pkg("bad", "pkg-nocard");
        let (_, good) = tmp_pkg("bad", "pkg-good");
        write_ui(&unparse, "not json{");
        write_ui(
            &v1,
            &json!({"$schema": "dsh/plugin-ui/v1", "kind": "form", "fields": []}).to_string(),
        );
        write_ui(
            &kindbad,
            &json!({"$schema": "dsh/plugin-ui/v2", "kind": "form", "cardId": "x"}).to_string(),
        );
        write_ui(
            &nocard,
            &json!({"$schema": "dsh/plugin-ui/v2", "kind": "card", "type": "misc"})
                .to_string(),
        );
        write_ui(&good, &v2_card("good.card", "Good").to_string());

        let m = build_manifest(&[unparse, v1, kindbad, nocard, good], &[]);
        assert_eq!(m.cards.len(), 5, "坏包不静默丢，得 {:?}", m.cards);
        let code_of = |name: &str| -> String {
            let e = m
                .cards
                .iter()
                .find(|c| c["pluginName"] == name)
                .unwrap_or_else(|| panic!("缺 {name} 条目"));
            e["error"]["code"].as_str().unwrap_or("<none>").to_string()
        };
        assert_eq!(code_of("pkg-unparse"), "declaration-unparseable");
        assert_eq!(code_of("pkg-v1"), "schema-version-unsupported");
        assert_eq!(code_of("pkg-kindbad"), "card-kind-unknown");
        assert_eq!(code_of("pkg-nocard"), "card-id-missing");
        // 好包照常
        assert_eq!(code_of("pkg-good"), "<none>");
        // error 条目带 pluginName + declPath + error.message（可诊断）
        let e = m.cards.iter().find(|c| c["pluginName"] == "pkg-v1").unwrap();
        assert_eq!(e["declPath"], "/plugins/pkg-v1/ui.json");
        assert!(e["error"]["message"].as_str().is_some_and(|s| !s.is_empty()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 4：未知/缺失 type → `misc`（未知保留 `declaredType`；缺失不造原值）。
    #[test]
    fn unknown_type_falls_to_misc_keeping_declared_type() {
        let (base, unknown) = tmp_pkg("ty", "pkg-unknown-type");
        let (_, missing) = tmp_pkg("ty", "pkg-missing-type");
        let mut card = v2_card("u.t", "U");
        card["type"] = json!("llm"); // 闭集外
        write_ui(&unknown, &card.to_string());
        let mut card2 = v2_card("m.t", "M");
        card2.as_object_mut().unwrap().remove("type");
        write_ui(&missing, &card2.to_string());

        let m = build_manifest(&[unknown, missing], &[]);
        assert_eq!(m.cards.len(), 2);
        assert_eq!(m.cards[0]["type"], "misc");
        assert_eq!(m.cards[0]["declaredType"], "llm");
        assert_eq!(m.cards[1]["type"], "misc");
        assert!(m.cards[1].get("declaredType").is_none(), "缺失 type 不造原值");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 5：size 越上限 → 裁剪 + `declaredSize` 记录；w:0 → 抬到 1；x/y 键不泄漏。
    #[test]
    fn oversized_size_clamped_and_recorded() {
        let (base, over) = tmp_pkg("sz", "pkg-oversize");
        let (_, tiny) = tmp_pkg("sz", "pkg-tiny");
        let mut card = v2_card("over.sz", "Over");
        card["size"] = json!({"w": 9, "h": 9, "x": 1, "y": 2});
        write_ui(&over, &card.to_string());
        let mut card2 = v2_card("tiny.sz", "Tiny");
        card2["size"] = json!({"w": 0, "h": 1});
        write_ui(&tiny, &card2.to_string());

        let m = build_manifest(&[over, tiny], &[]);
        assert_eq!(m.cards.len(), 2);
        assert_eq!(m.cards[0]["size"], json!({"w": 4, "h": 8}), "封顶裁剪");
        assert_eq!(m.cards[0]["declaredSize"], json!({"w": 9, "h": 9}));
        assert!(m.cards[0]["size"].get("x").is_none() && m.cards[0]["size"].get("y").is_none(),
            "坐标键绝不进清单");
        assert_eq!(m.cards[1]["size"], json!({"w": 1, "h": 1}), "下限抬到 1");
        assert!(m.cards[1].get("declaredSize").is_some());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 6：size 缺失/非法 → 按 view.kind 默认：status→2×2、list→4×4、其余→2×3。
    #[test]
    fn size_defaults_by_view_kind() {
        let (base, status) = tmp_pkg("szd", "pkg-status");
        let (_, list) = tmp_pkg("szd", "pkg-list");
        let (_, form) = tmp_pkg("szd", "pkg-form");
        for (pkg, kind, id) in [
            (&status, "status", "s.d"),
            (&list, "list", "l.d"),
            (&form, "form", "f.d"),
        ] {
            let mut card = v2_card(id, "D");
            card.as_object_mut().unwrap().remove("size");
            card["view"]["kind"] = json!(kind);
            write_ui(pkg, &card.to_string());
        }
        let m = build_manifest(&[status, list, form], &[]);
        assert_eq!(m.cards[0]["size"], json!({"w": 2, "h": 2}), "status 默认 2×2");
        assert_eq!(m.cards[1]["size"], json!({"w": 4, "h": 4}), "list 默认 4×4");
        assert_eq!(m.cards[2]["size"], json!({"w": 2, "h": 3}), "form 默认 2×3");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试 7：rev = 内容哈希——同盘两算稳定；增删卡/改 title 都变；格式 64-hex。
    #[test]
    fn rev_is_content_hash_stable_and_changes() {
        let (base, a) = tmp_pkg("rev", "pkg-rev-a");
        write_ui(&a, &v2_card("rev.a", "Rev A").to_string());

        let m1 = build_manifest(std::slice::from_ref(&a), &[]);
        let m2 = build_manifest(std::slice::from_ref(&a), &[]);
        assert_eq!(m1.rev, m2.rev, "同内容必同 rev（实时计算不含进程态）");
        assert_eq!(m1.rev.len(), 64, "sha256 hex 全长");
        assert!(m1.rev.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));

        // 改 title → rev 变
        write_ui(&a, &v2_card("rev.a", "Rev A2").to_string());
        let m3 = build_manifest(std::slice::from_ref(&a), &[]);
        assert_ne!(m1.rev, m3.rev, "title 变化必须反映到 rev");

        // 加卡 → rev 变
        let (_, b) = tmp_pkg("rev", "pkg-rev-b");
        write_ui(&b, &v2_card("rev.b", "Rev B").to_string());
        let m4 = build_manifest(&[a.clone(), b.clone()], &[]);
        assert_ne!(m3.rev, m4.rev, "加卡必须变 rev");

        // 删卡（空清单也有稳定 rev）→ 变且稳定
        let m5 = build_manifest(&[], &[]);
        assert_ne!(m4.rev, m5.rev);
        let m5b = build_manifest(&[], &[]);
        assert_eq!(m5.rev, m5b.rev, "空清单 rev 也确定");
        // error 条目计入 rev：坏→修好必须变
        let (base2, bad) = tmp_pkg("rev2", "pkg-rev-bad");
        write_ui(&bad, "not json{");
        let r_bad = build_manifest(std::slice::from_ref(&bad), &[]).rev;
        write_ui(&bad, &v2_card("rev.bad", "Fixed").to_string());
        let r_fixed = build_manifest(&[bad], &[]).rev;
        assert_ne!(r_bad, r_fixed, "坏声明修好 = 清单内容变化");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&base2);
    }

    /// 测试 8：disabled 交叉——同名 entry **全部**禁用才排除；任一 enabled 出卡；
    /// 无同名 entry（试点现状）出卡；group 条目不参与匹配。
    #[test]
    fn disabled_entry_excludes_card() {
        let (base, a) = tmp_pkg("dis", "pkg-dis");
        write_ui(&a, &v2_card("dis.card", "Dis").to_string());

        // 全禁用 → 排除
        let m = build_manifest(std::slice::from_ref(&a), &[snapshot("pkg-dis", true)]);
        assert!(m.cards.is_empty(), "同名 entry 全禁用必须排除");
        // 任一 enabled → 出卡
        let m = build_manifest(
            std::slice::from_ref(&a),
            &[snapshot("pkg-dis", true), snapshot("pkg-dis", false)],
        );
        assert_eq!(m.cards.len(), 1, "任一 enabled 必须出卡");
        // 无同名 entry → 出卡（试点未 entry 化现状）
        let m = build_manifest(std::slice::from_ref(&a), &[snapshot("other-pkg", true)]);
        assert_eq!(m.cards.len(), 1);
        // group 条目不参与匹配（group 禁用 ≠ 插件禁用）
        let m = build_manifest(std::slice::from_ref(&a), &[group_snapshot("pkg-dis", true)]);
        assert_eq!(m.cards.len(), 1, "group 条目不参与名字匹配");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S6（D-185）：发现挂载 → 清单联动——真实 wasm_base 经 `scan_remote_units` 发现
    /// 装配单元后，清单出「插件清单」卡（type runtime）与试点卡；宿主清单层零改动。
    #[test]
    fn scan_mounted_units_appear_in_manifest() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins");
        let units = crate::web::scan_remote_units(&base);
        let m = build_manifest(&units, &[]);
        let inv = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-plugin-inventory")
            .unwrap_or_else(|| panic!("插件清单卡应在清单里，得 {:?}", m.cards));
        assert_eq!(inv["type"], "runtime");
        assert_eq!(inv["cardId"], "panel-plugin-inventory.list");
        assert_eq!(inv["size"], json!({"w": 4, "h": 4}));
        assert_eq!(inv["declPath"], "/plugins/panel-plugin-inventory/ui.json");
        assert!(m.cards.iter().any(|c| c["pluginName"] == "llm-deepseek"), "试点卡同在");
        // 面板改写 #2（D-187）：运行时状态卡同样自动上桌布（宿主清单层零改动）。
        let st = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-runtime-status")
            .unwrap_or_else(|| panic!("运行时状态卡应在清单里，得 {:?}", m.cards));
        assert_eq!(st["type"], "runtime");
        assert_eq!(st["cardId"], "panel-runtime-status.status");
        assert_eq!(st["size"], json!({"w": 2, "h": 2}));
        // 面板改写 #3（D-188）：动态插件清单卡（第四卡，同样零宿主改动）。
        let dyn_card = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-dynamic-plugins")
            .unwrap_or_else(|| panic!("动态插件卡应在清单里，得 {:?}", m.cards));
        assert_eq!(dyn_card["type"], "runtime");
        assert_eq!(dyn_card["cardId"], "panel-dynamic-plugins.list");
        // 面板改写 #4（D-190）：工作区文件卡（第五卡，resource 分类首卡）。
        let ws = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-workspace-files")
            .unwrap_or_else(|| panic!("工作区文件卡应在清单里，得 {:?}", m.cards));
        assert_eq!(ws["type"], "resource");
        assert_eq!(ws["cardId"], "panel-workspace-files.list");
        // 面板改写 #5（D-191）：会话清单卡（第六卡，session 分类首卡）。
        let sess = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-sessions")
            .unwrap_or_else(|| panic!("会话清单卡应在清单里，得 {:?}", m.cards));
        assert_eq!(sess["type"], "session");
        assert_eq!(sess["cardId"], "panel-sessions.list");
        // 面板改写 #6（D-192）：设置概览卡（第七卡，config 分类首卡）。
        let cfg_card = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-settings")
            .unwrap_or_else(|| panic!("设置概览卡应在清单里，得 {:?}", m.cards));
        assert_eq!(cfg_card["type"], "config");
        assert_eq!(cfg_card["cardId"], "panel-settings.list");
        // C8-4（D-193）：聊天声明单元（第八卡——声明归单元、数据面在宿主原生臂）。
        let chat = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-chat")
            .unwrap_or_else(|| panic!("聊天卡应在清单里，得 {:?}", m.cards));
        assert_eq!(chat["type"], "session");
        assert_eq!(chat["cardId"], "panel-chat.chat");
        // S4（D-194）：设置编辑声明单元（第九卡，动态 fields 投影）。
        let se = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-settings-edit")
            .unwrap_or_else(|| panic!("设置编辑卡应在清单里，得 {:?}", m.cards));
        assert_eq!(se["type"], "config");
        assert_eq!(se["cardId"], "panel-settings-edit.edit");
        // D-195：调度清单声明单元（第十卡；E2E 清单最大缺口划账）。
        let sch = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-schedule")
            .unwrap_or_else(|| panic!("调度卡应在清单里，得 {:?}", m.cards));
        assert_eq!(sch["type"], "runtime");
        assert_eq!(sch["cardId"], "panel-schedule.list");
        // D-197：调度创建表单卡（第十一卡，写端闭环）。
        let sc = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-schedule-create")
            .unwrap_or_else(|| panic!("创建调度卡应在清单里，得 {:?}", m.cards));
        assert_eq!(sc["type"], "runtime");
        assert_eq!(sc["cardId"], "panel-schedule-create.form");
        // D-199：待审批卡（第十二卡——技术队列清零）。
        let ap = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-approval")
            .unwrap_or_else(|| panic!("审批卡应在清单里，得 {:?}", m.cards));
        assert_eq!(ap["type"], "session");
        assert_eq!(ap["cardId"], "panel-approval.pending");
        // D-200：locale 设置编辑卡（第十三卡，多 ns 机械复制首卡）。
        let le = m
            .cards
            .iter()
            .find(|c| c["pluginName"] == "panel-locale-edit")
            .unwrap_or_else(|| panic!("locale 编辑卡应在清单里，得 {:?}", m.cards));
        assert_eq!(le["type"], "config");
        assert_eq!(le["cardId"], "panel-locale-edit.edit");
    }
}

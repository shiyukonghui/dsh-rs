//! D-213 分组桌板：板级关闭集纯逻辑（板→列表 map；一切状态语义在这一层可证）。

use serde_json::{json, Map, Value};

/// 总览板 id（selected=None 时的板名）。
pub const BOARD_ALL: &str = "all";

/// selected → 当前板 id。
pub fn board_of(selected: Option<&String>) -> &str {
    selected.map(|s| s.as_str()).unwrap_or(BOARD_ALL)
}

/// 取某板的关闭列表（缺板=空）。
pub fn closed_for(map: &Map<String, Value>, board: &str) -> Vec<String> {
    map.get(board)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// 在指定板关闭一张卡（已关则幂等）。
pub fn close_on(map: &mut Map<String, Value>, board: &str, key: &str) {
    let mut list = closed_for(map, board);
    if !list.iter().any(|k| k == key) {
        list.push(key.to_string());
    }
    map.insert(board.to_string(), Value::Array(list.into_iter().map(Value::String).collect()));
}

/// 在指定板重开一张卡（只动该板）。
pub fn open_on(map: &mut Map<String, Value>, board: &str, key: &str) {
    let mut list = closed_for(map, board);
    let before = list.len();
    list.retain(|k| k != key);
    if list.is_empty() {
        map.remove(board);
    } else if list.len() != before {
        map.insert(board.to_string(), Value::Array(list.into_iter().map(Value::String).collect()));
    }
}

/// 旧全局键（数组）→ v2 板级 map（归「全部」板）。
pub fn migrate_legacy(old: &[String]) -> Map<String, Value> {
    let mut m = Map::new();
    if !old.is_empty() {
        m.insert(BOARD_ALL.to_string(), Value::Array(old.iter().cloned().map(Value::String).collect()));
    }
    m
}

// ---------- D-214 摆位（板→卡→{x,y}；钉住的卡压过自动排布） ----------

/// 写一张卡的钉位（浮点取整在写入方做）。
pub fn set_pin(map: &mut Map<String, Value>, board: &str, key: &str, x: f64, y: f64) {
    let mut obj = map.get(board).and_then(Value::as_object).cloned().unwrap_or_default();
    obj.insert(key.to_string(), json!({ "x": x, "y": y }));
    map.insert(board.to_string(), Value::Object(obj));
}

/// 某板全部钉位 (key,x,y)。
pub fn pins_of(map: &Map<String, Value>, board: &str) -> Vec<(String, f64, f64)> {
    map.get(board)
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let x = v.get("x")?.as_f64()?;
                    let y = v.get("y")?.as_f64()?;
                    Some((k.clone(), x, y))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 本板是否有钉位（控制「重置摆位」入口显隐）。
pub fn has_pins(map: &Map<String, Value>, board: &str) -> bool {
    map.get(board)
        .and_then(Value::as_object)
        .map(|o| !o.is_empty())
        .unwrap_or(false)
}

/// 重置本板摆位；返回是否确有清除（无钉位时不动 map）。
pub fn reset_board(map: &mut Map<String, Value>, board: &str) -> bool {
    map.remove(board).is_some()
}

/// 合并自动排布与钉位：钉卡用钉位（压过自动位/声明位），非钉卡维持自动位；
/// 总高 = max(自动总高, 各钉卡 y+实测高)。缺实测高的钉卡按 200px 兜底。
pub fn merge_pinned(
    auto: Vec<(String, i64, i64)>,
    auto_total: i64,
    pins: &[(String, f64, f64)],
    heights: &std::collections::HashMap<String, i64>,
) -> (Vec<(String, f64, f64)>, i64) {
    let pinned: std::collections::HashSet<&str> = pins.iter().map(|(k, _, _)| k.as_str()).collect();
    let mut out: Vec<(String, f64, f64)> = auto
        .into_iter()
        .filter(|(k, _, _)| !pinned.contains(k.as_str()))
        .map(|(k, cx, y)| (k, cx as f64, y as f64))
        .collect();
    let mut total = auto_total;
    for (k, x, y) in pins {
        let h = *heights.get(k).unwrap_or(&200);
        total = total.max(*y as i64 + h);
        out.push((k.clone(), *x, *y));
    }
    (out, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn board_of_none_is_all_board() {
        assert_eq!(board_of(None), BOARD_ALL);
        let s = "model".to_string();
        assert_eq!(board_of(Some(&s)), "model");
    }

    #[test]
    fn boards_are_isolated() {
        let mut m = Map::new();
        close_on(&mut m, "config", "panel-settings.list");
        // config 板关闭：全部板与 model 板都不受牵连
        assert_eq!(closed_for(&m, "config"), vec!["panel-settings.list".to_string()]);
        assert!(closed_for(&m, "all").is_empty());
        assert!(closed_for(&m, "model").is_empty());
        // 另一板关同名的卡互不干涉
        close_on(&mut m, "model", "llm-deepseek.form");
        assert_eq!(closed_for(&m, "config"), vec!["panel-settings.list".to_string()]);
        // 幂等：重复关闭不重复入列
        close_on(&mut m, "config", "panel-settings.list");
        assert_eq!(closed_for(&m, "config").len(), 1);
    }

    #[test]
    fn open_touches_only_its_board() {
        let mut m = Map::new();
        close_on(&mut m, "all", "panel-chat.chat");
        close_on(&mut m, "config", "panel-chat.chat");
        open_on(&mut m, "all", "panel-chat.chat");
        assert!(closed_for(&m, "all").is_empty());
        assert_eq!(closed_for(&m, "config"), vec!["panel-chat.chat".to_string()]);
        // 空列表的板键被清掉（存储不留垃圾）
        assert!(!m.contains_key("all"));
    }

    #[test]
    fn legacy_array_migrates_to_all_board() {
        let m = migrate_legacy(&["panel-chat.chat".to_string(), "panel-sessions.list".to_string()]);
        assert_eq!(
            m.get("all").cloned().unwrap_or(Value::Null),
            json!(["panel-chat.chat", "panel-sessions.list"])
        );
        // 空旧键 → 空 map（不落无意义键）
        assert!(migrate_legacy(&[]).is_empty());
    }

    // ---------- D-214 摆位测试 ----------

    #[test]
    fn pins_are_per_board_and_resettable() {
        let mut m = Map::new();
        assert!(!has_pins(&m, "all"));
        set_pin(&mut m, "all", "panel-chat.chat", 320.0, 140.0);
        set_pin(&mut m, "model", "llm-deepseek.form", 80.0, 60.0);
        assert!(has_pins(&m, "all"));
        // 板间零牵连
        let all = pins_of(&m, "all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], ("panel-chat.chat".to_string(), 320.0, 140.0));
        assert_eq!(pins_of(&m, "model").len(), 1);
        // 同板重钉=覆盖
        set_pin(&mut m, "all", "panel-chat.chat", 10.0, 20.0);
        assert_eq!(pins_of(&m, "all")[0].1, 10.0);
        // 重置只清本板
        assert!(reset_board(&mut m, "all"));
        assert!(!has_pins(&m, "all"));
        assert!(has_pins(&m, "model"));
        assert!(!reset_board(&mut m, "all")); // 无钉位=false 且不报错
    }

    #[test]
    fn merge_pinned_overrides_auto_keeps_others() {
        let auto = vec![
            ("a".to_string(), 0_i64, 0_i64),
            ("b".to_string(), 300_i64, 0_i64),
            ("c".to_string(), 0_i64, 400_i64),
        ];
        let pins = vec![("b".to_string(), 900.0, 55.0)];
        let mut heights = std::collections::HashMap::new();
        heights.insert("b".to_string(), 220_i64);
        let (out, total) = merge_pinned(auto, 800, &pins, &heights);
        // 钉卡用钉位；非钉卡原样
        let b = out.iter().find(|(k, _, _)| k == "b").unwrap();
        assert_eq!((b.1, b.2), (900.0, 55.0));
        let c = out.iter().find(|(k, _, _)| k == "c").unwrap();
        assert_eq!((c.1, c.2), (0.0, 400.0));
        assert_eq!(out.len(), 3);
        // 总高 = max(自动 800, 钉卡底 55+220=275) = 800
        assert_eq!(total, 800);
    }

    #[test]
    fn merge_total_grows_for_low_pinned_card() {
        let pins = vec![("deep".to_string(), 50.0, 2000.0)];
        let mut heights = std::collections::HashMap::new();
        heights.insert("deep".to_string(), 300_i64);
        let (_out, total) = merge_pinned(vec![("x".to_string(), 0, 0)], 500, &pins, &heights);
        assert_eq!(total, 2300);
        // 实测高参与底边计算（非兜底路径）
        let (_o2, t2) = merge_pinned(vec![], 100, &pins, &heights);
        assert_eq!(t2, 2300);
        // 缺实测高按 200 兜底（工作台不留底边窟窿）
        let (_o3, t3) = merge_pinned(vec![], 100, &[("ghost".to_string(), 0.0, 900.0)], &heights);
        assert_eq!(t3, 1100);
    }
}

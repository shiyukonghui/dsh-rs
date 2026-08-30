//! D-213 分组桌板：板级关闭集纯逻辑（板→列表 map；一切状态语义在这一层可证）。

use serde_json::{Map, Value};

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
}

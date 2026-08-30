//! i18n（locale-toggle D-225）：shell chrome 字典 + 声明文案解析器 ltext。
//! 契约（.spec/locale-toggle/design.md）：文案位 = `"字符串" | {"zh":..,"en":..}`，
//! 解析序 lang → zh → en → 首个字符串值。

use serde_json::Value;

/// chrome 文案键（zh/en 双版；`{}` 占位由调用方 format!）。
pub fn t(lang: &str, key: &str) -> &'static str {
    let en = lang == "en";
    match key {
        "app_title" => if en { "Service Assembly Canvas" } else { "服务装配单元 · 桌布" },
        "manifest_ok" => if en { "manifest" } else { "清单" }, // 渲染拼 `✓ {t} N 卡`
        "cards" => if en { "cards" } else { "卡" },
        "manifest_loading" => if en { "Loading manifest…" } else { "载入清单…" },
        "manifest_err" => if en { "manifest fetch failed" } else { "清单拉取失败" },
        "board_all" => if en { "All" } else { "全部" },
        "board_empty" => if en { "No cards on this board." } else { "此板没有卡" },
        "all_closed" => if en { "All cards in this group are closed — click the dimmed title to reopen." } else { "本组卡片已全部关闭——点左侧灰显标题可重新打开。" },
        "load_body" => if en { "Loading…" } else { "载入体面…" },
        "load_values" => if en { "Loading current values…" } else { "载入当前值…" },
        "load_sessions" => if en { "Loading sessions…" } else { "载入会话列表…" },
        "no_sessions" => if en { "No sessions available" } else { "没有可选会话" },
        "saved" => if en { "✓ Saved" } else { "✓ 已保存" },
        "saved_restart" => if en { "✓ Saved — restart to apply" } else { "✓ 已保存——需重启生效" },
        "save_failed" => if en { "✗ Save failed: " } else { "✗ 保存失败：" },
        "field_err" => if en { "✗ Field " } else { "✗ 字段 " }, // 拼 `字段 X：err`
        "field_err2" => if en { ": " } else { "：" },
        "injected" => if en { "✓ Found · injected into " } else { "✓ 发现 " },
        "injected_items" => if en { " items (unsaved)" } else { " 项 · 已注入 " },
        "injected_tail" => if en { "" } else { "（未保存）" },
        "confirm_act" => if en { "Confirm \u{201c}" } else { "确认「" },
        "confirm_act2" => if en { "\u{201d}?" } else { "」？" },
        "chat_ph" => if en { "Send a message…" } else { "发消息…" },
        "send" => if en { "Send" } else { "发送" },
        "stop" => if en { "Stop" } else { "停止" },
        "sent" => if en { "✓ Sent" } else { "✓ 已发送" },
        "send_failed" => if en { "✗ Send: " } else { "✗ 发送：" },
        "cancel" => if en { "→ Cancel " } else { "→ 取消 " },
        "cancel2" => if en { " …" } else { " …" },
        "cancel_ok" => if en { "✓ Cancel requested" } else { "✓ 已请求取消" },
        "cancel_failed" => if en { "✗ Cancel: " } else { "✗ 取消：" },
        "no_session" => if en { "✗ No session selected" } else { "✗ 当前无会话" },
        "reset_pos" => if en { "⟲ Reset layout" } else { "⟲ 重置摆位" },
        "busy" => if en { "·busy" } else { "·忙" },
        "idle" => if en { "·idle" } else { "·闲" },
        "decl_broken" => if en { "declaration broken" } else { "声明损坏" },
        "no_items" => if en { "No items" } else { "暂无条目" },
        _ => if en { "?" } else { "？" },
    }
}

/// 声明文案解析（LocalizedText 契约）。字符串直返；对象按 lang→zh→en→首值。
pub fn ltext(v: Option<&Value>, lang: &str) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(m)) => {
            for k in [lang, "zh", "en"] {
                if let Some(Value::String(s)) = m.get(k) {
                    return s.clone();
                }
            }
            m.values().find_map(Value::as_str).map(str::to_string).unwrap_or_default()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ltext_plain_string_passthrough() {
        assert_eq!(ltext(Some(&json!("待审批")), "en"), "待审批");
    }

    #[test]
    fn ltext_bilingual_picks_lang_then_fallback_chain() {
        let v = json!({"zh": "待审批", "en": "Pending Approvals"});
        assert_eq!(ltext(Some(&v), "en"), "Pending Approvals");
        assert_eq!(ltext(Some(&v), "zh"), "待审批");
        assert_eq!(ltext(Some(&v), "fr"), "待审批", "未知语言回退 zh");
        let only_en = json!({"en": "Only EN"});
        assert_eq!(ltext(Some(&only_en), "zh"), "Only EN", "缺 zh 回退 en");
    }

    #[test]
    fn ltext_absent_is_empty() {
        assert_eq!(ltext(None, "zh"), "");
        assert_eq!(ltext(Some(&serde_json::Value::Null), "zh"), "");
    }

    #[test]
    fn dict_both_languages_nonempty() {
        for k in ["app_title", "board_all", "saved", "send", "reset_pos"] {
            assert!(!t("zh", k).is_empty() && !t("en", k).is_empty());
            assert_ne!(t("zh", k), t("en", k), "{k} 双语必须有差异");
        }
    }
}

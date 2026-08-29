//! D-184：桌布 C2/C3 的宿主入口——`/canvas` 独立视图（不依赖 harness dist、不碰 SPA）。
//!
//! 壳资产编译进二进制（`include_str!`）：`/canvas` 是唯一入口，`/canvas/assets/*` 是
//! 闭环集；未识别的 `/canvas/*` **必须 404**（绝不回落 SPA——防「桌布失踪变前端」）。
//! 核心逻辑可测性在 JS 侧（`assets/canvas/tests/`，node --test）；此处只证路由分发。

/// 壳页面（模块引用齐；`type="module"` 是渲染器在浏览器且零 eval 的形态锚）。
pub const CANVAS_HTML: &str = include_str!("../assets/canvas/index.html");
/// 样式 + 网格几何 CSS 变量（格距 10px 契约值）。
pub const CANVAS_CSS: &str = include_str!("../assets/canvas/canvas.css");
/// 纯逻辑核心（零 DOM/零 fetch/零 eval）。
pub const CANVAS_CORE_JS: &str = include_str!("../assets/canvas/core.js");
/// DOM/fetch/定时器粘合层（薄）。
pub const CANVAS_APP_JS: &str = include_str!("../assets/canvas/app.js");

/// `/canvas` 路由纯函数：命中 → `(status, content_type, body)`；miss → `None`（调用方
/// 必须回 404，不得落到 SPA fallback）。
pub fn canvas_response(path: &str) -> Option<(u16, &'static str, &'static [u8])> {
    let (ct, body) = match path {
        "/canvas" | "/canvas/" => ("text/html; charset=utf-8", CANVAS_HTML.as_bytes()),
        "/canvas/assets/canvas.css" => ("text/css; charset=utf-8", CANVAS_CSS.as_bytes()),
        "/canvas/assets/core.js" => ("text/javascript; charset=utf-8", CANVAS_CORE_JS.as_bytes()),
        "/canvas/assets/app.js" => ("text/javascript; charset=utf-8", CANVAS_APP_JS.as_bytes()),
        _ => return None,
    };
    Some((200, ct, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 壳入口：200 html + 资产引用齐（css/module 脚本）——缺一个就是白屏现场。
    #[test]
    fn canvas_shell_served_with_asset_refs() {
        let (status, ct, body) = canvas_response("/canvas").expect("/canvas served");
        assert_eq!(status, 200);
        assert_eq!(ct, "text/html; charset=utf-8");
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("/canvas/assets/canvas.css"), "css 引用");
        assert!(
            html.contains("type=\"module\" src=\"/canvas/assets/app.js\""),
            "module 脚本引用"
        );
        // 独立视图：不引用 harness bundle
        assert!(!html.contains("__DSH_BOOT__"), "桌布零依赖 harness boot 注入");
        assert!(canvas_response("/canvas/").is_some(), "尾斜杠同页");
    }

    /// 资产闭环集：mime 正确（模块脚本必须 text/javascript，浏览器才吃 ESM）。
    #[test]
    fn canvas_assets_served_with_mimes() {
        for (path, ct) in [
            ("/canvas/assets/canvas.css", "text/css; charset=utf-8"),
            ("/canvas/assets/core.js", "text/javascript; charset=utf-8"),
            ("/canvas/assets/app.js", "text/javascript; charset=utf-8"),
        ] {
            let (status, got, body) = canvas_response(path).unwrap_or_else(|| panic!("{path} served"));
            assert_eq!((status, got), (200, ct), "{path}");
            assert!(!body.is_empty(), "{path} 非空");
        }
        // 核心零 eval 护栏（不变式 D-179 的资产面哨兵——壳自己也不得引入执行面）
        assert!(!CANVAS_CORE_JS.contains("eval("), "core.js 零 eval");
        assert!(!CANVAS_APP_JS.contains("eval("), "app.js 零 eval");
        // core 必须真导出契约函数（app 引用的名字一个不能少）
        for name in [
            "buildModel",
            "layoutGrid",
            "validateDeclaration",
            "columnsForWidth",
            "collectValues",
            "rpcEnvelope",
            "pollDecision",
            "focusKey",
            "extractPath",
            "listRows",
            "statusItems",
            "rowActionBody",
            "needsConfirm",
            "chatFoldFrame",
            "chatOptions",
            "schemaFields",
            "nsSelectModel",
        ] {
            assert!(CANVAS_CORE_JS.contains(&format!("export function {name}")), "core 导出 {name}");
        }
    }

    /// miss → None（调用方 404；**绝不回落 SPA**——D-184）。
    #[test]
    fn canvas_unknown_paths_are_none() {
        for p in ["/canvas/nope.js", "/canvas/assets", "/canvas/assets/evil.wasm", "/canvasx"] {
            assert!(canvas_response(p).is_none(), "{p} 必须 miss");
        }
    }
}

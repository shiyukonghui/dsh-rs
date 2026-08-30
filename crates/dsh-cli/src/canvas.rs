//! D-184：桌布 C2/C3 的宿主入口——`/canvas` 独立视图（不依赖 harness dist、不碰 SPA）。
//!
//! 壳资产编译进二进制（`include_str!`）；未识别的 `/canvas/*` **必须 404**（绝不回落
//! 任何前端）。S6d 后 `/canvas` 默认 = Rust 壳；D-212 后根 `/` 亦由本壳直服（旧
//! deepseek 前端下线）。JS 渲染器已按拍板退役：`index/core/app/tests` 存档于
//! `.spec/archive/canvas-js/`，仅 canvas.css 幸存（Rust 壳在用）。

/// 样式 + 网格几何 CSS 变量（格距 10px 契约值；JS 壳退役后唯一幸存资产）。
pub const CANVAS_CSS: &str = include_str!("../assets/canvas/canvas.css");

// D-210 S5：Rust 壳发布产物内嵌表（build.rs 扫描 assets/canvas-shell/ 生成）。
include!(concat!(env!("OUT_DIR"), "/shell_assets.rs"));

fn shell_mime(rel: &str) -> &'static str {
    if rel.ends_with(".wasm") {
        "application/wasm"
    } else if rel.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if rel.ends_with(".html") {
        "text/html; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

/// Rust 壳入口 html（内嵌件）——`/canvas`、`/canvas/rust` 与根下线后的 `/` 共用。
pub fn canvas_shell_entry() -> Option<(u16, &'static str, &'static [u8])> {
    let body = SHELL_ASSETS
        .iter()
        .find(|(p, _, _)| *p == "rust.html")
        .map(|(_, b, _)| *b)?;
    Some((200, "text/html; charset=utf-8", body))
}

/// `/canvas` 路由纯函数：命中 → `(status, content_type, body)`；miss → `None`（调用方
/// 必须回 404，不得落到任何前端 fallback）。
pub fn canvas_response(path: &str) -> Option<(u16, &'static str, &'static [u8])> {
    // D-210 S5→S6d：`/canvas` 与 `/canvas/rust` 同归 Rust 壳。
    if path == "/canvas" || path == "/canvas/" || path == "/canvas/rust" || path == "/canvas/rust/" {
        return canvas_shell_entry();
    }
    if let Some(rel) = path.strip_prefix("/canvas/rust/assets/") {
        let rel = rel.trim_start_matches('/');
        let (_, body, _) = SHELL_ASSETS.iter().find(|(p, _, _)| *p == rel)?;
        return Some((200, shell_mime(rel), body));
    }
    // D-212：JS 渲染器退役——legacy/core/app 路由已删（revert 本提交 + 存档回迁即回滚）。
    let (ct, body) = match path {
        "/canvas/assets/canvas.css" => ("text/css; charset=utf-8", CANVAS_CSS.as_bytes()),
        _ => return None,
    };
    Some((200, ct, body))
}

/// S5 尾项：带内容协商的响应——第四值 = Content-Encoding。`gz_ok` 且该件有构建期
/// 预压缩产物 → gzip 字节；否则原样。纯函数可测；miss 仍 None（404 铁律不变）。
pub fn canvas_response_enc(path: &str, gz_ok: bool) -> Option<(u16, &'static str, &'static [u8], Option<&'static str>)> {
    let (status, ct, body) = canvas_response(path)?;
    if gz_ok {
        if let Some(rel) = path.strip_prefix("/canvas/rust/assets/") {
            let rel = rel.trim_start_matches('/');
            if let Some((_, _, gz)) = SHELL_ASSETS.iter().find(|(p, _, _)| *p == rel) {
                if !gz.is_empty() {
                    return Some((status, ct, *gz, Some("gzip")));
                }
            }
        }
    }
    Some((status, ct, body, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-210 S6d：默认 = Rust 壳（四入口齐）+ 共享 css 路由不破。
    #[test]
    fn s6d_canvas_default_serves_rust_shell() {
        for p in ["/canvas", "/canvas/", "/canvas/rust", "/canvas/rust/"] {
            let (status, ct, body) = canvas_response(p).unwrap_or_else(|| panic!("{p} served"));
            assert_eq!((status, ct), (200, "text/html; charset=utf-8"), "{p}");
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                html.contains("import init from '/canvas/rust/assets/canvas-shell.js'"),
                "{p} 默认必须是 Rust 壳入口"
            );
        }
        // 共享 css 路由不破（rust.html 引用它）。
        let (_s, ct, _b) = canvas_response("/canvas/assets/canvas.css").expect("css still served");
        assert_eq!(ct, "text/css; charset=utf-8");
    }

    /// D-212 根下线：canvas_shell_entry 提供与 /canvas 同源的入口件（web.rs 根路由用）。
    #[test]
    fn root_entry_serves_canvas_shell() {
        let (status, ct, body) = canvas_shell_entry().expect("shell entry");
        assert_eq!((status, ct), (200, "text/html; charset=utf-8"));
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("import init from '/canvas/rust/assets/canvas-shell.js'"));
        assert!(!html.contains("__DSH_BOOT__"), "零 harness boot 注入");
    }

    /// D-212 JS 渲染器退役：legacy 与 core/app 路由已灭（miss→404 铁律接管）。
    #[test]
    fn js_shell_routes_are_retired() {
        for p in [
            "/canvas/legacy",
            "/canvas/legacy/",
            "/canvas/assets/core.js",
            "/canvas/assets/app.js",
        ] {
            assert!(canvas_response(p).is_none(), "{p} 必须 miss（已退役）");
        }
    }

    /// miss → None（调用方 404；**绝不回落任何前端**——D-184）。
    #[test]
    fn canvas_unknown_paths_are_none() {
        for p in [
            "/canvas/nope.js",
            "/canvas/assets",
            "/canvas/assets/evil.wasm",
            "/canvasx",
            "/canvas/rust/nope.js",
            "/canvas/rust/assets/../../etc/passwd",
        ] {
            assert!(canvas_response(p).is_none(), "{p} 必须 miss");
        }
    }

    /// D-210 S5：Rust 壳内嵌面——入口引用绝对路径、胶水/wasm mime 正确、表非空。
    #[test]
    fn rust_shell_embedded_served() {
        assert!(!SHELL_ASSETS.is_empty(), "build.rs 表非空");
        let (status, ct, body) = canvas_response("/canvas/rust").expect("/canvas/rust served");
        assert_eq!((status, ct), (200, "text/html; charset=utf-8"));
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("import init from '/canvas/rust/assets/canvas-shell.js'"),
            "胶水引用绝对路径"
        );
        let (st2, ct2, b2) = canvas_response("/canvas/rust/assets/canvas-shell.js").unwrap();
        assert_eq!((st2, ct2), (200, "text/javascript; charset=utf-8"));
        assert!(!b2.is_empty());
        let (_s, ct3, b3) = canvas_response("/canvas/rust/assets/canvas-shell_bg.wasm").unwrap();
        assert_eq!(ct3, "application/wasm");
        assert!(b3.starts_with(b"\0asm"), "wasm 魔数");
        // snippets 相对导入面：表里有的必须可按 assets 前缀命中（mime=js）。
        let snip = SHELL_ASSETS.iter().find(|(p, _, _)| p.starts_with("snippets/")).expect("snippets 存在");
        let hit = canvas_response(&format!("/canvas/rust/assets/{}", snip.0)).expect("snippet 命中");
        assert_eq!(hit.1, "text/javascript; charset=utf-8");
    }

    /// S5 尾项：gzip 内容协商——accept 方得预压缩件（magic+头），拒绝方得原样；
    /// 未知路径在协商面同样 miss（404 铁律不破）。
    #[test]
    fn rust_shell_gzip_negotiation() {
        let p = "/canvas/rust/assets/canvas-shell_bg.wasm";
        let (_s, _ct, body, enc) = canvas_response_enc(p, true).expect("gz served");
        assert_eq!(enc, Some("gzip"));
        assert!(body.starts_with(&[0x1f, 0x8b]), "gzip magic");
        let (_s2, _ct2, raw, enc2) = canvas_response_enc(p, false).expect("raw served");
        assert_eq!(enc2, None);
        assert!(raw.starts_with(b"\0asm"), "raw 仍是 wasm");
        assert!(raw.len() > body.len(), "gzip 必须真瘦身");
        assert!(canvas_response_enc("/canvas/rust/nope.js", true).is_none());
        // css 不在 gzip 表面，恒原样。
        let (_s3, _ct3, _b3, enc3) = canvas_response_enc("/canvas/assets/canvas.css", true).expect("css served");
        assert_eq!(enc3, None);
    }
}

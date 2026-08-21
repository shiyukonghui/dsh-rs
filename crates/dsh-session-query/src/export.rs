//! session-log-export：前端日志导出形状（M1d）。
//!
//! 权威参考：`deepseek-harness/packages/host/apiproxy/src/types/session-export.js`。
//! 本模块是**输出形状描述**——不是 ZIP 编码器（本 crate 无 `zip` 依赖）：
//! 定义导出的确定性 ZIP 布局（`session.jsonl` 根 + `subagents/{segment}/session.jsonl` +
//! `media/{attachmentId}.{ext}`），把内容以 `(path, bytes)` 条目描述，由上层编码 ZIP。
//!
//! 布局约定：
//! 1. 首项恒为根会话 `session.jsonl`（`read_raw` 文件名恒为 `session.jsonl`，与物理
//!    压缩无关——`SessionRawArtifact.content` 已是解码后 JSONL 文本）；
//! 2. `include_descendants` 时按序追加 `subagents/{segment}/session.jsonl`；
//!    artifact 缺失的 descendant 是错误；
//! 3. 最后按给定顺序追加 `media/{attachmentId}.{ext}`（image → ext 映射见
//!    [`image_mime_ext`]；条目由调用方预整理）。

use dsh_brand::SessionId;
use dsh_persistence::SessionRawArtifact;

/// 导出 ZIP 中的一条文件项：ZIP 布局内路径 + 原始字节内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogExportEntry {
    /// ZIP 内的路径（正向斜杠布局，如 `session.jsonl`、`subagents/x/session.jsonl`）。
    pub path: String,
    /// 该路径的字节内容。
    pub content: Vec<u8>,
}

/// 会话日志导出的完整构建结果：条目按确定性顺序排列（根 → 子代理 → 媒体）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogExportBuild {
    pub entries: Vec<LogExportEntry>,
}

/// 把会话 id 清洗为 ZIP 内的安全路径段：`[A-Za-z0-9_-]` 之外的每个字符替换为 `_`
/// （对齐 TS `safeSessionIdSegment`）。
pub fn session_id_to_segment(id: &SessionId) -> String {
    id.raw()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 构建一次日志导出的条目序列。
///
/// - `artifact`：根会话的原始 artifact（恒为首项 `session.jsonl`）；
/// - `descendants`：后代会话（id, artifact) 有序列表；`include_descendants` 时按序追加；
/// - `images`：`(attachment_id, ext, bytes)` 已整理好的媒体条目，按给定顺序追加。
pub fn build_session_export(
    artifact: &SessionRawArtifact,
    descendants: &[(SessionId, Option<SessionRawArtifact>)],
    images: &[(String, String, Vec<u8>)],
    include_descendants: bool,
) -> Result<LogExportBuild, String> {
    let mut entries = Vec::new();
    // 1. 根会话日志恒为首项：read_raw 文件名恒为 session.jsonl（与物理压缩无关）
    entries.push(LogExportEntry {
        path: "session.jsonl".to_string(),
        content: artifact.content.as_bytes().to_vec(),
    });
    // 2. 子代理日志（按传入顺序）；artifact 缺失是错误
    if include_descendants {
        for (id, descendant) in descendants {
            let descendant = descendant.as_ref().ok_or_else(|| {
                format!("subagent \"{id}\" has no stored log artifact")
            })?;
            entries.push(LogExportEntry {
                path: format!(
                    "subagents/{}/session.jsonl",
                    session_id_to_segment(id)
                ),
                content: descendant.content.as_bytes().to_vec(),
            });
        }
    }
    // 3. 媒体（按给定顺序；attachment id / ext 由调用方依 image_mime_ext 预整理）
    for (attachment_id, ext, bytes) in images {
        entries.push(LogExportEntry {
            path: format!("media/{attachment_id}.{ext}"),
            content: bytes.clone(),
        });
    }
    Ok(LogExportBuild { entries })
}

/// 导出 zip 的文件名：`dsh-session-<sanitized>.zip`。
pub fn export_filename(id: &SessionId) -> String {
    format!("dsh-session-{}.zip", session_id_to_segment(id))
}

/// image MIME → 扩展名映射（导出媒体路径的权威 ext 来源；元数据由调用方据此整理条目）。
pub fn image_mime_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_session::types::SessionHeader;

    fn header(id: &str) -> SessionHeader {
        SessionHeader::new(SessionId::from_raw(id), 1000)
    }

    fn artifact(id: &str, body: &str) -> SessionRawArtifact {
        SessionRawArtifact {
            meta: header(id),
            filename: "session.jsonl".to_string(),
            content: body.to_string(),
        }
    }

    // ---- session.jsonl 根 ----

    #[test]
    fn session_jsonl_entry_is_first_and_carries_decoded_content() {
        let build = build_session_export(&artifact("s1", "line1\nline2\n"), &[], &[], false)
            .expect("root-only export");
        assert_eq!(build.entries.len(), 1);
        assert_eq!(build.entries[0].path, "session.jsonl", "root path is exactly session.jsonl");
        assert_eq!(build.entries[0].content, b"line1\nline2\n");
    }

    // ---- 子代理 ----

    #[test]
    fn subagent_entries_follow_root_in_given_order() {
        let root = artifact("s1", "root\n");
        let a = artifact("a", "a\n");
        let b = artifact("b", "b\n");
        let descendants =
            vec![(SessionId::from_raw("a"), Some(a)), (SessionId::from_raw("b"), Some(b))];
        let build = build_session_export(&root, &descendants, &[], true).expect("with subagents");
        let paths: Vec<&str> = build.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["session.jsonl", "subagents/a/session.jsonl", "subagents/b/session.jsonl"]
        );
        assert_eq!(&build.entries[1].content, b"a\n");
        assert_eq!(&build.entries[2].content, b"b\n");
    }

    #[test]
    fn missing_descendant_artifact_is_an_error() {
        let root = artifact("s1", "root\n");
        let descendants = vec![(SessionId::from_raw("a"), None)];
        let err = build_session_export(&root, &descendants, &[], true).expect_err("must refuse");
        assert_eq!(err, "subagent \"a\" has no stored log artifact");
    }

    #[test]
    fn include_descendants_false_omits_subagent_entries() {
        let root = artifact("s1", "root\n");
        let a = artifact("a", "a\n");
        let descendants = vec![(SessionId::from_raw("a"), Some(a))];
        let build =
            build_session_export(&root, &descendants, &[], false).expect("root-only export");
        let paths: Vec<&str> = build.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["session.jsonl"]);
    }

    // ---- 清洗与文件名 ----

    #[test]
    fn session_id_segment_sanitizes_unsafe_characters() {
        assert_eq!(session_id_to_segment(&SessionId::from_raw("a/b:c")), "a_b_c");
        assert_eq!(session_id_to_segment(&SessionId::from_raw("ok_1-2")), "ok_1-2", "safe chars kept");
        assert_eq!(session_id_to_segment(&SessionId::from_raw("a b\tc")), "a_b_c");
        assert_eq!(session_id_to_segment(&SessionId::from_raw("x.y.z")), "x_y_z");
        // 按码点替换（与 TS 正则按字符替换一致）：两个汉字 → 两个占位符
        assert_eq!(session_id_to_segment(&SessionId::from_raw("中文")), "__");
    }

    #[test]
    fn export_filename_uses_sanitized_segment() {
        assert_eq!(export_filename(&SessionId::from_raw("a/b")), "dsh-session-a_b.zip");
        assert_eq!(export_filename(&SessionId::from_raw("ok-1")), "dsh-session-ok-1.zip");
    }

    // ---- 媒体 ----

    #[test]
    fn image_entries_placed_under_media_with_given_ext_and_order() {
        let root = artifact("s1", "root\n");
        let images = vec![
            ("img1".to_string(), "png".to_string(), b"PNGDATA".to_vec()),
            ("img2".to_string(), "webp".to_string(), b"WEBPDATA".to_vec()),
        ];
        let build = build_session_export(&root, &[], &images, false).expect("with images");
        let paths: Vec<&str> = build.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["session.jsonl", "media/img1.png", "media/img2.webp"]);
        assert_eq!(&build.entries[1].content, b"PNGDATA");
        assert_eq!(&build.entries[2].content, b"WEBPDATA");
    }

    #[test]
    fn image_mime_ext_map_covers_four_formats() {
        assert_eq!(image_mime_ext("image/png"), Some("png"));
        assert_eq!(image_mime_ext("image/jpeg"), Some("jpg"));
        assert_eq!(image_mime_ext("image/webp"), Some("webp"));
        assert_eq!(image_mime_ext("image/gif"), Some("gif"));
        assert_eq!(image_mime_ext("application/pdf"), None);
    }
}

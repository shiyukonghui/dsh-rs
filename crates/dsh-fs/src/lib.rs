//! dsh-fs — M5 文件系统能力缝（设计见 M5-DESIGN.md §4）。
//!
//! 阶段三 TDD 骨架：先落地 `types`（FsErrorCode/FsError/branded-target/write-intent/edit-
//! request/outcome）与 `LocalFileSystem`（resolve/readText/writeText/editText 守卫语义 +
//! 原子写）。observation policy、sandbox fence、tool-fs 随各自红测陆续加入。

mod local;
mod types;

pub use local::LocalFileSystem;
pub use types::{
    FsEditOutcome, FsEditRequest, FsError, FsErrorCode, FsReadText, FsTarget, FsTargetKey,
    FsVersion, FsWriteIntent, FsWriteOutcome, ReadTextOptions, ResolveOptions,
};

/// 进程内沙箱围栏（写路径守卫）。
pub mod sandbox;
pub use sandbox::{checked_target, SandboxPolicy};

/// observation policy（read-before-edit / version CAS 决策）。
pub mod policy;
pub use policy::{Observation, ObservationGate, OwnerId};

/// tool `read` 纯渲染面（read_render）。
pub mod read_render;

/// glob/grep 搜索（DIV-7：globset+ignore 进程内引擎，参考 tool-fs-search）。
pub mod fs_search;
pub use fs_search::{glob_search, glob_search_in, parse_glob_args, GlobInput};

/// grep 搜索（DIV-7：ignore+regex 进程内引擎，参考 tool-fs-search grep.ts）。
pub mod grep;
pub use grep::{
    format_grep_matches, format_grep_output, grep_search, grep_search_in, parse_grep_args,
    preview_line, retain_grep_matches, GrepError, GrepErrorCode, GrepInput, GrepMatch,
    RetainedMatches,
};

/// tool-fs 纯映射面（write/edit/error 补救/read_image 渲染）。
pub mod tool_fs;
pub use tool_fs::{
    format_edit_output, format_image_read_output, format_write_output, image_media_type_for_path,
    parse_edit_args, parse_write_args, remediate_fs_error, EditInput, ImageRead, WriteInput,
};

/// str_replace_editor 纯面（view 渲染 / str_replace 唯一性 / insert 插入）。
pub mod sr_editor;
pub use sr_editor::{
    apply_insert, apply_str_replace, format_file_view, line_numbers_at, match_offsets,
    maybe_truncate, validate_view_range, DEFAULT_MAX_OUTPUT_CHARS, TRUNCATED_MESSAGE,
};

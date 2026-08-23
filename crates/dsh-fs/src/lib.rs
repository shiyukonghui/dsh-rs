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

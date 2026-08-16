//! Entry 组（Cordis `EntryGroup`：有序子入口 id 列表）。

use serde::{Deserialize, Serialize};

/// 一组有序的子入口 id。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntryGroup {
    pub data: Vec<String>,
}

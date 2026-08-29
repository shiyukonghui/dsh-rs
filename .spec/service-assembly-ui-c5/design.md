# 设计结论：桌布 C5 —— 运行时热插拔同步 + `ui-manifest-changed` 广播

日期：2026-09-05 | 阶段：系统设计 | 决策记录 **D-186**。

## 1. 形状

```
ui_manifest.rs（纯/宿主侧，全部可测）
  pub struct UiManifestWatchState { last_check_ms: u64, last_rev: String, mounted: Vec<String> }
  pub fn init_watch_state(boot: &Boot) -> UiManifestWatchState        // 启动基线（rev 现算，不广播）
  pub fn ui_manifest_watch_tick(
      boot: &mut Boot, wasm_base: &Path, now_ms: u64, st: &mut UiManifestWatchState
  ) -> Option<String>                                                  // Some(new_rev) = 需广播
  scan_remote_units_opts(wasm_base, build_missing: bool)               // 启动=true；watch=false
  （scan_remote_units 保留为 build_missing=true 的薄封装）

hmr_events.rs
  fn ui_manifest_changed_line(rev) -> String                           // {type:"ui-manifest-changed",rev}
  pub fn broadcast_ui_manifest(&self, rev: &str)                       // 复用 clients mpsc 面

web.rs serve 主循环（tick 后）
  let mut watch = ui_manifest::init_watch_state(&boot);   // 启动 scan 之后
  loop { …dispatch…; …m5g_tick…;
    if let Some(rev) = ui_manifest::ui_manifest_watch_tick(boot, &wasm_base, now, &mut watch) {
        hmr.broadcast_ui_manifest(&rev);
    }
  }

assets/canvas/app.js
  new EventSource('/plugins/events') → onmessage：JSON.parse，type=="ui-manifest-changed"
  → loadManifest()；轮询间隔 4s → 10s（SSE 断线兜底）
```

## 2. `ui_manifest_watch_tick` 语义（S2-S6 钉死）

1. 节流：`now_ms - st.last_check_ms < 2000` → `None`（不重扫）。
2. 重扫（**不构建**）：`desired = scan_remote_units_opts(base, false)`（目录缺失 = 空集，诚实）。
3. 同步 boot：
   - 新挂载：`desired` 中不在 `st.mounted` → 读构件字节；空/载体加载失败 → eprintln **跳过**
     （不上死卡、不炸循环）；成功 → `remote_carriers.push` + `packages.push` + `mounted.push`。
   - 卸载：`st.mounted` 中不在 `desired` → `packages.retain(!= name)` + `carriers.retain(!= ns)` +
     `mounted.retain`（**只动 scan 挂载的**，S4）。
4. rev = `build_manifest(&boot.packages, loader_entries).rev`；`!= st.last_rev` → 更新并
   `Some(rev)`；否则 `None`。

`boot.packages` 变化同时改变 `/api/uiManifest/list` 与静态面（`serve_package_asset` 读
同一 Vec），**卸载即卡片消失 + 资产 404**——一个同步点，两面生效（无双权威）。

## 3. 测试计划（TDD；Rust 全测，app.js DOM 边界照旧诚实声明）

- `ui_manifest_changed_line_wire_format`（S1）
- `watch_tick_mounts_new_unit_and_broadcasts`（S2：真 llm-deepseek 构件字节复制到 temp 单元目录；
  广播经 `HmrChannel::connect()` 的 rx 收取验证）
- `watch_tick_edited_ui_json_changes_rev`（S3）
- `watch_tick_unmounts_removed_unit_only_scan_mounted`（S4：boot.packages 预置一个非 scan 包，
  删单元后它仍在）
- `watch_tick_throttles_within_window`（S5）
- `watch_tick_skips_unit_without_component_never_builds`（S6：只有 plugin.json+web，无 wasm →
  不挂载；断言 mounted 不含 + 返回 None）

**红验证**：先写测试 + `tick` 桩（返回 None / 同步不实现）→ 全红；实现转绿。

## 4. 回滚点

ui_manifest 两函数 + hmr 两函数 + serve 一钩 + app.js 十余行；撤提交回 `11021e8`。

# 阶段 2 · panel-plugin-inventory（插件清单）— ✅ 通过（含 P2 合并行的浏览器级证据）

- 功能清单：只读卡；dataRpc `panel-plugin-inventory/list` → 自有载体 →
  host services `loader`（实时条目）+ `contract`（P2 不兼容合并行 + note 列）。
- 静态链路：四层在场（矩阵）；无缺口。
- 浏览器实测 A（常规态）：真 loader 条目上屏
  `echo-loop loop active` / `dsh:services services active`，四列表头齐
  （插件/入口/状态/说明），console 零错误，pass=true。
- 浏览器实测 B（**功能点强证**：P2 不兼容合并行 DOM 级）：注入
  `panel-runtime-status` 坏 requires → mount-sync 窗口内新页面清单卡出现
  `panel-runtime-status — incompatible requirement-unsupported` 行
  （同时画布 12 卡=该单元真被协商关挡下）→ 复原 plugin.json → manifest 回 13。
- 判定：清单的「实时反映装配态」语义（含不兼容并表）在浏览器真实发挥作用。

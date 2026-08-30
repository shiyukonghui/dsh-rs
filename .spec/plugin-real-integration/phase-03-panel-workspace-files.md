# 阶段 3 · panel-workspace-files（工作区文件）— ✅ 通过

- 功能清单：只读卡；dataRpc `panel-workspace-files/list` → 自有载体 →
  host services `agentWorkspace`（默认工作区 cwd）+ `workspaceFiles`（真实 fs 扫描）。
- 静态链路：四层在场（矩阵）；无缺口。
- 浏览器实测：真实磁盘条目上屏（`F:\RustProjects\dsh-rs\.cargo`、`.git`、
  `.gitattributes`、`.gitignore`、`canvas-shell`… 共 16 行），列头「文件路径」，
  console 零错误，pass=true。
- 判定：工作区文件可见性=真实 fs 投影，功能在浏览器中真实发挥作用。
- 动作面：无（只读卡）。

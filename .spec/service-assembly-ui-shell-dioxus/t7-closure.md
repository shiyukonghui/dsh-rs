# T7 热插拔疑云结案（第 59 轮）：壳无罪，审计技术有罪

## 结论
loop 使能（--agent-loop）时代的「T7 波动」根因 = **审计假卸载技术**：
`rename wasm-plugins/<unit> → <unit>.off` 仍在扫描树内，mount-sync 重扫约 1.2s
把 `.off` 目录里的 ui.json+组件重新计回 → manifest 回 13 → DOM 恒 13 是**诚实正确行为**。
pre-loop 时代的多轮「绿」实为幸运采样撞进 1s 瞬态 12 窗口（两个时代都在测瞬态，
只是运气不同）。**真卸载 = 整目录移出 wasm-plugins（同盘树外 `.off-store/`）**。

## 证据链（全部同 serve、--agent-loop、真组件）
1. `.off` 树内 + 200ms 细粒度时间线：rpc `12×6样本→13×34样本`，dom 恒 13（假卸载现形）。
2. 页面旁路 EventSource 实验：rename 期间收到完整 `{"rev":…,"type":"ui-manifest-changed"}`
   帧（传输层无罪）。
3. 移出树外探针：rpc 恒 12，**DOM 1400ms 落 12**，restore 后 ~1s 回 13（完整闭环）。
4. 审计 T7 修正后全量重跑：`m1=12 dom1=12 dom2=13` 全绿 + consoleErrs=[]。

## 修正入库
- `e2e-audit.mjs` T7 与 `e2e-hotplug.mjs`（新）：改用 `.off-store/` 树外移技术。
- `e2e-sse.mjs`（新）：SSE 传输层旁路实验工具。
- 附带观察：审计 T5 在 loop 使能下诚实翻转（schedule host 已装配，创建成功=True），
  语义从「诚实错误」变「创建成功」，环境更健康所致，保留为观察项非缺陷。

## 教训（固化）
测「卸载」必须让目标**从所有扫描面消失**（移出扫描树），重命名/改后缀都是在
赌某一条扫描线的过滤规则——同一目录下不同扫描线（RPC list vs mount-sync）
过滤规则可以不一致，赌赢一次不代表技术正确。

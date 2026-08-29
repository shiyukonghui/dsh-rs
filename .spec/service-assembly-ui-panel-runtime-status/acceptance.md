# 验收结论：面板改写 #2 —— panel-runtime-status（运行时状态卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-187** | git：本提交（上游 `62b7802`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | v2 status 卡契约（dataRpc 显式、items 不硬编码、size 无坐标、type 闭集） | m34 `describe_ui_returns_valid_status_declaration` | ✅ |
| S2 | 一份契约 | m34 `static_ui_json_matches_describe_ui` | ✅ |
| S3 | 跨服务聚合 + tone（disabled>0→warn，group 不计数） | m34 `status_aggregates_loader_and_dynamic_plugins` | ✅ |
| S4 | 任一服务失败整体 fail-loud，不部分伪造 | m34 `status_fail_loud_when_any_service_down`（错误体无 items） | ✅ |
| S5 | 未知端点 fail-loud；scan 自动挂载 + 清单第三卡 | m34 `unknown_endpoint_fail_loud` + `scan_mounted_units_appear_in_manifest` 扩断言（宿主清单层零改动） | ✅ |
| S6 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m32 8/8、m33 5/5、m34 5/5）、clippy **0** | ✅ |

## TDD 记录
m34 先对不存在包红（5 FAILED：构建失败/行为缺失）→ 包落地转绿；一处类型错
（`error(&str)` 传 String）编译期暴露即修；clippy 抓到测试文件 unused import。

## 诚实台账
1. status 渲染器的**浏览器端到端**（dataRpc→items→DOM）仍未手测（无浏览器基建）；
   `statusItems` 纯函数 + m34 数据面形状（`value.items` 契约位）双向锁定。
2. tone 规则（warn 阈值等）是单元自持的诚实判断，非契约条款——可随面板演进。
3. 进度：面板改写 **2/N**（台账见 `.spec/service-assembly-ui-panels/progress.md`）；
   聊天面板依赖 `chat` 契约预留渲染器点亮，属后续独立流程。

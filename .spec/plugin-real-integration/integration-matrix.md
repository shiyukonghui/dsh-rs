# 对接矩阵 · 13 卡 + plan（2026-09-06 实勘）

**总结论：静态链路四层（单元载体 / host-remote 回退 / RemoteHost 投影臂 / 宿主
handle_rpc 特判臂）无一缺席；11 个只读端点线上探针全部 `ok:true` 且数据真实**。
「真正对接」的剩余工作量因此**不在补臂**，而在：①浏览器语义级确认（DOM 真实
渲染/中文无损）；②动作面（写侧）端到端真实生效 + 回滚纪律；③三个疑难单元
（approval/dynamic-plugins/llm）的「真实可用」形态定义。

## 逐单元矩阵

| 单元/卡 | UI 调用面 | 承载层 | 只读实测（60890） | 动作面 | 浏览器确认 |
|---|---|---|---|---|---|
| panel-plugin-inventory | own/list | 载体（loader+contract 服务） | ✓ 真行（echo-loop 等） | 只读卡 | 部分（T0 计数级） |
| plan | own/projection,exitCheck | 载体（planEvents） | ✓（T17） | 只读+判定（v2 留写） | ✓ T17 |
| panel-sessions | own/list | 载体（sessionCandidates） | ✓ 真会话 default | 只读卡 | ✗ |
| panel-workspace-files | own/list | 载体（agentWorkspace+workspaceFiles） | ✓ 真 fs 扫描 | 只读卡 | ✗ |
| panel-settings | own/list | 载体（settingsDescribe） | ✓ 真行（llm.model=echo…） | 只读卡 | ✗ |
| panel-runtime-status | own/status | 载体（loader/事件计数） | ✓ items（PS 控制台显中文乱码=终端显示问题，**浏览器端需验真伪**） | 只读卡 | ✗ |
| panel-settings-edit | 宿主臂 settings.describe/update | 宿主特判臂 | describe ✓ | **save=写设置** | ✗ |
| panel-locale-edit | 宿主臂 settings.describe/update | 同上 | ✓ | **save=写设置** | ✗ |
| panel-chat | 宿主臂 session.history/prompt/cancel | 宿主特判臂 | history ✓ | send/cancel（长 RPC） | ✓ T6/T11 |
| panel-schedule | 宿主臂 schedule/list,delete | 宿主特判臂 | ✓ 空表 | **delete=写** | 部分（T5 创建侧） |
| panel-schedule-create | 宿主臂 schedule/create | 宿主特判臂 | —（表单卡） | **create=写+60s 真触发** | 部分（T5） |
| panel-approval | 宿主臂 approval/pending + session.approval.decide | 宿主特判臂 | ✓ 空 pending | **decide=真实放行/拒绝**（需先制造 pending 审批） | ✗ |
| panel-dynamic-plugins | own/list/activate/stop/undefine（dynamicPlugins+dynamic* 服务臂） | 载体 | ✓ 诚实空（冒烟未配 --dynamic-plugins-dir） | activate/stop/undefine（需动态目录+包） | ✗ |
| llm-deepseek | own/currentValues,save,discoverModels（svc()→settings 读写） | 载体 + 真 HTTP（discover） | currentValues ✓ `{}`（冒烟 llm=echo，deepseek ns 无值——语义待核） | save=写设置；discoverModels=外呼 | ✗ |

## 疑点清单（各阶段验证时重点盯）

1. **runtime-status 中文标签**：PS 探针显 `loader æ¡çç®` 形态——大概率是控制台
   解码问题，但**必须浏览器 DOM 端钉死**（若真坏则升级为编码缺陷对接）。
2. **locale-edit save**：写 locale 后是否有真实效果面（i18n 面在壳上是否有消费）
   ——若宿主/壳无 locale 消费者，「正常发挥作用」如何定义需用户裁决。
3. **approval 真实场景**：需制造真 pending（echo provider 不出工具调用 → 可能需
   构造触发路径或注入审批源）；空 pending 的诚实呈现 ≠ 功能验证，用户裁量深度。
4. **dynamic-plugins**：冒烟未配动态目录=卡永远空。真对接验证需带
   `--dynamic-plugins-dir` + 测试包的场景（或复用 .off-store 里现成包）。
5. **llm-deepseek currentValues={}`**：冒烟 llm 是 echo/dsh，deepseek ns 空可能
   本就正确；需核 currentValues 读的是哪个 ns 的 settings（代码 svc() 变量名）。
6. **schedule create→触发→chat 可见** = 最完整的「功能真正发挥作用」端到端样板，
   适合中后段做（牵涉 agent loop，等待 60s+）。

## 探针原始记录（2026-09-06，:60890，--service-units 冒烟）

11 端点全 `ok:true`：settings/describe（namespaces 含 llm/locale/ui-theme…）、
schedule/list（空）、approval/pending（空）、runtime-status/status（数字项×n）、
sessions/list（default）、workspace-files/list（真目录）、settings/list（真行）、
inventory/list（loader 真行）、dynamic-plugins/list（空）、llm/currentValues（{}）、
session/history（no-such → 空事件 + projections 在场）。

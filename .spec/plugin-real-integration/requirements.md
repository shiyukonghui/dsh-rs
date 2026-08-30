# 需求结论 · 插件功能真实对接（v1，已确认 2026-09-06）

日期：2026-09-06 ｜ 状态：**需求分析中（待用户确认后进设计）**

## 1. 目标（第一性原理还原）

剥掉惯例说法，本目标的不可分基本事实：**每张服务装配单元的卡，其 UI 声明的
每个功能（数据面 + 动作面），用户在浏览器里真实操作后，系统做出该功能语义上
应有的真实行为，且效果可见/可复查**。不是「卡能画出来」，不是「RPC 返回 200」，
而是**功能语义端到端成立**，且**每个插件的成立与否都由浏览器实测证据背书**。

成功标准（自上而下）：
- S1 全覆盖：13 张卡 + plan 服务单元逐个过，无一遗漏；
- S2 真实：数据来自真实服务态（loader/会话/调度/文件/设置…），动作真实改变状态；
- S3 浏览器背书：每插件的验证必须在真实页面（CDP 驱动的 headless Edge）完成，
  证据=DOM 断言 + RPC 线报文 + console 零错误；
- S4 一阶段一插件：每阶段完成**一个**插件的对接 + 浏览器验证 + 提交，过关才下一个。

## 2. 非目标

- 不重做 UI 视觉；不动壳（canvas-shell）除非对接确证需要（届时单独决策）；
- 不引入新能力（每插件只做其 ui.json 已声明功能的真对接）；
- LLM 真实外呼不在本目标硬验收内（外部凭据问题，单列「待凭据」类，见 §5 Q3）；
- 不做性能/并发目标。

## 3. 现状基线（自下而上，证据在 integration-matrix.md）

**实勘关键结论（2026-09-06）**：四层承载（单元载体 / host-remote 回退 /
RemoteHost 投影臂 / 宿主 handle_rpc 特判臂）**无一缺席**；11 个只读端点线上
探针全 `ok:true` 且数据真实。因此本目标的工作量**不在补臂**，而在三件事：
①每插件**浏览器语义级确认**（DOM 真渲染、中文无损、行数据真实）；
②**动作面端到端**（写侧真实生效 + 可见 + 回滚/留痕纪律）；
③三个疑难单元的「真实可用」形态定义（approval 需真 pending、
dynamic-plugins 需动态目录场景、llm-deepseek 需凭据语义裁决）——见 §5。

## 4. 每插件验收模板（阶段产物 = 一插件一份）

1. **功能清单**：从 ui.json/lib.rs 提取该插件全部端点（数据面+动作面）；
2. **静态链路核验**：载体/host-remote/投影臂/宿主 RPC 臂四层逐臂在场证明；
3. **缺口对接**：缺席臂按现有惯例补齐（TDD：先写失败的对接测试）；
4. **浏览器实测**（CDP）：
   - 数据面：打开卡 → 真实数据行出现（DOM 断言非空/形状对）；
   - 动作面：填表/点击 → 断言真实状态变化（再读数据面/宿主 RPC 双确证），
     动作可回滚的验证后回滚（不可回滚的在冒烟会话内做并记录）；
   - console 零错误 + 相关日志无 fail 噪音；
5. **提交**：对接改动 + 审计脚本 T<n> 增量 + 决策日志（若有决策）。

## 5. 待确认问题（**已全部裁决，见 §8**）

- **Q1 顺序**：默认按「依赖轻→重」排：panel-sessions → panel-plugin-inventory →
  panel-workspace-files → panel-settings → panel-settings-edit → panel-locale-edit →
  panel-runtime-status → panel-dynamic-plugins → panel-approval → panel-chat →
  panel-schedule → panel-schedule-create → llm-deepseek。是否按此序？
- **Q2 动作面尺度**：动作会写真实状态（设置改动/日程创建/会话消息）。默认在
  **冒烟场景**（scenarios/web-smoke.cordis.yml + 专用测试会话）内做真写并尽量
  验证后回滚；不可回滚项（如日程项触发）留痕执行。是否认可？
- **Q3 LLM 真调用**：llm-deepseek 真对话需外部 API key。本目标内按其
  「配置面真实可用 + 无 key 时诚实错误」验收，真外呼凭据由您另行提供再做？
- **Q4 缺口对接权限**：若矩阵发现某端点链路断裂（臂缺席），默认**就地补齐**
  （按现有投影/宿主臂惯例，TDD），还是先报告给您裁决？

## 6. 假设（用户未明说但默认成立，已按此展开）

- 验证环境 = 现有 headless Edge + CDP + 60890 冒烟 serve 现场（审计基建复用）；
- 「插件」= wasm-plugins 下 13 卡单元 + plan 服务单元（demo 族 hello*/echo-loop/
  tool-loop/combo-eval/llm-loop 属测试夹具，不在验收面）；
- 每阶段一个插件 = 一个提交 + 审计脚本一条 T 编号（延续 T18 起）。

## 7. 边界与风险

- plan 单元读面已对拍收口；其写面 v2（事件追加接缝）**不在本目标**，
  除非您指定；
- sse-reload-starvation 旧缺陷（事件通道饥饿）可能干扰长审计——验证脚本按
  「新鲜页起步 + 短会话」设计规避，修复另轮；
- 每插件对接若牵出设计级问题（如双权威），停下回设计阶段修正（瀑布纪律）。

## 8. 用户裁决记录（2026-09-06，需求关卡通过）

- **R1** 确认需求边界与逐插件模板，按序推进（sessions → inventory →
  workspace-files → settings → runtime-status → settings-edit → locale-edit →
  dynamic-plugins → schedule → schedule-create → chat → approval → llm-deepseek）；
- **R2** 动作面真写授权：可回滚项验证后回滚；不可回滚项（日程触发）留痕执行；
- **R3** **LLM 真实凭据已提供**（base URL/模型/key；存 `target/verify-secrets.env`，
  gitignore 内**绝不入库**；文档/提交/日志一律只写「凭据已提供」不写值）；
  llm-deepseek 按真实外呼全形态验收；
- **R4** approval（注入审批源造真 pending）与 dynamic-plugins（配动态目录+包）
  按最真实形态验收。

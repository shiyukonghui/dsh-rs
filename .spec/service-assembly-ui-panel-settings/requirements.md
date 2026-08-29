# 需求结论：面板改写 #6 —— 设置概览卡（panel-settings，只读）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-192。
数据面实证：`settings.describe`（web.rs:4292）经 `describe_all`（**源头已 redact**）
投影 `namespace_view {ns,schema,value,base?,user?,applies,secrets,revision}`；
RemoteHost 现无 settings 引用（构造点 4：serve + 3 测试）。

## 1. 目标（第一性）

设置面板是 harness 核心面板。写端（编辑表单）依赖「动态 fields」契约演进（async schema，
D-187 已裁定独立决策）；**读端今天就能做**：投影器加一个只读 arm（复用 `namespace_view`，
零新形状），单元把各 namespace 的 resolved value 拍平成概览行。与 #5「发现端先行」同策略：
先做无争议的一半。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 宿主投影 | `RemoteHost::get("settingsDescribe")` → 与原生 `settings.describe` **同形状**（复用 `namespace_view`，一个视图函数两处用，杜绝双源） |
| 2 | RemoteHost 装配 | 构造器加第 4 参 `settings: Option<...>`（serve 传真实引用；测试传 None → 该 arm 报 unknown-service? 不——**None → `{ok:false,error:"no-settings"}`**，诚实缺依赖） |
| 3 | 行投影（单元） | 每 ns 的 resolved value 顶层字段拍平 `{ns, field, value}`；非对象 value → 单行 `{ns, field:"—", value}`（object/array 值原样出，list 渲染器 stringify） |
| 4 | 敏感面 | 不自行 redact（describe_all 源头已做，secrets[].set 仅存在性）；单元**不展开 secrets 路径** |
| 5 | 写端 | **不做**（settings.update 需动态 fields 表单契约；概览卡只读） |
| 6 | type/尺寸 | `config`（分类表语义位首卡）；4×4 |

## 3. 验收判据
S1 宿主：`settingsDescribe` 投影 == 原生 describe 形状（一致性断言 + 无 settings 引用诚实报错）；
S2 单元 v2 list 契约 + type config + 一份契约；S3 行拍平正确（含非对象 value 行）；
S4 服务失败 fail-loud 无 items；S5 未知端点 fail-loud；S6 清单第七卡；S7 回归 + clippy 0。

## 4. 边界
不做编辑/保存（写端契约演进）· 不做 schema 树展示 · secrets 值永不进卡（源头 redact + 单元不展开）。

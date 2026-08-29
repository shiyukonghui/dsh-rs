# 验收结论：面板改写 #6 —— panel-settings（设置概览卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-192** | git：本提交（上游 `36fa730`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | 宿主投影与原生 describe 同形状；缺引用诚实报错 | remote_host tests `settings_describe_projection_matches_native_shape` + `settings_describe_without_reference_is_honest`（**伪造空表探针被抓红**后还原） | ✅ |
| S2 | v2 list 契约 + type config + 一份契约 | m38 契约 2 测 | ✅ |
| S3 | 行拍平（对象逐键行 + 非对象占位行；键序非契约按 (ns,field) 查找） | m38 `list_flattens_namespaces_to_rows` | ✅ |
| S4 | 服务失败 fail-loud 无 items | m38 `list_service_failure_is_fail_loud` | ✅ |
| S5 | 未知端点 fail-loud | m38 `unknown_endpoint_fail_loud` | ✅ |
| S6 | scan 挂载 + 清单第七卡（config 首卡） | `scan_mounted_units_appear_in_manifest` 扩断言 | ✅ |
| S7 | 回归 | dsh-cli **253/0**（+2 宿主测）、dsh-wasmrt 全绿（m32–m38）、clippy **0** | ✅ |

## TDD 记录
宿主测试先绿后探针（空 namespaces 注入 → 必红 → 还原）；m38 先对不存在包红 → **5/5**；
一处测试自身的键序假设在红前修正（value 对象键序非契约）。

## 诚实台账
1. **只读边界**：编辑/保存未做（写端 = 动态 fields 契约演进，D-187 已立题）；卡上无编辑 affordance。
2. 概览行 = resolved 值的顶层拍平，**schema/校验信息不展示**（深层结构属未来详情形态）。
3. 敏感面单一权威：provider 源头 redact，单元不展开 secrets、不解除脱敏。
4. 进度 **6/N**：D-181 分类表五个语义位（model/runtime/resource/session/**config**）全部有真实卡。

# 阶段 13 · llm-deepseek（DeepSeek Provider）— ✅ 通过（真凭据全形态）

- 功能清单：form 卡（apiKeyEnv/baseURL/thinking/reasoningEffort/maxTokens/
  defaultContextWindow/models）+ save（kv 持久化）+ discoverModels（模型发现）。
- **三个真缺口（本轮全闭）**：
  1. **D-221 卡面→生产 loop 不接线**：conn/传输闭包不读任何卡设置（settings 或 kv）
     ——provider 卡对 loop「零影响」。修复=共享 kv 权威（RemoteHost 单源
     `Arc<Mutex<…>>`）注入 runtime 闭包，`provider_cfg` 每调用 live 合并
     （装配基址 ← env 缺省(D-219) ← 卡面 baseURL/effort/thinking 覆盖）。
  2. **D-222 discoverModels 是桩**（返回硬编码目录，注释自认「真实网络探测留后续」）。
     修复=新宿主臂 `llmDiscover`（GET {baseURL}/models，key 与 loop 同权威 env-only）
     + dsh-core `http_get_json`（复用既有手写传输；**含 chunked 解码**——
     node/uvicorn 对 JSON 也分块，实测无解码必坏）+ 单元改契约（表单/已存 baseURL
     →宿主臂透传；无 baseURL 诚实报错）。
  3. **D-223 动作体/结果契约断链（画布面）**：①FormSave 默认平铺 args，单元契约
     `{values}`——卡面 save 必失败；②form_specs 装配白名单静默丢未知键，
     声明到不了 FormSave；③set_input_value 对 textarea 无效（textarea 无 value 属性）。
     修复=声明式 `valuesKey`（包装键）+ `resultToField/resultPath`（结果注入）
     贯通 ui.json→装配→FormSave，textarea set_value 补上。
- 浏览器全链（verify-llm.mjs，**5/5 PASS**，console 零错）：
  save(valuesKey)✓已保存 → 重载 currentValues 回填=桩地址（kv 往返）→
  **发现模型：真外呼臂打到本地桩（stubHits.models=1）+ models textarea 注入
  stub-model-a + act「✓ 发现 2 项」**（D-222/223 浏览器铁证）→
  **卡 baseURL=桩 → chat 轮经桩回复 STUB-REPLY 上聊天卡（stubChat=1）**
  （D-221 热覆盖铁证：卡设置即时改变循环外呼地，无需重启）→
  复原真端点+effort=low → chat **真 qwen 回复落 assistant/message**（真链活体）。
- 测试：dsh-core llm_http 7/7（新 chunked 测试）；m32 10/10（discoverModels 契约
  三例替换桩断言）；dsh-cli lib 274/274（provider_cfg 双测试：kv 覆盖+坏值忽略）。
- 诚实记录（审计 T5_honestErr=false）：审计断言假设「schedule host 未装配→诚实
  报错」已过时——现网 host 装配，创建真实成功无 ✗ 属合法行为，非回归（探针任务
  已清理）。
- 挂死观察（待查 backlog）：审计跑完后 serve 曾整体无响应（CPU 静止、线程 38）；
  最小复现（黑洞端点长轮 40s 并发探测）不复现——排除「长 LLM 调用饿死 accept 线程」
  假设。指纹=「审计 reload 风暴 × 长时间运行 × 真调度触发」组合，与
  sse-reload-starvation 相邻但未证实同源。repro-hang.mjs 留作复现工具。

// ui.json 双语迁移（D-225 R1）：字符串文案位 → {zh,en}。字典驱动，未命中报告。
import fs from "node:fs";
import path from "node:path";
const EN = {
  // 标题
  "待审批": "Pending Approvals", "聊天": "Chat", "会话清单": "Sessions",
  "运行时状态": "Runtime Status", "调度任务": "Scheduled Tasks", "创建调度": "Create Schedule",
  "动态插件": "Dynamic Plugins", "插件清单": "Plugin Inventory", "工作区文件": "Workspace Files",
  "设置概览": "Settings Overview", "设置编辑": "Settings Editor", "设置编辑 · locale": "Settings Editor · locale",
  // 动作
  "保存": "Save", "允许": "Approve", "拒绝": "Reject", "删除": "Delete",
  "启用": "Start", "停止": "Stop", "卸载": "Unload", "发现模型": "Discover Models",
  // 列/字段名
  "ID": "ID", "类型": "Type", "提示": "Prompt", "计划时间": "Planned At",
  "插件": "Plugin", "插件 ID": "Plugin ID", "包名": "Package", "入口": "Entry",
  "状态": "Status", "说明": "Description", "名称": "Name", "值": "Value",
  "命名空间": "Namespace", "键": "Key", "会话": "Session", "路径": "Path",
  "大小": "Size", "修改时间": "Modified", "模式": "Mode", "模型": "Model",
  "Base URL": "Base URL", "温度": "Temperature", "最大 tokens": "Max tokens",
  // 空态
  "没有待审批项": "No pending approvals", "暂无条目": "No items",
  "暂无调度": "No schedules", "没有调度记录": "No schedule records", "暂无会话": "No sessions", "暂无插件": "No plugins",
  // 二轮补齐（描述/列/空态/字段）
  "DeepSeek provider 连接与模型目录设置": "DeepSeek provider connection & model catalog settings",
  "API Key 环境变量": "API Key env var", "Models（目录）": "Models (catalog)", "显示名": "Display name",
  "未决工具审批（决定走宿主 session.approval.decide；拒绝需确认）": "Pending tool approvals (decided via host session.approval.decide; reject needs confirm)",
  "工具": "Tool", "原因": "Reason",
  "会话聊天（选择/历史/发送/停止；会话协议在宿主，C8-4 声明单元）": "Session chat (pick/history/send/stop; protocol lives in the host)",
  "dynamicCordisRunner 定义与运行态（启用/停止/卸载）": "dynamicCordisRunner definitions & runtime (start/stop/unload)",
  "没有已定义的动态插件": "No dynamic plugins defined",
  "语言偏好（D-200 多 ns 机械复制首卡；保存带乐观锁）": "Language preference (multi-ns settings card; optimistic-locked save)",
  "loader 已组装服务装配单元的实时清单（只读）": "Live inventory of assembled service units (read-only)",
  "暂无已组装入口": "No assembled entries",
  "loader / 动态包实时聚合（只读）": "Live loader / dynamic package aggregate (read-only)",
  "after/at/every 调度记录（只读；协议在宿主事件日志权威）": "after/at/every schedules (read-only; host event log is authoritative)",
  "after/at/every 调度创建（写端；动作走宿主 schedule/create 臂）": "Create after/at/every schedule (writes via host schedule/create)",
  "延迟秒（after）": "Delay seconds (after)", "创建": "Create",
  "宿主真实会话候选（只读；打开/切换属未来交互形态）": "Host session candidates (read-only)",
  "创建 (epoch ms)": "Created (epoch ms)", "还没有会话": "No sessions yet",
  "已注册 settings 命名空间的 resolved 值（只读；redact 在源头）": "Resolved values of registered settings namespaces (read-only; redacted at source)",
  "字段": "Field", "没有已注册的设置": "No registered settings",
  "命名空间下拉 + 动态 fields 投影（D-201 一卡通用；保存带乐观锁；secrets 不可编辑）": "Namespace picker + dynamic fields (optimistic-locked save; secrets not editable)",
  "agent 默认工作区顶层文件（只读）": "Top-level files in agent workspace (read-only)",
  "文件路径": "File path", "工作区没有文件": "Workspace has no files",
};
const KEYS = new Set(["title", "description", "emptyText", "label"]);
let mapped = 0; const unmapped = new Set();
const isCJK = (s) => /[\u4e00-\u9fff]/.test(s);
function walk(v, key) {
  if (Array.isArray(v)) { v.forEach((x) => walk(x, key)); return; }
  if (!v || typeof v !== "object") return;
  for (const [k, child] of Object.entries(v)) {
    if (typeof child === "string" && KEYS.has(k)) {
      if (!isCJK(child)) continue;
      if (EN[child] !== undefined) { v[k] = { zh: child, en: EN[child] }; mapped++; }
      else { unmapped.add(`${key}: ${child}`); v[k] = { zh: child, en: child }; mapped++; }
    } else if (child && typeof child === "object" && KEYS.has(k) && !Array.isArray(child)) {
      // 二轮：已包裹但 en 仍是中文回退 → 按字典升级。
      const zh = child.zh, en = child.en;
      if (typeof zh === "string" && en === zh && EN[zh] !== undefined) { v[k].en = EN[zh]; mapped++; }
      else if (typeof zh === "string" && isCJK(en || "") && EN[zh] === undefined) unmapped.add(`${key}: ${zh}`);
      else continue;
    } else if (child && typeof child === "object") walk(child, key);
  }
}
for (const dir of fs.readdirSync("wasm-plugins")) {
  if (!dir.startsWith("panel-") && dir !== "llm-deepseek") continue;
  const f = path.join("wasm-plugins", dir, "web", "ui.json");
  if (!fs.existsSync(f)) continue;
  const doc = JSON.parse(fs.readFileSync(f, "utf8"));
  walk(doc, dir);
  fs.writeFileSync(f, JSON.stringify(doc, null, 2) + "\n", "utf8");
  console.log("migrated", f);
}
console.log("wrapped:", mapped, "unmapped:", unmapped.size);
for (const u of unmapped) console.log("  UNMAP", u);

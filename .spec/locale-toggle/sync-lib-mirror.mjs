// lib.rs 镜像同步（D-225）：ui_declaration() 字面量 → include_str! 解析 web/ui.json。
// 收益：describeUI==ui.json 一致由构造保证（m3X 断言永久免漂）。
import fs from "node:fs";
import path from "node:path";
const NEW_BODY = `fn ui_declaration() -> Value {
    // D-225：单一事实源=web/ui.json（编译期嵌入；声明=数据，非代码）。
    serde_json::from_str(include_str!("../web/ui.json")).expect("ui.json must be valid JSON")
}`;
let done = 0;
for (const dir of fs.readdirSync("wasm-plugins")) {
  const lib = path.join("wasm-plugins", dir, "src", "lib.rs");
  if (!fs.existsSync(lib)) continue;
  let src = fs.readFileSync(lib, "utf8");
  const start = src.indexOf("fn ui_declaration() -> Value {");
  if (start === -1) continue;
  const end = src.indexOf("\n}\n", start);
  if (end === -1) { console.log("NO-END", dir); continue; }
  src = src.slice(0, start) + NEW_BODY + src.slice(end + 3);
  fs.writeFileSync(lib, src, "utf8");
  console.log("synced", dir);
  done++;
}
console.log("total:", done);

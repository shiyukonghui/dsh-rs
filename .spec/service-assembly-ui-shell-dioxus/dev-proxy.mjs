// S2 开发期静态代理：canvas-shell/dist 静态 + 其余路径透传 60890（/api、SSE、canvas.css）。
// 正式路线（/canvas/rust 内嵌路由）在 S5 双壳对齐后落地——本脚本是开发验证件，非产品面。
import http from "node:http";
import fs from "node:fs";
import path from "node:path";

const DIST = path.resolve("canvas-shell/dist");
const UP = "http://127.0.0.1:60890";
http.createServer(async (req, res) => {
  const url = (req.url || "/").split("?")[0];
  try {
    if (url === "/" || url === "/rust.html") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(fs.readFileSync(path.join(DIST, "rust.html")));
      return;
    }
    const safe = path.normalize(url).replace(/^(\.\.[/\\])+/, "").replace(/^[/\\]+/, "");
    const local = path.join(DIST, safe);
    // dist 全目录可服务（bindgen 胶水会 import ./snippets/... 相对路径），MIME 严格。
    if (local.startsWith(DIST) && fs.existsSync(local) && fs.statSync(local).isFile()) {
      const ext = path.extname(safe).toLowerCase();
      const type = ext === ".wasm" ? "application/wasm"
        : ext === ".js" ? "text/javascript; charset=utf-8"
        : ext === ".css" ? "text/css; charset=utf-8"
        : ext === ".html" ? "text/html; charset=utf-8"
        : "application/octet-stream";
      res.writeHead(200, { "content-type": type });
      fs.createReadStream(local).pipe(res);
      return;
    }
    const r = await fetch(UP + url, {
      method: req.method,
      headers: { "content-type": req.headers["content-type"] || "" },
      body: ["POST", "PUT"].includes(req.method) ? req : undefined,
      duplex: "half",
    });
    const headers = {};
    for (const [k, v] of r.headers) if (!["content-encoding", "content-length", "transfer-encoding"].includes(k)) headers[k] = v;
    res.writeHead(r.status, headers);
    if (r.body) res.write(Buffer.from(await r.arrayBuffer()));
    res.end();
  } catch (e) {
    res.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
    res.end("proxy err: " + e.message);
  }
}).listen(60700, "127.0.0.1", () => console.log("rust shell dev proxy :60700 -> :60890"));

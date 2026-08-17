//! hello-net 组件插件：WASI preview2 网络能力验证（M30）。
//!
//! apply(config) 时经 `std::net::TcpStream` 连接 `config.host:config.port`
//! （wasm32-wasip1 组件 → preview2 `wasi:sockets/tcp`）——caps 含 wasi-net 时
//! 连接成功（host_log "NET_OK=..."）；无 net 位时 `check_allowed_tcp` 拒绝
//! （host_log "NET_ERR=..."）。config 缺省连 `127.0.0.1:1`（拒绝连接端口，
//! 仍可区分「被能力拒绝」与「连接失败」）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::plugin::plugin_api::Guest;
use serde_json::Value;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

struct Component;

impl Guest for Component {
    fn apply(config: Vec<u8>) -> i32 {
        let config: Value = serde_json::from_slice(&config).unwrap_or(Value::Null);
        let host = config
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1")
            .to_string();
        let port = config.get("port").and_then(|v| v.as_u64()).unwrap_or(1) as u16;

        // WASI preview2 sockets：std::net::TcpStream 映射到 wasi:sockets/tcp。
        // 无 wasi-net 能力时宿主 check_allowed_tcp 拒绝 → 连接报错。
        match TcpStream::connect((host.as_str(), port)) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.write_all(b"dsh-net-probe\n");
                bindings::dsh::plugin::host_api::log(&format!("NET_OK={host}:{port}"));
                0
            }
            Err(e) => {
                bindings::dsh::plugin::host_api::log(&format!("NET_ERR={host}:{port}: {e}"));
                0
            }
        }
    }

    fn handle_event(event: String, payload: Vec<u8>) -> i32 {
        bindings::dsh::plugin::host_api::log(&format!("hello-net handle_event {event} {payload:?}"));
        0
    }

    fn dispose() -> i32 {
        bindings::dsh::plugin::host_api::log("hello-net dispose");
        0
    }
}

bindings::export!(Component with_types_in bindings);

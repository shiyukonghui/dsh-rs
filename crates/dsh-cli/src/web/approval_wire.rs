//! D-108/G：approval wire 注册表。把执行层审批门（段 B）的挂起/结算投影成前端
//! 可消费的 `approval/requested`（stable rpcId）/ `approval/resolved` MuxFrame，供
//! `events.mux` SSE/WS 下推 + `POST /api/respond` 接答复——对齐 harness fork 契约
//! （`packages/host/apiproxy/tests/api-proxy-approval.spec.ts`）：
//!
//! - `approval/requested`：server-request 信封 `{type:"server-request", rpcId:
//!   "approval-<n>", method:"approval/requested", payload:{type, sessionId,
//!   approvalId, toolName, callId, reason?}}`。rpcId 稳定；**pending 期间在 mux
//!   重开时逐字重放（同 rpcId）**（刷新恢复）。
//! - respond：前端应答 = `client-response`（echo requested 的 rpcId），走
//!   `POST /api/respond`；校验 sessionId+approvalId → 映射决策 → 返回
//!   RpcReceipt `{accepted:true}` / `{accepted:false, reason:"not-pending"|
//!   "bad-response"}`。
//! - `approval/resolved`：settle 后广播 `{type:"approval/resolved", sessionId,
//!   approvalId, outcome}`（outcome ∈ 四个 harness 字面量；client 只发前两个）。
//!
//! 线程纪律：serve 主线程 / agent driver 线程 / 请求线程都会触碰 → `Arc<Mutex>`
//! （对齐 host_events 既有模式）。帧日志 append-only：mux 线程各自持游标增量下推，
//! 无锁长期持有。
//!
//! 审计 id（approvalId）：由 call id 派生（`ap-<call_id>`）——稳定、唯一、可在
//! resume/decided 重推导，配对 `approval/asked`/`approval/decided`（对齐 harness
//! 「approvalId = 审计 id，按 callId 配平」）。不伪造批准来源：resolve 只由真实
//! 决策驱动。

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub type ApprovalWireRef = Arc<ApprovalWire>;

/// wire 上 client 两种可答 outcome（对齐 harness `ApprovalOutcome` 子集）。
pub const WIRE_OUTCOME_ALLOWED_ONCE: &str = "allowed-once";
pub const WIRE_OUTCOME_REJECTED: &str = "rejected";

/// 审计 id（approvalId = `ap-<call_id>`）：稳定唯一，可在 resume/decided 重推导，
/// 配对 `approval/asked`/`approval/decided`。
pub fn approval_audit_id(call_id: &str) -> String {
    format!("ap-{call_id}")
}

pub struct ApprovalWire {
    inner: Mutex<WireInner>,
}

struct WireInner {
    /// append-only 帧日志（requested 与 resolved 按 seq 混排）。
    frames: Vec<Value>,
    /// 未决条目（仅 requested 进入；resolve 移除）。
    pending: Vec<PendingEntry>,
    /// 下一个 rpcId 序号（每 ask 递增，保证跨 mux 重开稳定且唯一）。
    next_rpc: u64,
}

#[derive(Debug)]
struct PendingEntry {
    rpc_id: String,
    session_id: String,
    approval_id: String,
    call_id: String,
    /// 原 requested 帧（pending 重放直接复用，逐字同 rpcId）。
    requested: Value,
}

impl ApprovalWire {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(WireInner {
                frames: Vec::new(),
                pending: Vec::new(),
                next_rpc: 0,
            }),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut WireInner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    /// 当前帧日志长度（mux 重开时作为增量游标起点）。
    pub fn len(&self) -> usize {
        self.with(|i| i.frames.len())
    }

    /// 帧日志是否为空（空闲语义）。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 一条 requested（mint stable rpcId `approval-<n>`）→ 返回 rpcId。
    pub fn push_requested(
        &self,
        session_id: &str,
        call_id: &str,
        tool: &str,
        reason: Option<&str>,
    ) -> String {
        self.with(|i| {
            let rpc_id = format!("approval-{}", i.next_rpc);
            i.next_rpc += 1;
            let approval_id = approval_audit_id(call_id);
            let mut payload = json!({
                "type": "approval/requested",
                "sessionId": session_id,
                "approvalId": approval_id,
                "toolName": tool,
                "callId": call_id,
            });
            if let Some(r) = reason {
                payload["reason"] = json!(r);
            }
            let requested = requested_envelope(&rpc_id, payload);
            i.frames.push(requested.clone());
            i.pending.push(PendingEntry {
                rpc_id: rpc_id.clone(),
                session_id: session_id.to_string(),
                approval_id,
                call_id: call_id.to_string(),
                requested,
            });
            rpc_id
        })
    }

    /// 结算（按 rpcId）：移除 pending + 追加 resolved 帧。返回是否命中。
    pub fn resolve_by_rpc(&self, rpc_id: &str, outcome: &str) -> bool {
        self.with(|i| {
            let Some(pos) = i.pending.iter().position(|p| p.rpc_id == rpc_id) else {
                return false;
            };
            let entry = i.pending.remove(pos);
            i.frames.push(resolved_envelope(
                &entry.rpc_id,
                &entry.session_id,
                &entry.approval_id,
                outcome,
            ));
            true
        })
    }

    /// 结算（按 call id）——`session.approval.decide` 后向兼容路径（respond 之外的
    /// 决策入口也能推进 wire，前端 pending 不悬挂）。
    pub fn resolve_by_call_id(&self, call_id: &str, outcome: &str) -> bool {
        self.with(|i| {
            let Some(pos) = i.pending.iter().position(|p| p.call_id == call_id) else {
                return false;
            };
            let entry = i.pending.remove(pos);
            i.frames.push(resolved_envelope(
                &entry.rpc_id,
                &entry.session_id,
                &entry.approval_id,
                outcome,
            ));
            true
        })
    }

    /// 按 rpcId 取未决 `(session_id, approval_id, call_id)`（respond 校验用）。
    pub fn pending_by_rpc(&self, rpc_id: &str) -> Option<(String, String, String)> {
        self.with(|i| {
            i.pending
                .iter()
                .find(|p| p.rpc_id == rpc_id)
                .map(|p| (p.session_id.clone(), p.approval_id.clone(), p.call_id.clone()))
        })
    }

    /// pending 重放：仍未决的 requested 帧（逐字，含原 rpcId）——mux 重开恢复。
    pub fn pending_requests(&self) -> Vec<Value> {
        self.with(|i| i.pending.iter().map(|p| p.requested.clone()).collect())
    }

    /// 增量帧：seq >= cursor → `(new_cursor, frames)`（requested+resolved 按序）。
    pub fn frames_since(&self, cursor: usize) -> (usize, Vec<Value>) {
        self.with(|i| {
            let n = i.frames.len();
            (n, i.frames.iter().skip(cursor).cloned().collect())
        })
    }
}

impl Default for ApprovalWire {
    fn default() -> Self {
        Self::new()
    }
}

fn requested_envelope(rpc_id: &str, payload: Value) -> Value {
    json!({
        "type": "server-request",
        "rpcId": rpc_id,
        "method": "approval/requested",
        "payload": payload,
    })
}

fn resolved_envelope(rpc_id: &str, session_id: &str, approval_id: &str, outcome: &str) -> Value {
    json!({
        "type": "server-request",
        "rpcId": rpc_id,
        "method": "approval/resolved",
        "payload": {
            "type": "approval/resolved",
            "sessionId": session_id,
            "approvalId": approval_id,
            "outcome": outcome,
        },
    })
}

/// RpcReceipt（harness `rpcReceiptSchema`）。
fn receipt(accepted: bool, reason: &str) -> Value {
    if accepted {
        json!({ "accepted": true })
    } else {
        json!({ "accepted": false, "reason": reason })
    }
}

/// `POST /api/respond` 处理器：解析 client-response（echo requested 的 rpcId）→
/// 按 rpcId 路由 pending → 校验 sessionId+approvalId+outcome → 映射 allowed-once/
/// rejected 到执行层决策（真 decide + kick）→ resolve wire + 返回 RpcReceipt。
/// 语义对齐 harness（首次 → accepted:true；迟到 → not-pending；畸形/审计不符 →
/// bad-response）。
///
/// - `wire`：None（未装配，如测试口）→ 任何应答都是 not-pending（无决可答）。
/// - `decide`：`(call_id, decision) -> Result<usize, String>`——真决策侧，Err 视为
///   状态不一致（bad-response，不 resolve，不伪装 accepted）。
pub fn approval_respond(
    wire: Option<&ApprovalWireRef>,
    body: &[u8],
    mut decide: impl FnMut(&str, &str) -> Result<usize, String>,
) -> Value {
    let Ok(msg) = serde_json::from_slice::<Value>(body) else {
        return receipt(false, "bad-response");
    };
    // 信封判别：`{type:"client-response", rpcId, result}`（对齐 rpcMessageSchema）。
    if msg.get("type").and_then(Value::as_str) != Some("client-response") {
        return receipt(false, "bad-response");
    }
    let Some(rpc_id) = msg.get("rpcId").and_then(Value::as_str) else {
        return receipt(false, "bad-response");
    };
    let Some(wire) = wire else {
        return receipt(false, "not-pending");
    };
    let Some((session_id, approval_id, call_id)) = wire.pending_by_rpc(rpc_id) else {
        return receipt(false, "not-pending");
    };
    // `result` 槽：`{ok:true, value:{sessionId, approvalId, outcome}}`。
    let Some(result) = msg.get("result") else {
        return receipt(false, "bad-response");
    };
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or_default();
    let Some(value) = result.get("value") else {
        return receipt(false, "bad-response");
    };
    // 审计相关校验收紧（不符 = 畸形，非迟到）。
    let value_ok = value.get("sessionId").and_then(Value::as_str) == Some(session_id.as_str())
        && value.get("approvalId").and_then(Value::as_str) == Some(approval_id.as_str());
    // outcome 只接受 client 能答的两个字面量；其余（含 host 侧字面量）→ bad-response。
    let decision = match value.get("outcome").and_then(Value::as_str) {
        Some(WIRE_OUTCOME_ALLOWED_ONCE) => crate::web::approval::DECISION_ALLOWED_ONCE,
        Some(WIRE_OUTCOME_REJECTED) => crate::web::approval::DECISION_REJECTED,
        _ => return receipt(false, "bad-response"),
    };
    if !ok || !value_ok {
        return receipt(false, "bad-response");
    }
    if decide(&call_id, decision).is_err() {
        return receipt(false, "bad-response");
    }
    let _ = wire.resolve_by_rpc(rpc_id, outcome_of(decision));
    json!({ "accepted": true })
}

/// 决策 → wire outcome 字面量（只有 accepted 路径走到）。
fn outcome_of(decision: &str) -> &str {
    match decision {
        crate::web::approval::DECISION_ALLOWED_ONCE => WIRE_OUTCOME_ALLOWED_ONCE,
        crate::web::approval::DECISION_REJECTED => WIRE_OUTCOME_REJECTED,
        _ => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::approval::{DECISION_ALLOWED_ONCE, DECISION_REJECTED};

    fn noop_decide(_call_id: &str, _decision: &str) -> Result<usize, String> {
        Ok(0)
    }

    fn requested_payload(msg: &Value) -> Value {
        msg["payload"].clone()
    }

    #[test]
    fn push_mints_stable_rpc_id_and_requested_frame() {
        let wire = ApprovalWire::new();
        let rpc1 = wire.push_requested("default", "call-1", "bash", Some("mutates"));
        let rpc2 = wire.push_requested("default", "call-2", "write", None);
        assert_eq!(rpc1, "approval-0");
        assert_eq!(rpc2, "approval-1");
        assert_ne!(rpc1, rpc2, "每 ask 唯一 rpcId");
        let frames = wire.frames_since(0).1;
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "server-request");
        assert_eq!(frames[0]["method"], "approval/requested");
        assert_eq!(frames[0]["rpcId"], "approval-0");
        let p = requested_payload(&frames[0]);
        assert_eq!(p["type"], "approval/requested");
        assert_eq!(p["sessionId"], "default");
        assert_eq!(p["approvalId"], "ap-call-1");
        assert_eq!(p["toolName"], "bash");
        assert_eq!(p["callId"], "call-1");
        assert_eq!(p["reason"], "mutates");
        // 无 reason → 不出现字段。
        let p2 = requested_payload(&frames[1]);
        assert!(p2.get("reason").is_none());
    }

    #[test]
    fn pending_replay_is_verbatim_same_rpc_id_and_resolved_is_terminal() {
        let wire = ApprovalWire::new();
        let rpc = wire.push_requested("s1", "c1", "bash", None);
        let replay = wire.pending_requests();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0], wire.frames_since(0).1[0], "重放逐字（同 rpcId）");
        assert_eq!(replay[0]["rpcId"], rpc);
        assert!(wire.resolve_by_rpc(&rpc, WIRE_OUTCOME_ALLOWED_ONCE));
        assert!(wire.pending_requests().is_empty(), "resolve 后不再是 pending");
        assert!(!wire.resolve_by_rpc(&rpc, WIRE_OUTCOME_ALLOWED_ONCE), "二次 settle 命中失败");
        let frames = wire.frames_since(0).1;
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["method"], "approval/resolved");
        let p = requested_payload(&frames[1]);
        assert_eq!(p["type"], "approval/resolved");
        assert_eq!(p["approvalId"], "ap-c1");
        assert_eq!(p["outcome"], "allowed-once");
    }

    #[test]
    fn resolve_by_call_id_keeps_legacy_decide_path_consistent() {
        let wire = ApprovalWire::new();
        let rpc = wire.push_requested("s1", "c1", "bash", None);
        assert!(!wire.resolve_by_call_id("nope", WIRE_OUTCOME_REJECTED));
        assert!(wire.resolve_by_call_id("c1", WIRE_OUTCOME_REJECTED));
        assert!(wire.pending_requests().is_empty());
        let frames = wire.frames_since(0).1;
        assert_eq!(frames[1]["payload"]["outcome"], "rejected");
        let _ = rpc;
    }

    #[test]
    fn frames_since_is_incremental_in_append_order() {
        let wire = ApprovalWire::new();
        let r = wire.push_requested("s1", "c1", "bash", None);
        let (_n, first) = wire.frames_since(0);
        assert_eq!(first.len(), 1);
        let _ = wire.resolve_by_rpc(&r, WIRE_OUTCOME_REJECTED);
        let (cursor, after) = wire.frames_since(1);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["payload"]["type"], "approval/resolved");
        let (_, none) = wire.frames_since(cursor);
        assert!(none.is_empty(), "游标到末尾后无增量");
    }

    #[test]
    fn respond_accepts_echoed_rpc_and_maps_to_decide() {
        let wire = ApprovalWireRef::new(ApprovalWire::new());
        let rpc = wire.push_requested("default", "call-9", "write", None);
        let mut decision: Option<(String, String)> = None;
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": rpc,
            "result": { "ok": true, "value": {
                "sessionId": "default",
                "approvalId": "ap-call-9",
                "outcome": "allowed-once",
            }},
        }))
        .unwrap();
        let rec = approval_respond(Some(&wire), &body, |cid, d| {
            decision = Some((cid.to_string(), d.to_string()));
            Ok(0)
        });
        assert_eq!(rec, json!({ "accepted": true }));
        assert_eq!(decision, Some(("call-9".into(), DECISION_ALLOWED_ONCE.into())));
        // accepted 后 wire 已 resolve → 同 rpcId 再答 = not-pending。
        let again = approval_respond(Some(&wire), &body, noop_decide);
        assert_eq!(again, json!({ "accepted": false, "reason": "not-pending" }));
    }

    #[test]
    fn respond_rejects_unknown_and_malformed() {
        let wire = ApprovalWireRef::new(ApprovalWire::new());
        let _rpc = wire.push_requested("default", "c1", "bash", None);
        // 未知 rpcId → not-pending。
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": "approval-99",
            "result": { "ok": true, "value": { "sessionId": "default", "approvalId": "ap-c1", "outcome": "allowed-once" } },
        }))
        .unwrap();
        assert_eq!(
            approval_respond(Some(&wire), &body, noop_decide),
            json!({ "accepted": false, "reason": "not-pending" })
        );
        // 非 client-response 信封 → bad-response。
        let bad = serde_json::to_vec(&json!({ "rpcId": "approval-0" })).unwrap();
        assert_eq!(
            approval_respond(Some(&wire), &bad, noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
        // 非 JSON → bad-response。
        assert_eq!(
            approval_respond(Some(&wire), b"not-json", noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
    }

    #[test]
    fn respond_validates_audit_correlation_and_outcome() {
        let wire = ApprovalWireRef::new(ApprovalWire::new());
        let rpc = wire.push_requested("default", "c1", "bash", None);
        let rp = |approval_id: &str, outcome: &str| {
            serde_json::to_vec(&json!({
                "type": "client-response",
                "rpcId": rpc,
                "result": { "ok": true, "value": {
                    "sessionId": "default",
                    "approvalId": approval_id,
                    "outcome": outcome,
                }},
            }))
            .unwrap()
        };
        // approvalId 不符 → bad-response（审计相关错配 = 畸形，非迟到）。
        assert_eq!(
            approval_respond(Some(&wire), &rp("ap-other", "allowed-once"), noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
        // sessionId 不符 → bad-response。
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": rpc,
            "result": { "ok": true, "value": { "sessionId": "other", "approvalId": "ap-c1", "outcome": "allowed-once" } },
        }))
        .unwrap();
        assert_eq!(
            approval_respond(Some(&wire), &body, noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
        // 非法 outcome → bad-response。
        assert_eq!(
            approval_respond(Some(&wire), &rp("ap-c1", "cancelled"), noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
        // result.ok=false → bad-response。
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": rpc,
            "result": { "ok": false, "error": { "code": "x", "message": "y" } },
        }))
        .unwrap();
        assert_eq!(
            approval_respond(Some(&wire), &body, noop_decide),
            json!({ "accepted": false, "reason": "bad-response" })
        );
        // reject 正常路径（剩余 :decide 尚未调用才到得了这里——上面的 bad 都未消耗 pending）。
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": rpc,
            "result": { "ok": true, "value": { "sessionId": "default", "approvalId": "ap-c1", "outcome": "rejected" } },
        }))
        .unwrap();
        let rec = approval_respond(Some(&wire), &body, |cid, d| {
            assert_eq!(d, DECISION_REJECTED);
            let _ = cid;
            Ok(0)
        });
        assert_eq!(rec, json!({ "accepted": true }));
    }

    #[test]
    fn respond_on_decide_failure_is_bad_response_and_leaves_pending() {
        let wire = ApprovalWireRef::new(ApprovalWire::new());
        let rpc = wire.push_requested("default", "c1", "bash", None);
        let body = serde_json::to_vec(&json!({
            "type": "client-response",
            "rpcId": rpc,
            "result": { "ok": true, "value": { "sessionId": "default", "approvalId": "ap-c1", "outcome": "allowed-once" } },
        }))
        .unwrap();
        let rec = approval_respond(Some(&wire), &body, |_, _| Err("no host".into()));
        assert_eq!(rec, json!({ "accepted": false, "reason": "bad-response" }));
        assert_eq!(wire.pending_requests().len(), 1, "decide 失败不 resolve、不伪装 accepted");
    }
}

//! 跨界品牌新类型（SharedIds）的行为契约测试。
//!
//! 权威参考：`@deepseek-ai/dsh-brand`（TS）`Branded<B>` = 编译期幻影品牌，
//! 运行时就是普通字符串。Rust 等价 = 对 `String` 的 newtype。
//! 测试断言三类契约：
//! 1. 透传语义：`raw()` 原样拿回构造时的字符串（零运行时校验/转换）；
//! 2. 身份语义：不同品牌类型之间不相等（哪怕字符串相同），同类型按字符串比较；
//! 3. wire 语义：serde 序列化为普通 JSON 字符串（与 TS 品牌在 wire 上一致），
//!    反序列化回相同值。

use dsh_brand::{
    AttachmentIdType, CallId, MessageId, ProviderRequestId, ReasoningEffortId, RpcId, SessionId,
    WorkspaceId,
};

#[test]
fn session_id_roundtrips_raw_value() {
    let id = SessionId("s_123".into());
    assert_eq!(id.raw(), "s_123");
    assert_eq!(SessionId::from_raw("s_123"), id);
}

#[test]
fn brand_identity_is_nominal_not_structural() {
    // 名义类型（nominal typing）：不同品牌无法互相比较/赋值（编译期保证）。
    // 同名品牌按字符串比较；`raw()` 证明共享同一底层字符串不被误认为同一标识。
    assert_eq!(SessionId("abc".into()), SessionId("abc".into()));
    assert_ne!(SessionId("abc".into()), SessionId("abd".into()));
    assert_eq!(
        SessionId("abc".into()).raw(),
        MessageId("abc".into()).raw(),
        "不同品牌可含相同字符串，但类型身份不同"
    );
}

#[test]
fn all_brand_types_serialize_as_plain_strings() {
    let cases: Vec<(String, serde_json::Value)> = vec![
        (serde_json::to_string(&SessionId("sid-1".into())).unwrap(), serde_json::json!("sid-1")),
        (serde_json::to_string(&MessageId("msg-1".into())).unwrap(), serde_json::json!("msg-1")),
        (serde_json::to_string(&CallId("call-1".into())).unwrap(), serde_json::json!("call-1")),
        (
            serde_json::to_string(&ProviderRequestId("req-1".into())).unwrap(),
            serde_json::json!("req-1"),
        ),
        (
            serde_json::to_string(&ReasoningEffortId("effort-1".into())).unwrap(),
            serde_json::json!("effort-1"),
        ),
        (serde_json::to_string(&RpcId("rpc-1".into())).unwrap(), serde_json::json!("rpc-1")),
        (serde_json::to_string(&WorkspaceId("ws-1".into())).unwrap(), serde_json::json!("ws-1")),
        (
            serde_json::to_string(&AttachmentIdType("att-1".into())).unwrap(),
            serde_json::json!("att-1"),
        ),
    ];
    for (encoded, expected) in cases {
        assert_eq!(serde_json::from_str::<serde_json::Value>(&encoded).unwrap(), expected);
    }
}

#[test]
fn brand_types_deserialize_from_plain_strings() {
    let sid: SessionId = serde_json::from_str(r#""sid-2""#).unwrap();
    assert_eq!(sid.raw(), "sid-2");
    let call: CallId = serde_json::from_str(r#""call-2""#).unwrap();
    assert_eq!(call.raw(), "call-2");
    let rpc: RpcId = serde_json::from_str(r#""rpc-2""#).unwrap();
    assert_eq!(rpc.raw(), "rpc-2");
}

#[test]
fn display_and_hash_for_brand_types() {
    // 供断言/日志/HashMap 使用：Display = 原始字符串；Hash 按字符串内容。
    assert_eq!(SessionId("k".into()).to_string(), "k");
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(SessionId("a".into()));
    set.insert(SessionId("a".into()));
    assert_eq!(set.len(), 1);
}

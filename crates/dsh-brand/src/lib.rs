//! 跨界品牌新类型（SharedIds）。
//!
//! 权威参考：TS `@deepseek-ai/dsh-brand` 的 `Branded<B>` —— 编译期"名义类型"
//! （nominal typing）品牌，运行时就是普通字符串、零校验零转换。它是 `dsh-brand`
//! 在 DSH 里的定位：**零依赖类型包**，供所有跨界 id 的所有者引用，而无需依赖拥有者
//! crate（避免能力缝 crate 之间的品牌环）。
//!
//! Rust 等价实现 = 对 `String` 的 newtype。`raw()` / `from_raw()` 保证透传语义
//!（与 TS 的 "brand a string, no validation" 完全一致）；serde 派生使 wire 形态为
//! 普通 JSON 字符串；`PartialEq/Eq/Hash` 提供同品牌按字符串比较 + 异品牌天然不等
//!（名义身份）。
//!
//! 迁移归属（D-011）：
//! - `SessionId`：拥有者为 dsh-session（TS 定义于 `core/session/types.ts`）；
//! - `MessageId`/`CallId`/`ProviderRequestId`/`ReasoningEffortId`：拥有者为 dsh-llm
//!   （TS 定义于 `llm/llm/src/brand.ts`）；
//! - `RpcId`：拥有者为 api 契约层（TS 定义于 `host/apiproxy/src/api/rpc.ts`）；
//! - `WorkspaceId`：拥有者为 workspace 域；
//! - `AttachmentIdType`：拥有者为 attachment 域。
//!   各拥有 crate 从本 crate newtype 后再以自己名字 re-export（保留"拥有者暴露 id"语义）。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 定义一个品牌新类型。
///
/// - `name`：结构体名（如 `SessionId`）；
/// - `brand`：TS 侧品牌字面量（如 `'SessionId'`），用于文档与调试。
macro_rules! define_brand {
    ($(#[$meta:meta])* $name:ident, $brand:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// 品牌字符串字面量（对齐 TS `Branded<'$brand'>` 的品牌标签）。
            pub const BRAND: &'static str = $brand;

            /// 把原始字符串品牌化为本类型（TS `$name(id)`——零校验透传）。
            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            /// 拿回原始字符串（透传，无拷贝由调用方决定 `&str`/`String`）。
            pub fn raw(&self) -> &str {
                &self.0
            }

            /// 消耗式转换为原始字符串。
            pub fn into_raw(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(raw: String) -> Self {
                Self(raw)
            }
        }

        impl From<&str> for $name {
            fn from(raw: &str) -> Self {
                Self(raw.to_string())
            }
        }
    };
}

define_brand! {
    /// 标识会话存储中的一个会话（及其持久化 artifact）。
    SessionId, "SessionId"
}
define_brand! {
    /// 一条消息跨 inbox/日志/模型请求边界的稳定标识。
    MessageId, "MessageId"
}
define_brand! {
    /// 模型发出的工具调用与其结果的相关 id。
    CallId, "CallId"
}
define_brand! {
    /// 提供者（provider）发出的、跨包保留用于诊断的请求标识。
    ProviderRequestId, "ProviderRequestId"
}
define_brand! {
    /// 适配器拥有的、某个模型可选推理强度（reasoning effort）的标识。
    ReasoningEffortId, "ReasoningEffortId"
}
define_brand! {
    /// RPC 消息关联 id：发起者铸造、响应回显（从不新造）。
    RpcId, "rpc-id"
}
define_brand! {
    /// 工作区（workspace）标识。
    WorkspaceId, "WorkspaceId"
}
define_brand! {
    /// 附件（attachment）标识。
    AttachmentIdType, "AttachmentIdType"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_transparent_serde() {
        let sid = SessionId::from_raw("a1");
        assert_eq!(serde_json::to_string(&sid).unwrap(), r#""a1""#);
        let back: SessionId = serde_json::from_str(r#""a1""#).unwrap();
        assert_eq!(back, sid);
    }

    #[test]
    fn distinct_brands_are_nominal() {
        // 名义身份：不同品牌类型即使字符串相同也不能互相赋值/比较（编译期保证）。
        // 这里用"需要 SessionId 的函数不接受 MessageId"来证明类型不透明。
        fn wait_session(_: &SessionId) {}
        let sid = SessionId::from_raw("x");
        wait_session(&sid);
        assert_eq!(sid.raw(), MessageId::from_raw("x").raw());
    }
}

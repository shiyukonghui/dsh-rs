//! scoped-dispatch 不变量（对齐 TS `packages/core/scope/src/invariant.ts` +
//! `scoped-events.generated.ts`）。
//!
//! 伴生插件在 Rust 侧以纯函数 `check_scoped_dispatch` 承载（供派发边界调用）：
//! 对每个 scope-filtered 事件，先要求派发带 carrier，再要求 carrier key 与载荷
//! subject 同一对象。`scoped-events.generated.ts` 的 26 事件表是**生成产物**，
//! 此处逐字复刻（19 个 `args[0]['agent']`、1 个 `args[1]['scope']`、
//! 6 个 presence-only null）。

/// 包名常量（对齐 invariant 插件 `PACKAGE_NAME`）。
pub const PACKAGE_NAME: &str = "@deepseek-ai/dsh-scope";

/// 事件载荷 subject 解析器种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectResolver {
    /// `args[0]['agent']`
    AgentAt0,
    /// `args[1]['scope']`
    ScopeAt1,
    /// presence-only：只检查 carrier 存在，不比对一个载荷 subject。
    PresenceOnly,
}

/// 生成表（`scoped-events.generated.ts` 逐字复刻）。
pub const SCOPED_EVENTS: [(&str, SubjectResolver); 26] = [
    ("agent/created", SubjectResolver::AgentAt0),
    ("agent/disposed", SubjectResolver::AgentAt0),
    ("agent/error", SubjectResolver::AgentAt0),
    ("agent/inbox/claimed", SubjectResolver::AgentAt0),
    ("agent/inbox/discarded", SubjectResolver::AgentAt0),
    ("agent/inbox/inserted", SubjectResolver::AgentAt0),
    ("agent/pre-step", SubjectResolver::AgentAt0),
    ("agent/request", SubjectResolver::AgentAt0),
    ("agent/request-error", SubjectResolver::AgentAt0),
    ("agent/session-start", SubjectResolver::AgentAt0),
    ("agent/status", SubjectResolver::AgentAt0),
    ("agent/turn-stopping", SubjectResolver::AgentAt0),
    ("approval/request", SubjectResolver::AgentAt0),
    ("goal/changed", SubjectResolver::AgentAt0),
    ("tools/code-dispatch-log", SubjectResolver::AgentAt0),
    ("tools/execute", SubjectResolver::AgentAt0),
    ("tools/post-execute", SubjectResolver::AgentAt0),
    ("tools/pre-execute", SubjectResolver::AgentAt0),
    ("tools/result", SubjectResolver::AgentAt0),
    ("system-prompt/assemble", SubjectResolver::ScopeAt1),
    ("session/created", SubjectResolver::PresenceOnly),
    ("session/disposed", SubjectResolver::PresenceOnly),
    ("session/event", SubjectResolver::PresenceOnly),
    ("session/flush", SubjectResolver::PresenceOnly),
    ("subagent/end", SubjectResolver::PresenceOnly),
    ("subagent/start", SubjectResolver::PresenceOnly),
];

/// 取事件名对应的 subject 解析器：表内 → 有 resolver / presence-only；
/// 表外 → `None`（非 scope-filtered 事件，不检查）。
pub fn scoped_subject_resolver_for(event: &str) -> Option<SubjectResolver> {
    SCOPED_EVENTS
        .iter()
        .find(|(name, _)| *name == event)
        .map(|(_, r)| *r)
}

/// 无 carrier 的 invariant 失败消息（逐字：`—` 为 U+2014 em dash）。
pub fn no_carrier_message(event: &str) -> String {
    format!(
        "\"{event}\" is a scope-filtered event but was dispatched without a scope carrier — pass scopeTarget(base, subject) as the dispatch thisArg (agent events: use agentEvents(ctx, agent))"
    )
}

/// carrier key 与载荷 subject 不一致的 invariant 失败消息（逐字：`event's` 的
/// `'` 为 U+2019 右单引号）。
pub fn mismatched_subject_message(event: &str) -> String {
    format!(
        "\"{event}\" was dispatched with a scope carrier keyed to a DIFFERENT subject than its arguments name — the carrier key and the event's subject must be the same object (use agentEvents(ctx, agent))"
    )
}

/// 从 args 取载荷 subject（对齐 resolver）。
fn subject_of(resolver: SubjectResolver, args: &[serde_json::Value]) -> Option<&serde_json::Value> {
    match resolver {
        SubjectResolver::AgentAt0 => args.first().and_then(|a| a.get("agent")),
        SubjectResolver::ScopeAt1 => args.get(1).and_then(|a| a.get("scope")),
        SubjectResolver::PresenceOnly => None,
    }
}

/// 本包内部的载体判定：`key_carrier: Option<()>` 表示是否存在 carrier。
/// 现场 api 由上层（dsh-agent 等）的派发点持有 key → subject 判定。这里提供
/// 与 TS 等价的组合判定：
/// - 无 carrier → Err(no-caller)
/// - presence-only → Ok（不比较 subject）
/// - 有 subject resolver → carrier key 与载荷 subject 相等才 Ok，否则 Err(mismatch)
///
/// `carrier_key` 为派发 carrier 的路由键（`ScopeKey` 身份）。载荷 subject 以
/// `Value` 传入并按其字符串 id 与 `carrier_key` 的可串化身份比较——Rust 侧
/// dsh-agent 把同一 `Rc` 句柄同时用作 key 与 agent 身份，故此处比较由调用方
/// 提供 `subject_matches: impl Fn(&serde_json::Value) -> bool` 更稳妥；本函数
/// 仅负责「无 carrier / presence-only / resolver 选择」的结构。
pub fn check_scoped_dispatch<F>(
    event: &str,
    args: &[serde_json::Value],
    carrier_present: bool,
    subject_matches: F,
) -> Result<(), String>
where
    F: Fn(&serde_json::Value) -> bool,
{
    let resolver = match scoped_subject_resolver_for(event) {
        Some(r) => r,
        None => return Ok(()), // 非 scope-filtered
    };
    // 检查 1：carrier 存在
    if !carrier_present {
        return Err(no_carrier_message(event));
    }
    // 检查 2：key 与载荷 subject 一致（presence-only 跳过）
    if resolver == SubjectResolver::PresenceOnly {
        return Ok(());
    }
    match subject_of(resolver, args) {
        Some(subject) if subject_matches(subject) => Ok(()),
        _ => Err(mismatched_subject_message(event)),
    }
}

//! `dsh-subagent` 描述符 —— 对齐 `packages/subagent/subagent/src/descriptor.ts`。
//!
//! durable `subagent/descriptor`（版本 2）标识 each session-backed subagent 并记录
//! one-shot / continuable；continuable 保留冷 resume 所需组合。
//! - `snapshot_descriptor`：校验 + 键集合约束 + lostless-JSON 边界（前置于创建）。
//! - `fold_descriptor_from_events`：首条 descriptor 权威；版本不符 → None；当前版本
//!   但未知字段/类型错 → fail loud。

use serde::Serialize;

/// 当前描述符格式版本。
pub const SUBAGENT_DESCRIPTOR_VERSION: u64 = 2;

/// 工具限制（`ToolRestriction { allow?, deny? }`，至少一个）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolRestriction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
}

/// 校验后的子代理描述符（durable payload）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Descriptor {
    #[serde(rename = "one-shot")]
    OneShot {
        version: u64,
        provider: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    #[serde(rename = "continuable")]
    Continuable {
        version: u64,
        provider: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_filter: Option<ToolRestriction>,
    },
}

/// 描述符构造输入（caller 在创建前收集的组合字段）。
#[derive(Debug, Clone)]
pub enum DescriptorInput {
    OneShot {
        mode: String,
        provider: String,
        label: Option<String>,
    },
    Continuable {
        mode: String,
        provider: String,
        label: String,
        agent_provider: Option<String>,
        agent_model: Option<String>,
        persona: Option<String>,
        tool_filter: Option<ToolRestriction>,
    },
}

fn is_record(v: &serde_json::Value) -> bool {
    v.is_object()
}

fn has_key(v: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    v.contains_key(key)
}

/// 当前版本但带未知字段 → Err（fail loud）。
fn assert_known_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<(), String> {
    if let Some(unknown) = obj.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("persisted subagent descriptor payload has unknown field \"{unknown}\""));
    }
    Ok(())
}

/// 校验并快照一个描述符输入。
pub fn snapshot_descriptor(input: &DescriptorInput) -> Result<Descriptor, String> {
    match input {
        DescriptorInput::OneShot { mode, provider, label } => {
            if mode != "one-shot" {
                return Err("one-shot input requires mode 'one-shot'".into());
            }
            validate_provider(provider)?;
            Ok(Descriptor::OneShot {
                version: SUBAGENT_DESCRIPTOR_VERSION,
                provider: provider.clone(),
                label: label.clone(),
            })
        }
        DescriptorInput::Continuable { mode, provider, label, agent_provider, agent_model, persona, tool_filter } => {
            if mode != "continuable" {
                return Err("continuable input requires mode 'continuable'".into());
            }
            validate_provider(provider)?;
            if label.is_empty() {
                return Err("continuable descriptor requires a non-empty label".into());
            }
            if persona.is_some() && persona.as_ref().is_some_and(|p| p.is_empty()) {
                return Err("continuable descriptor persona must be non-empty".into());
            }
            Ok(Descriptor::Continuable {
                version: SUBAGENT_DESCRIPTOR_VERSION,
                provider: provider.clone(),
                label: label.clone(),
                agent_provider: agent_provider.clone(),
                agent_model: agent_model.clone(),
                persona: persona.clone(),
                tool_filter: tool_filter.clone(),
            })
        }
    }
}

fn validate_provider(provider: &str) -> Result<(), String> {
    if provider.is_empty() {
        return Err("subagent descriptor provider must be a non-empty string".into());
    }
    Ok(())
}

/// 解析一条持久化 descriptor payload（当前版本且完整 schema → Some；版本不符 → None；
/// 当前版本但结构坏 → Err）。
fn parse_descriptor(value: &serde_json::Value) -> Result<Option<Descriptor>, String> {
    if !is_record(value) {
        return Err("persisted subagent descriptor payload must be an object".into());
    }
    let obj = value.as_object().expect("is_object");
    let version = obj
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "persisted subagent descriptor version must be a number".to_string())?;
    if version != SUBAGENT_DESCRIPTOR_VERSION {
        return Ok(None);
    }
    let mode = obj.get("mode").and_then(|v| v.as_str()).ok_or_else(|| {
        "persisted subagent descriptor mode must be \"one-shot\" or \"continuable\"".to_string()
    })?;
    let provider = obj
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "persisted subagent descriptor provider must be a string".to_string())?
        .to_string();
    match mode {
        "one-shot" => {
            assert_known_keys(obj, &["version", "mode", "provider", "label"])?;
            let label = match obj.get("label") {
                None => None,
                Some(v) => Some(
                    v.as_str()
                        .ok_or_else(|| "persisted subagent descriptor label must be a string".to_string())?
                        .to_string(),
                ),
            };
            Ok(Some(Descriptor::OneShot {
                version,
                provider,
                label,
            }))
        }
        "continuable" => {
            assert_known_keys(obj, &["version", "mode", "provider", "label", "agentProvider", "agentModel", "persona", "toolFilter"])?;
            let label = obj
                .get("label")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "persisted subagent descriptor label must be a string".to_string())?
                .to_string();
            let agent_provider = optional_string(obj, "agentProvider")?;
            let agent_model = optional_string(obj, "agentModel")?;
            let persona = optional_string(obj, "persona")?;
            let tool_filter = match obj.get("toolFilter") {
                None => None,
                Some(v) => Some(parse_tool_filter(v)?),
            };
            Ok(Some(Descriptor::Continuable {
                version,
                provider,
                label,
                agent_provider,
                agent_model,
                persona,
                tool_filter,
            }))
        }
        other => Err(format!(
            "persisted subagent descriptor mode must be \"one-shot\" or \"continuable\", got \"{other}\""
        )),
    }
}

fn optional_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    if !has_key(obj, key) {
        return Ok(None);
    }
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .map(Some)
        .ok_or_else(|| format!("persisted subagent descriptor {key} must be a string"))
}

fn parse_tool_filter(value: &serde_json::Value) -> Result<ToolRestriction, String> {
    if !is_record(value) {
        return Err("persisted subagent descriptor toolFilter must be an object".into());
    }
    let obj = value.as_object().expect("is_object");
    assert_known_keys(obj, &["allow", "deny"])?;
    let allow = match obj.get("allow") {
        None => None,
        Some(v) => Some(optional_string_array(v, "allow")?),
    };
    let deny = match obj.get("deny") {
        None => None,
        Some(v) => Some(optional_string_array(v, "deny")?),
    };
    if allow.is_none() && deny.is_none() {
        return Err("persisted subagent descriptor toolFilter must declare allow and/or deny".into());
    }
    Ok(ToolRestriction { allow, deny })
}

fn optional_string_array(value: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    let arr = value
        .as_array()
        .ok_or_else(|| format!("persisted subagent descriptor toolFilter.{key} must be an array of strings"))?;
    arr.iter()
        .map(|item| {
            item.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| format!("persisted subagent descriptor toolFilter.{key} must be an array of strings"))
        })
        .collect()
}

/// 从子代理会话事件折叠出描述符。首条 `subagent/descriptor` 权威；无/版本不符 → None；
/// 当前版本但结构坏 → Err。
pub fn fold_descriptor_from_events(events: &[serde_json::Value]) -> Result<Option<Descriptor>, String> {
    for event in events {
        if event.get("type").and_then(|t| t.as_str()) == Some("subagent/descriptor") {
            let data = event
                .get("data")
                .ok_or_else(|| "subagent/descriptor event missing data".to_string())?;
            return parse_descriptor(data);
        }
    }
    Ok(None)
}

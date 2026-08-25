//! M2c: dsh-system-prompt 行为测试（移植 system-prompt.spec / scoped.spec /
//! tool-order.spec / invariant.spec 的可观察行为；消息逐字）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dsh_llm::ToolSchema;
use dsh_scope::{bind_scope_parent, ScopeKey, Undo};
use dsh_system_prompt::{
    render_context_snapshot, render_prompt, validate_tool_order, AssembleContext,
    AssembledContext, AssembledSection, Config, PromptAssembly, PromptContext, PromptContextText,
    PromptSection, PromptSectionText, SystemPrompt, ToolProviderResult, PERSONA_ORDER,
    PERSONA_SECTION, TOOL_ORDER_REST,
};

fn sec(name: &str, order: f64, text: &str) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        order,
        text: PromptSectionText::Static(text.to_string()),
        complete: false,
    }
}

fn ctx(name: &str, order: f64, text: &str) -> PromptContext {
    PromptContext {
        name: name.to_string(),
        order,
        text: PromptContextText::Static(text.to_string()),
    }
}

fn new_sp(cfg: Config) -> Rc<SystemPrompt> {
    let changes = Rc::new(Cell::new(0usize));
    let notify = changes.clone();
    Rc::new(SystemPrompt::new(&cfg, Rc::new(move || notify.set(notify.get() + 1))).unwrap())
}

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

fn var(sp: &SystemPrompt, name: &str, value: &str) -> Undo {
    let v = value.to_string();
    sp.variable(None, name, Rc::new(move |_| Some(v.clone())))
        .unwrap()
}

const IDENTITY: &str = "You are an AI agent powered by DeepSeek Harness.";

// ---------------------------------------------------------------------------
// 内置 + render
// ---------------------------------------------------------------------------

#[test]
fn built_ins_and_default_render() {
    let sp = new_sp(Config::default());
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(a.sections.len(), 2);
    assert_eq!(render_prompt(&a).unwrap(), IDENTITY);
    // 无上下文 → 空
    assert_eq!(render_context_snapshot(&a).unwrap(), "");
}

#[test]
fn persona_is_second_section_and_composes() {
    let cfg = Config {
        persona: "You are DeepSeek Harness.".to_string(),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        render_prompt(&a).unwrap(),
        format!("{IDENTITY}\n\nYou are DeepSeek Harness.")
    );
}

#[test]
fn include_harness_identity_false_keeps_only_persona() {
    let cfg = Config {
        include_harness_identity: false,
        persona: "You are DeepSeek Harness.".to_string(),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(render_prompt(&a).unwrap(), "You are DeepSeek Harness.");
}

#[test]
fn sections_and_contexts_are_order_sorted_and_render_exactly() {
    let sp = new_sp(Config {
        persona: "You are DeepSeek Harness.".to_string(),
        ..Config::default()
    });
    sp.section(None, &sec("rules", 10.0, "Be precise.")).unwrap();
    sp.section(None, &sec("cwd", 20.0, "cwd: /tmp")).unwrap();
    sp.context(None, &ctx("earlier", 1.0, "context 1")).unwrap();
    sp.context(None, &ctx("later", 2.0, "context 2")).unwrap();
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a.sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["harness:identity", PERSONA_SECTION, "rules", "cwd"]
    );
    assert_eq!(
        render_prompt(&a).unwrap(),
        format!("{IDENTITY}\n\nYou are DeepSeek Harness.\n\nBe precise.\n\ncwd: /tmp")
    );
    assert_eq!(
        render_context_snapshot(&a).unwrap(),
        "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\ncontext 1\n\ncontext 2"
    );
}

#[test]
fn section_text_provider_evaluated_per_assemble_call() {
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let sp = new_sp(Config::default());
    let provider = PromptSectionText::Fn(Rc::new(move |_| {
        c.set(c.get() + 1);
        format!("call {}", c.get())
    }));
    sp.section(
        None,
        &PromptSection {
            name: "who".to_string(),
            order: 5.0,
            text: provider,
            complete: false,
        },
    )
    .unwrap();
    let a1 = sp.assemble(&AssembleContext::default()).unwrap();
    let a2 = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(a1.sections.iter().find(|s| s.name == "who").unwrap().text, "call 1");
    assert_eq!(a2.sections.iter().find(|s| s.name == "who").unwrap().text, "call 2");
}

#[test]
fn include_runtime_context_false_suppresses_and_skips_provider() {
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let sp = new_sp(Config {
        include_runtime_context: false,
        ..Config::default()
    });
    let provider = PromptContextText::Fn(Rc::new(move |_| {
        c.set(c.get() + 1);
        "should never be evaluated".to_string()
    }));
    sp.context(
        None,
        &PromptContext {
            name: "dyn".to_string(),
            order: 0.0,
            text: provider,
        },
    )
    .unwrap();
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert!(a.contexts.is_empty());
    assert_eq!(calls.get(), 0);
}

// ---------------------------------------------------------------------------
// 注册校验
// ---------------------------------------------------------------------------

#[test]
fn duplicate_and_invalid_registrations_fail() {
    let sp = new_sp(Config::default());
    let err = sp.section(None, &sec(PERSONA_SECTION, 1.0, "x")).err().unwrap();
    assert_eq!(
        err,
        format!(
            "prompt section \"{PERSONA_SECTION}\" is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
        )
    );
    sp.context(None, &ctx("c", 1.0, "x")).unwrap();
    let err = sp.context(None, &ctx("c", 2.0, "y")).err().unwrap();
    assert_eq!(
        err,
        "prompt context \"c\" is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
    );
    let err = sp.variable(None, "Bad", Rc::new(|_: &AssembleContext| None)).err().unwrap();
    assert_eq!(
        err,
        "invalid prompt variable name \"Bad\" (must match /^[a-z][a-z0-9_]*$/)"
    );
    let err = sp.section(None, &sec("inf", f64::NAN, "x")).err().unwrap();
    assert_eq!(err, "prompt section \"inf\" order must be a finite number");
    let err = sp.context(None, &ctx("inf", f64::INFINITY, "x")).err().unwrap();
    assert_eq!(err, "prompt context \"inf\" order must be a finite number");
    // 非有限 order 拒绝且不透传
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert!(!a.sections.iter().any(|s| s.name == "inf"));
}

#[test]
fn scoped_duplicate_uses_scope_message_and_global_shadowing() {
    let sp = new_sp(Config::default());
    let key = ScopeKey::new();
    sp.section(Some(&key), &sec("s", 1.0, "x")).unwrap();
    let err = sp.section(Some(&key), &sec("s", 2.0, "y")).err().unwrap();
    assert_eq!(err, "prompt section \"s\" is already registered in this scope");
    // scoped 遮蔽不占全局：同名全局可注册，且 scoped 组装里 scoped 优先
    sp.section(None, &sec("s", 3.0, "global")).unwrap();
    // 全局重复注册 → agent.ctx 覆写提示
    let err = sp.section(None, &sec("s", 4.0, "dup")).err().unwrap();
    assert_eq!(
        err,
        "prompt section \"s\" is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
    );
    let a = sp
        .assemble(&AssembleContext{
            scope: Some(key.clone()),
            session_id: None,
        })
        .unwrap();
    let s = a.sections.iter().find(|s| s.name == "s").unwrap();
    assert_eq!(s.text, "x", "scoped shadows global in scoped assembly");
    let g = sp.assemble(&AssembleContext::default()).unwrap();
    let gs = g.sections.iter().find(|s| s.name == "s").unwrap();
    assert_eq!(gs.text, "global");
}

#[test]
fn register_and_dispose_each_emit_exactly_one_change() {
    let changes = Rc::new(Cell::new(0usize));
    let n = changes.clone();
    let sp = SystemPrompt::new(&Config::default(), Rc::new(move || n.set(n.get() + 1))).unwrap();
    let before = changes.get();
    let d = sp.context(None, &ctx("c", 1.0, "x")).unwrap();
    assert_eq!(changes.get() - before, 1, "register → one change");
    d();
    assert_eq!(changes.get() - before, 2, "dispose → one change");
    let b2 = changes.get();
    let v = sp.variable(None, "v", Rc::new(|_| None)).unwrap();
    assert_eq!(changes.get() - b2, 1);
    v();
    // dispose 幂等：再次调用不加
    v();
    assert_eq!(changes.get() - b2, 2);
}

// ---------------------------------------------------------------------------
// 工具组装
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::type_complexity)]
fn tools_members_snapshot_between_assemblies() {
    let registered: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec!["first".to_string()]));
    let names = registered.clone();
    let sp = new_sp(Config::default());
    let provider: Rc<dyn Fn(&AssembleContext) -> ToolProviderResult> =
        Rc::new(move |_: &AssembleContext| {
            let n = names.borrow();
            ToolProviderResult {
                schemas: n.iter().map(|s| tool(s)).collect(),
                known_names: None,
            }
        });
    sp.tools(None, provider);
    let a1 = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a1.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["first"]
    );
    // 运行中新增 provider → 本轮不见、下轮见
    registered.borrow_mut().push("late".to_string());
    let a2 = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a2.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["first", "late"]
    );
}

#[test]
fn tools_lexicographic_without_tool_order() {
    let sp = new_sp(Config::default());
    let names = Rc::new(vec!["charlie", "alpha", "bravo"]);
    let names2 = names.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: names2.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "bravo", "charlie"]
    );
}

#[test]
fn tool_order_applies_rest_marker_and_keeps_stable_order() {
    let cfg = Config {
        tool_order: Some(vec![
            "todo_write".to_string(),
            TOOL_ORDER_REST.to_string(),
            "bash".to_string(),
        ]),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let covered = Rc::new(vec![
        "echo_a".to_string(),
        "bash".to_string(),
        "todo_write".to_string(),
        "echo_b".to_string(),
    ]);
    let c = covered.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: c.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["todo_write", "echo_a", "echo_b", "bash"]
    );
}

#[test]
fn tool_order_typo_reports_singular_and_known_sorted() {
    let cfg = Config {
        tool_order: Some(vec![
            "basj".to_string(),
            TOOL_ORDER_REST.to_string(),
            "bash".to_string(),
        ]),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let covered = Rc::new(vec!["read".to_string(), "bash".to_string()]);
    let c = covered.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: c.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    let err = sp.assemble(&AssembleContext::default()).unwrap_err();
    assert_eq!(
        err,
        "toolOrder lists unregistered tool \"basj\"; known tools: bash, read"
    );
}

#[test]
fn tool_order_plural_unknowns_and_missing_known_is_absent() {
    let cfg = Config {
        tool_order: Some(vec![
            "ghost".to_string(),
            "wraith".to_string(),
            TOOL_ORDER_REST.to_string(),
        ]),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let covered = Rc::new(vec!["bash".to_string(), "todo_write".to_string()]);
    let c = covered.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: c.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    let err = sp.assemble(&AssembleContext::default()).unwrap_err();
    assert_eq!(
        err,
        "toolOrder lists unregistered tools \"ghost\", \"wraith\"; known tools: bash, todo_write"
    );
}

#[test]
fn tool_order_known_but_scoped_absent_is_not_an_error() {
    let cfg = Config {
        tool_order: Some(vec!["bash".to_string(), TOOL_ORDER_REST.to_string()]),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let covered = Rc::new(vec!["read".to_string()]);
    let c = covered.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: c.iter().map(|s| tool(s)).collect(),
            known_names: Some(vec!["bash".to_string(), "read".to_string()]),
        }),
    );
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    // bash 已知但本组装缺席 → 不抛，仅列出 read
    assert_eq!(
        a.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["read"]
    );
}

#[test]
fn tool_provider_reserved_name_fails() {
    let sp = new_sp(Config::default());
    let names = Rc::new(vec![TOOL_ORDER_REST.to_string()]);
    let n = names.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: n.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    let err = sp.assemble(&AssembleContext::default()).unwrap_err();
    assert_eq!(
        err,
        format!(
            "tool provider returned reserved tool name \"{TOOL_ORDER_REST}\" (reserved for toolOrder's rest entry)"
        )
    );
}

#[test]
fn validate_tool_order_load_errors() {
    assert_eq!(
        validate_tool_order(Some(vec![TOOL_ORDER_REST.to_string(), TOOL_ORDER_REST.to_string()]))
            .unwrap_err(),
        format!("toolOrder lists \"{TOOL_ORDER_REST}\" more than once")
    );
    assert_eq!(
        validate_tool_order(Some(vec!["a".to_string()])).unwrap_err(),
        format!(
            "toolOrder must contain the \"{TOOL_ORDER_REST}\" rest entry (where unlisted tools are inserted)"
        )
    );
    // 空列表（显式）→ 缺 rest 标记
    assert!(validate_tool_order(Some(vec![])).is_err());
    // 省略 → 合法
    assert!(validate_tool_order(None).unwrap().is_none());
}

#[test]
fn canonical_order_precedes_waterfall_and_extra_is_appended() {
    let cfg = Config {
        tool_order: Some(vec!["alpha".to_string(), TOOL_ORDER_REST.to_string()]),
        ..Config::default()
    };
    let sp = new_sp(cfg);
    let covered = Rc::new(vec!["zulu".to_string(), "alpha".to_string()]);
    let c = covered.clone();
    sp.tools(
        None,
        Rc::new(move |_| ToolProviderResult {
            schemas: c.iter().map(|s| tool(s)).collect(),
            known_names: None,
        }),
    );
    // 监听器看到已排序 ['alpha','zulu']；随后添加 aardvark 不重排
    sp.register_assemble_listener(None, false, Rc::new(move |mut a, _, next| {
        assert_eq!(
            a.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zulu"]
        );
        a.tools.push(tool("aardvark"));
        next(a)
    }));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "zulu", "aardvark"]
    );
}

#[test]
fn multiple_complete_sections_fail() {
    let sp = new_sp(Config::default());
    let _ = sp
        .section(
            None,
            &PromptSection {
                name: "complete1".to_string(),
                order: 1.0,
                text: PromptSectionText::Static("a".to_string()),
                complete: true,
            },
        )
        .unwrap();
    let _ = sp
        .section(
            None,
            &PromptSection {
                name: "complete2".to_string(),
                order: 2.0,
                text: PromptSectionText::Static("b".to_string()),
                complete: true,
            },
        )
        .unwrap();
    let err = sp.assemble(&AssembleContext::default()).unwrap_err();
    assert_eq!(
        err,
        "multiple complete prompt sections are active: \"complete1\", \"complete2\""
    );
}

// ---------------------------------------------------------------------------
// 水岭
// ---------------------------------------------------------------------------

#[test]
fn waterfall_composes_in_registration_order() {
    let sp = new_sp(Config::default());
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let s1 = seen.clone();
    sp.register_assemble_listener(None, false, Rc::new(move |mut a, _, next| {
        s1.borrow_mut().push("A".to_string());
        a.sections.push(AssembledSection {
            name: "fromA".to_string(),
            text: "A text".to_string(),
        });
        next(a)
    }));
    let s2 = seen.clone();
    sp.register_assemble_listener(None, false, Rc::new(move |a, _, next| {
        s2.borrow_mut().push("B".to_string());
        // B 看到 A 的 section
        assert!(a.sections.iter().any(|s| s.name == "fromA"));
        next(a)
    }));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(*seen.borrow(), vec!["A".to_string(), "B".to_string()]);
    assert!(a.sections.iter().any(|s| s.name == "fromA"));
}

#[test]
fn waterfall_short_circuit_replaces_assembly() {
    let sp = new_sp(Config::default());
    sp.register_assemble_listener(None, false, Rc::new(move |_a, _, _next| {
        // 不调 next → 短路（整体替换）
        Ok(PromptAssembly {
            sections: vec![],
            contexts: vec![],
            tools: vec![],
            variables: vec![],
        })
    }));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert!(a.sections.is_empty());
}

#[test]
fn complete_section_restored_after_waterfall() {
    let sp = new_sp(Config::default());
    sp.section(
        None,
        &PromptSection {
            name: "complete".to_string(),
            order: 5.0,
            text: PromptSectionText::Static("Exact prompt.".to_string()),
            complete: true,
        },
    )
    .unwrap();
    // 水岭试图改写 complete section / 加 section → 均被丢弃
    sp.register_assemble_listener(None, false, Rc::new(move |mut a, _, next| {
        a.sections.push(AssembledSection {
            name: "tamper".to_string(),
            text: "should vanish".to_string(),
        });
        for s in a.sections.iter_mut() {
            if s.name == "complete" {
                s.text = "overwritten".to_string();
                s.name = "renamed".to_string();
            }
        }
        next(a)
    }));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        a.sections,
        vec![AssembledSection {
            name: "complete".to_string(),
            text: "Exact prompt.".to_string(),
        }]
    );
}

#[test]
fn scoped_assemble_listener_only_affects_own_scope() {
    let sp = new_sp(Config::default());
    let s2 = ScopeKey::new();
    // 监听器持有自己的 scope 键副本（key 本身是身份句柄，克隆保留同一身份）
    let listener_key = s2.clone();
    sp.register_assemble_listener(Some(listener_key), false, Rc::new(move |mut a, _, next| {
        a.sections.push(AssembledSection {
            name: "scopedOnly".to_string(),
            text: "scope2".to_string(),
        });
        next(a)
    }));
    let g = sp.assemble(&AssembleContext::default()).unwrap();
    assert!(!g.sections.iter().any(|s| s.name == "scopedOnly"));
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(s2),
            session_id: None,
        })
        .unwrap();
    assert!(sc.sections.iter().any(|s| s.name == "scopedOnly"));
}

// ---------------------------------------------------------------------------
// 无泄漏快照
// ---------------------------------------------------------------------------

#[test]
fn mutating_returned_assembly_does_not_leak() {
    let sp = new_sp(Config::default());
    let mut a1 = sp.assemble(&AssembleContext::default()).unwrap();
    for s in a1.sections.iter_mut() {
        s.text = "tampered".to_string();
    }
    let a2 = sp.assemble(&AssembleContext::default()).unwrap();
    // persona 未泄漏篡改（默认空文本）
    assert!(a2.sections.iter().find(|s| s.name == PERSONA_SECTION).unwrap().text.is_empty());
}

// ---------------------------------------------------------------------------
// 变量与插值
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::type_complexity)]
fn variables_live_iteration_new_registration_visible_same_round() {
    let sp = new_sp(Config::default());
    let register_late: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let done = register_late.clone();
    let sp2 = sp.clone();
    // early provider 首次求值时注册 late —— live 迭代下本轮即见
    let p: Rc<dyn Fn(&AssembleContext) -> Option<String>> =
        Rc::new(move |_: &AssembleContext| {
            if !done.replace(true) {
                let _ = sp2.variable(
                    None,
                    "latevar",
                    Rc::new(|_: &AssembleContext| Some("late".to_string())),
                );
            }
            Some("early".to_string())
        });
    let _ = sp.variable(None, "early", p);
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    let names: Vec<String> = a.variables.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(names, vec!["early".to_string(), "latevar".to_string()]);
    assert!(register_late.get());
}

#[test]
fn variable_undefined_provider_and_render_no_value() {
    let sp = new_sp(Config::default());
    let _ = sp.variable(None, "cwd", Rc::new(|_| None));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    // 已注册但无值 → 存在（own-property）
    assert_eq!(a.variables, vec![("cwd".to_string(), None)]);
    let err = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "persona".to_string(),
            text: "cwd: {{cwd}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap_err();
    assert_eq!(
        err,
        "prompt variable \"{{cwd}}\" has no value for this assembly (section \"persona\")"
    );
}

#[test]
fn interpolation_errors_are_attributed_to_kind_and_name() {
    let sp = new_sp(Config::default());
    let _ = var(&sp, "model", "deepseek");
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    // unknown（单名无逗号）
    let err = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "persona".to_string(),
            text: "{{modle}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap_err();
    assert_eq!(
        err,
        "unknown prompt variable \"{{modle}}\" in section \"persona\"; registered variables: model"
    );
    // context 归属 context kind
    let err2 = render_context_snapshot(&PromptAssembly {
        sections: vec![],
        contexts: vec![AssembledContext {
            name: "policy".to_string(),
            text: "{{modle}}".to_string(),
        }],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap_err();
    assert_eq!(
        err2,
        "unknown prompt variable \"{{modle}}\" in context \"policy\"; registered variables: model"
    );
    // 无变数 → (none)
    let err3 = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "{{missing}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: vec![],
    })
    .unwrap_err();
    assert_eq!(
        err3,
        "unknown prompt variable \"{{missing}}\" in section \"s\"; registered variables: (none)"
    );
}

#[test]
fn malformed_references_and_literal_lone_open_brace() {
    let sp = new_sp(Config::default());
    let _ = var(&sp, "model", "deepseek");
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    let render = |text: &str| {
        render_prompt(&PromptAssembly {
            sections: vec![AssembledSection {
                name: "s".to_string(),
                text: text.to_string(),
            }],
            contexts: vec![],
            tools: vec![],
            variables: a.variables.clone(),
        })
    };
    // 空格 → malformed（名字匹配失败）
    assert_eq!(
        render("{{ model }}").unwrap_err(),
        "malformed prompt variable reference \"{{ model }}\" in section \"s\" (variable names match /^[a-z][a-z0-9_]*$/)"
    );
    // 裸 {{ 后无 }} → 纯散文留字面
    assert_eq!(render("shell ${X:-{{fallback} stays").unwrap(), "shell ${X:-{{fallback} stays");
    // {{{model}}} → malformed generic
    let e = render("{{{model}}}").unwrap_err();
    assert!(e.starts_with("malformed prompt variable reference at \""), "got: {e}");
    assert!(
        e.ends_with("…\" in section \"s\" (references are complete simple {name} groups)"),
        "got: {e}"
    );
    // {{a{b}}…}} → malformed generic（预览 16 码元内完整给出原文 + “…”）
    let e = render("{{a{b}} tail}}").unwrap_err();
    assert_eq!(
        e,
        "malformed prompt variable reference at \"{{a{b}} tail}}…\" in section \"s\" (references are complete simple {name} groups)"
    );
}

#[test]
fn constructor_prototype_poisoned_name_is_unknown_then_registerable() {
    let sp = new_sp(Config::default());
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    let err = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "{{constructor}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap_err();
    assert_eq!(
        err,
        "unknown prompt variable \"{{constructor}}\" in section \"s\"; registered variables: (none)"
    );
    // 注册后可变数插值
    let _ = sp.variable(None, "constructor", Rc::new(|_| Some("v".to_string()))).unwrap();
    let a2 = sp.assemble(&AssembleContext::default()).unwrap();
    let out = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "{{constructor}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a2.variables.clone(),
    })
    .unwrap();
    assert_eq!(out, "v");
}

#[test]
fn substituted_values_are_not_rescanned() {
    let sp = new_sp(Config::default());
    let _ = var(&sp, "sneaky", "{{unregistered}}");
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    let out = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "{{sneaky}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap();
    assert_eq!(out, "{{unregistered}}");
}

#[test]
fn waterfall_can_add_variables_before_render() {
    let sp = new_sp(Config::default());
    sp.register_assemble_listener(None, false, Rc::new(move |mut a, _, next| {
        a.variables.push(("added".to_string(), Some("yes".to_string())));
        next(a)
    }));
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    let out = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "{{added}}".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap();
    assert_eq!(out, "yes");
}

// ---------------------------------------------------------------------------
// scoped 行为
// ---------------------------------------------------------------------------

#[test]
fn scoped_persona_shadows_global_within_scope_only() {
    let sp = new_sp(Config {
        persona: "You are DeepSeek Harness.".to_string(),
        ..Config::default()
    });
    let scope_key = ScopeKey::new();
    sp.section(
        Some(&scope_key),
        &sec(PERSONA_SECTION, PERSONA_ORDER, "You run tests."),
    )
    .unwrap();
    let g = sp.assemble(&AssembleContext::default()).unwrap();
    assert!(render_prompt(&g).unwrap().contains("You are DeepSeek Harness."));
    assert!(!render_prompt(&g).unwrap().contains("You run tests."));
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    let sc_render = render_prompt(&sc).unwrap();
    assert!(sc_render.contains("You run tests."));
    assert!(!sc_render.contains("You are DeepSeek Harness."));
}

#[test]
fn scoped_section_goes_away_after_dispose_and_global_returns() {
    let sp = new_sp(Config {
        persona: "You are DeepSeek Harness.".to_string(),
        ..Config::default()
    });
    let scope_key = ScopeKey::new();
    let d = sp
        .section(
            Some(&scope_key),
            &sec(PERSONA_SECTION, PERSONA_ORDER, "You run tests."),
        )
        .unwrap();
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert!(render_prompt(&sc).unwrap().contains("You run tests."));
    d();
    let sc2 = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert!(render_prompt(&sc2).unwrap().contains("You are DeepSeek Harness."));
}

#[test]
fn scoped_shadowing_wins_before_evaluation() {
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let sp = new_sp(Config::default());
    let scope_key = ScopeKey::new();
    // 全局 "shadowme" 用计数 provider；scoped 同名遮蔽 → 遮蔽先于求值
    let _ = sp
        .section(
            None,
            &PromptSection {
                name: "shadowme".to_string(),
                order: 5.0,
                text: PromptSectionText::Fn(Rc::new(move |_| {
                    c.set(c.get() + 1);
                    "global".to_string()
                })),
                complete: false,
            },
        )
        .unwrap();
    let _ = sp.section(Some(&scope_key), &sec("shadowme", 0.0, "scoped")).unwrap();
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert_eq!(
        sc.sections.iter().find(|s| s.name == "shadowme").unwrap().text,
        "scoped"
    );
    assert_eq!(calls.get(), 0, "global provider must not run under shadowing");
    let g = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(
        g.sections.iter().find(|s| s.name == "shadowme").unwrap().text,
        "global"
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn scoped_variable_shadows_global_and_dispose_restores() {
    let sp = new_sp(Config::default());
    let scope_key = ScopeKey::new();
    let _ = var(&sp, "mode", "normal");
    let d = sp
        .variable(Some(&scope_key), "mode", Rc::new(|_| Some("strict".to_string())))
        .unwrap();
    let a = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    let out = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "Mode: {{mode}}.".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a.variables.clone(),
    })
    .unwrap();
    assert_eq!(out, "Mode: strict.");
    d();
    let a2 = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    let out2 = render_prompt(&PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_string(),
            text: "Mode: {{mode}}.".to_string(),
        }],
        contexts: vec![],
        tools: vec![],
        variables: a2.variables.clone(),
    })
    .unwrap();
    assert_eq!(out2, "Mode: normal.");
}

#[test]
fn scoped_context_shadows_global_and_falls_back() {
    let sp = new_sp(Config::default());
    let scope_key = ScopeKey::new();
    sp.context(None, &ctx("topic", 1.0, "global topic")).unwrap();
    let d = sp.context(Some(&scope_key), &ctx("topic", 2.0, "scoped topic")).unwrap();
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert_eq!(sc.contexts[0].text, "scoped topic");
    d();
    let sc2 = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert_eq!(sc2.contexts[0].text, "global topic");
}

#[test]
fn suppress_runtime_context_is_scope_local() {
    let sp = new_sp(Config::default());
    let scope_key = ScopeKey::new();
    sp.context(None, &ctx("global", 1.0, "g")).unwrap();
    sp.context(Some(&scope_key), &ctx("local", 1.0, "l")).unwrap();
    let suppress = sp.suppress_runtime_context(Some(&scope_key));
    let sc = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    assert!(sc.contexts.is_empty(), "scope contexts suppressed");
    let g = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(g.contexts[0].text, "g", "global unaffected");
    suppress();
    let sc2 = sp
        .assemble(&AssembleContext{
            scope: Some(scope_key.clone()),
            session_id: None,
        })
        .unwrap();
    // 恢复后 scoped 组装 = 全局 g + scoped l（合并视图）
    let names: Vec<String> = sc2.contexts.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, vec!["global".to_string(), "local".to_string()], "restored after dispose");
}

// ---------------------------------------------------------------------------
// 作用域链（父链继承 + 遮蔽）
// ---------------------------------------------------------------------------

#[test]
fn scope_chain_nearest_wins_and_ancestor_visible() {
    let sp = new_sp(Config::default());
    let parent = ScopeKey::new();
    let child = ScopeKey::new();
    bind_scope_parent(child.clone(), parent.clone()).unwrap();
    let _ = sp.variable(Some(&parent), "mode", Rc::new(|_| Some("ancestor".to_string())));
    let a_child = sp
        .assemble(&AssembleContext{
            scope: Some(child.clone()),
            session_id: None,
        })
        .unwrap();
    let names: Vec<&str> = a_child.variables.iter().map(|(k, _)| k.as_str()).collect();
    assert!(names.contains(&"mode"), "ancestor variable visible to child");
    let _ = sp.variable(Some(&child), "mode", Rc::new(|_| Some("nearest".to_string())));
    let a_child2 = sp
        .assemble(&AssembleContext{
            scope: Some(child),
            session_id: None,
        })
        .unwrap();
    let val = a_child2.variables.iter().find(|(k, _)| k == "mode").unwrap().1.clone();
    assert_eq!(val, Some("nearest".to_string()));
}

#[test]
fn scoped_context_shadowing_through_parent_chain() {
    let sp = new_sp(Config::default());
    let root = ScopeKey::new();
    let child = ScopeKey::new();
    bind_scope_parent(child.clone(), root.clone()).unwrap();
    sp.context(Some(&root), &ctx("topic", 1.0, "root")).unwrap();
    sp.context(Some(&child), &ctx("topic", 2.0, "child")).unwrap();
    let a_child = sp.assemble(&AssembleContext{ scope: Some(child) , session_id: None }).unwrap();
    assert_eq!(a_child.contexts.iter().find(|c| c.name == "topic").unwrap().text, "child");
    let a_root = sp.assemble(&AssembleContext{ scope: Some(root) , session_id: None }).unwrap();
    assert_eq!(a_root.contexts.iter().find(|c| c.name == "topic").unwrap().text, "root");
}

// ---------------------------------------------------------------------------
// invariant
// ---------------------------------------------------------------------------

fn assembly_with(
    sections: Vec<(&str, &str)>,
    contexts: Vec<(&str, &str)>,
    tools: Vec<&str>,
    variables: Vec<(String, Option<String>)>,
) -> PromptAssembly {
    PromptAssembly {
        sections: sections
            .into_iter()
            .map(|(n, t)| AssembledSection {
                name: n.to_string(),
                text: t.to_string(),
            })
            .collect(),
        contexts: contexts
            .into_iter()
            .map(|(n, t)| AssembledContext {
                name: n.to_string(),
                text: t.to_string(),
            })
            .collect(),
        tools: tools.into_iter().map(tool).collect(),
        variables,
    }
}

fn validate(assembly: &PromptAssembly) -> Vec<String> {
    let mut out = Vec::new();
    let mut fail = |msg: String| out.push(msg);
    dsh_system_prompt::invariant::validate_assembly(assembly, &mut fail);
    out
}

#[test]
fn invariant_accepts_valid_assembly_with_undefined_variable() {
    let a = assembly_with(
        vec![("s", "text")],
        vec![("c", "t")],
        vec!["t"],
        vec![("v".to_string(), None)],
    );
    assert!(validate(&a).is_empty());
}

#[test]
fn invariant_rejects_invalid_shapes() {
    assert_eq!(
        validate(&assembly_with(vec![("", "t")], vec![], vec![], vec![])),
        vec!["assembled section names must be non-empty".to_string()]
    );
    assert_eq!(
        validate(&assembly_with(vec![("x", "1"), ("x", "2")], vec![], vec![], vec![])),
        vec!["assembled section name \"x\" is duplicated".to_string()]
    );
    assert_eq!(
        validate(&assembly_with(vec![], vec![("", "t")], vec![], vec![])),
        vec!["assembled context names must be non-empty".to_string()]
    );
    assert_eq!(
        validate(&assembly_with(vec![], vec![("x", "1"), ("x", "2")], vec![], vec![])),
        vec!["assembled context name \"x\" is duplicated".to_string()]
    );
    assert_eq!(
        validate(&assembly_with(vec![], vec![], vec![""], vec![])),
        vec!["assembled tool names must be non-empty".to_string()]
    );
    assert_eq!(
        validate(&assembly_with(vec![], vec![], vec![], vec![("Bad".to_string(), None)])),
        vec!["assembled variable name \"Bad\" is invalid".to_string()]
    );
}

#[test]
fn invariant_install_wraps_waterfall_and_validates_authority() {
    let sp = new_sp(Config::default());
    dsh_system_prompt::invariant::install(&sp);
    // 合法组装通过
    let a = sp.assemble(&AssembleContext::default()).unwrap();
    assert_eq!(a.sections.len(), 2);
    // 水岭把权威物改成非法（空 section 名）→ 安装的 invariant 报错
    sp.register_assemble_listener(None, false, Rc::new(move |mut a, _, next| {
        a.sections = vec![AssembledSection {
            name: String::new(),
            text: "x".to_string(),
        }];
        next(a)
    }));
    let err = sp.assemble(&AssembleContext::default()).unwrap_err();
    assert!(err.contains("assembled section names must be non-empty"), "got: {err}");
}

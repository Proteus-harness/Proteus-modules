use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use proteus_contracts::{
    contracts::ExecutionAttribution,
    domain::{ToolCall, ToolCallSurface, ToolResult, new_execution_id},
    process_module::{
        ProcessModuleError, ProcessModuleResult, ToolModule, ToolModuleHost,
        ToolModuleInvocationContext,
    },
};
use serde_json::{Value, json};

use crate::{CodeModeTool, config::Limits, engine};

#[derive(Default)]
struct Host {
    calls: Mutex<Vec<ToolCall>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
    cancelled: AtomicBool,
    pair: Option<(Mutex<usize>, Condvar)>,
    delay_ms: u64,
}

impl ToolModuleHost for Host {
    fn is_cancelled(&self) -> ProcessModuleResult<bool> {
        Ok(self.cancelled.load(Ordering::Acquire))
    }

    fn execute_tool_json(&self, request: String) -> ProcessModuleResult<String> {
        let call: ToolCall = serde_json::from_str(&request).unwrap();
        self.calls.lock().unwrap().push(call.clone());
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
        let paired = if let Some((ready, changed)) = &self.pair {
            let mut ready = ready.lock().unwrap();
            *ready += 1;
            changed.notify_all();
            let (ready, _) = changed
                .wait_timeout_while(ready, Duration::from_secs(2), |n| *n < 2)
                .unwrap();
            *ready >= 2
        } else {
            true
        };
        thread::sleep(Duration::from_millis(self.delay_ms));
        self.active.fetch_sub(1, Ordering::AcqRel);
        self.completed.fetch_add(1, Ordering::Release);
        let result = match call.name.as_str() {
            "transport_failure" => return Err(ProcessModuleError::new("host transport closed")),
            "wrong_id" => ToolResult::ok("foreign-id".into(), "unexpected"),
            "oversized" => ToolResult::ok(call.id, "x".repeat(8 * 1024 * 1024)),
            "fail" => ToolResult::error(call.id, "denied by test policy"),
            _ if !paired => ToolResult::error(call.id, "callbacks did not overlap"),
            _ => ToolResult::ok(call.id, call.args.to_string()),
        };
        Ok(serde_json::to_string(&result).unwrap())
    }
}

fn execute(code: &str, config: Value, host: &Host) -> ToolResult {
    let limits = Limits::parse(config).unwrap();
    let timeout = Duration::from_millis(limits.timeout_ms);
    engine::execute("outer".into(), code, limits, timeout, host)
}

#[test]
fn async_loops_filter_results_and_only_print_summary() {
    let host = Host::default();
    let result = execute(
        r#"
        const rows = [];
        for (let n = 0; n < 6; n++) {
            const r = await tools.call('echo', {n, privateData:'large child-only data'});
            if (!r.ok) throw Error(r.error);
            rows.push(JSON.parse(r.output).n);
        }
        text(rows.filter(n => n % 2 === 0));
        console.log('count', rows.length);
        return 'do not print return value';
    "#,
        json!({}),
        &host,
    );
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.output, "[0,2,4]\ncount 6\n");
    assert_eq!(result.metadata["tool_calls"], 6);
    let calls = host.calls.lock().unwrap();
    let ids: std::collections::HashSet<_> = calls.iter().map(|call| &call.id).collect();
    assert_eq!(ids.len(), 6);
}

#[test]
fn promise_all_runs_real_host_callbacks_concurrently_with_a_bound() {
    let host = Host {
        pair: Some((Mutex::new(0), Condvar::new())),
        delay_ms: 5,
        ..Host::default()
    };
    let result = execute(
        r#"
        const results = await Promise.all(Array.from({length:8}, (_,n) => tools.call('echo', {n})));
        if (results.some(r => !r.ok)) throw Error('nested callback failed');
        text(results.length);
    "#,
        json!({"max_concurrency":2,"timeout_ms":5000}),
        &host,
    );
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(host.peak.load(Ordering::Acquire), 2);
    assert_eq!(result.output, "8\n");
    assert_eq!(host.completed.load(Ordering::Acquire), 8);
}

#[test]
fn failed_tool_results_remain_values_and_are_counted_even_when_ignored() {
    let host = Host::default();
    let result = execute(
        r#"
        const r = await tools.call('fail', {});
        text({ok:r.ok, error:r.error});
    "#,
        json!({}),
        &host,
    );
    assert!(result.ok, "{:?}", result.error);
    assert!(result.output.contains("denied by test policy"));
    assert_eq!(result.metadata["failed_tool_calls"], 1);
    assert_eq!(result.metadata["host_errors"], 0);
}

#[test]
fn ordinary_js_errors_preserve_preceding_output() {
    let result = execute(
        "text('before'); throw new Error('broken script');",
        json!({}),
        &Host::default(),
    );
    assert!(!result.ok);
    assert_eq!(result.output, "before\n");
    assert!(result.error.unwrap().contains("broken script"));
}

#[test]
fn output_limit_is_utf8_bounded_and_fatal_even_when_caught() {
    let result = execute(
        "try { text('яяяяяя'); } catch (_) {}",
        json!({"max_output_bytes":7}),
        &Host::default(),
    );
    assert!(!result.ok);
    assert!(result.output.len() <= 7);
    assert_eq!(result.output, "яяя");
    assert_eq!(result.metadata["output_truncated"], true);
    assert!(result.error.unwrap().contains("max_output_bytes"));
}

#[test]
fn call_limit_prevents_excess_host_effects_even_when_caught() {
    let host = Host::default();
    let result = execute(
        r#"
        await tools.call('echo', {});
        try { await tools.call('echo', {}); } catch (_) {}
    "#,
        json!({"max_calls":1}),
        &host,
    );
    assert!(!result.ok);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert!(result.error.unwrap().contains("max_calls"));
}

#[test]
fn recursive_code_mode_never_reaches_host() {
    let host = Host::default();
    let result = execute(
        "await tools.call('code_mode', {code:'text(1)'});",
        json!({}),
        &host,
    );
    assert!(!result.ok);
    assert!(host.calls.lock().unwrap().is_empty());
    assert!(result.error.unwrap().contains("recursive"));
}

#[test]
fn structured_patch_arguments_use_function_surface_and_third_argument_is_rejected() {
    let host = Host::default();
    let result = execute(
        "await tools.call('apply_patch', {patch:'*** Begin Patch\\n*** End Patch'});",
        json!({}),
        &host,
    );
    assert!(result.ok, "{:?}", result.error);
    {
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls[0].surface, ToolCallSurface::Function);
        assert_eq!(
            calls[0].args,
            json!({"patch":"*** Begin Patch\n*** End Patch"})
        );
    }
    for extra in ["{}", "{surface:'freeform'}", "undefined"] {
        let result = execute(
            &format!("await tools.call('apply_patch', {{patch:'patch'}}, {extra});"),
            json!({}),
            &host,
        );
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("exactly two arguments"));
    }
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

#[test]
fn cpu_timeout_is_not_catchable_and_runtime_survives_for_next_call() {
    let host = Host::default();
    let started = Instant::now();
    let result = execute(
        "text('before'); try { while(true) {} } catch (_) { while(true) {} }",
        json!({"timeout_ms":30}),
        &host,
    );
    assert!(!result.ok);
    assert!(result.error.unwrap().contains("timeout"));
    assert_eq!(result.output, "before\n");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(execute("text(42)", json!({}), &host).ok);
}

#[test]
fn cancellation_interrupts_busy_javascript() {
    let host = Arc::new(Host::default());
    let other = host.clone();
    thread::scope(|scope| {
        scope.spawn(move || {
            thread::sleep(Duration::from_millis(30));
            other.cancelled.store(true, Ordering::Release);
        });
        let result = execute("while(true) {}", json!({"timeout_ms":1000}), &host);
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("cancelled"));
    });
}

#[test]
fn memory_limit_rejects_large_allocation() {
    let result = execute(
        "const a = new Array(10_000_000).fill('x'); text(a.length);",
        json!({"memory_limit_bytes":1024*1024}),
        &Host::default(),
    );
    assert!(!result.ok);
    assert!(result.output.is_empty());
}

#[test]
fn no_implicit_os_apis_and_no_global_state_between_invocations() {
    let host = Host::default();
    let result = execute(
        r#"
        text(['process','require','fetch','Deno','Bun','setTimeout','__codeModeCall','__codeModePrint']
            .every(name => typeof globalThis[name] === 'undefined'));
        globalThis.privateState = 42;
    "#,
        json!({}),
        &host,
    );
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.output, "true\n");
    let next = execute("text(typeof privateState);", json!({}), &host);
    assert_eq!(next.output, "undefined\n");
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn unresolved_and_unhandled_promises_fail_promptly() {
    for code in [
        "await new Promise(() => {});",
        "Promise.reject(new Error('forgotten'));",
        "Promise.resolve().then(() => text('late'));",
    ] {
        let started = Instant::now();
        let result = execute(code, json!({}), &Host::default());
        assert!(!result.ok, "{code} returned success");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[test]
fn unawaited_host_calls_are_drained_before_parent_returns() {
    let host = Host {
        delay_ms: 30,
        ..Host::default()
    };
    let result = execute(
        "tools.call('echo', {}).then(() => tools.call('echo', {late:true}));",
        json!({}),
        &host,
    );
    assert!(!result.ok);
    assert!(result.error.unwrap().contains("unawaited"));
    assert_eq!(host.active.load(Ordering::Acquire), 0);
    let count = host.calls.lock().unwrap().len();
    assert!(count <= 1);
    assert_eq!(count, host.completed.load(Ordering::Acquire));
}

#[test]
fn host_errors_and_malformed_ids_cannot_be_hidden_by_js_catch() {
    for name in ["transport_failure", "wrong_id", "oversized"] {
        let host = Host::default();
        let result = execute(
            &format!("try {{ await tools.call('{name}', {{}}); }} catch (_) {{}}"),
            json!({}),
            &host,
        );
        assert!(!result.ok, "{name} returned success");
        assert_eq!(result.metadata["host_errors"], 1);
        assert!(result.error.unwrap().contains("host callback failed"));
    }
}

#[test]
fn wrapper_rejects_unknown_arguments_and_config_and_invalid_budgets() {
    let mut host = Host::default();
    let context = ToolModuleInvocationContext {
        cwd: "/tmp".into(),
        attribution: ExecutionAttribution::detached(new_execution_id()),
        skills: Default::default(),
        config: json!({}),
    };
    for args in [
        json!({"code":"text(1)","unknown":true}),
        json!({"code":"text(1)","timeout_ms":0}),
        json!({"code":"x".repeat(65537)}),
    ] {
        let call = ToolCall::new("call", "code_mode", args);
        let result: ToolResult = serde_json::from_str(
            &CodeModeTool
                .invoke_json(
                    serde_json::to_string(&call).unwrap(),
                    serde_json::to_string(&context).unwrap(),
                    &mut host,
                )
                .unwrap(),
        )
        .unwrap();
        assert!(!result.ok);
    }
    for config in [
        json!({"unknown":true}),
        json!({"timeout_ms":60001}),
        json!({"max_concurrency":0}),
        json!({"memory_limit_bytes":0}),
    ] {
        assert!(Limits::parse(config).is_err());
    }
    crate::config_schema().validate().unwrap();
}

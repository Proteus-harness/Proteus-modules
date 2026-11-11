use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
        mpsc::RecvTimeoutError,
    },
    time::{Duration, Instant},
};

use proteus_contracts::{
    domain::{ToolCall, ToolResult},
    process_module::ToolModuleHost,
};
use rquickjs::{Context, Ctx, Exception, Function, Promise, Runtime, Value, promise::PromiseState};
use serde_json::json;

use crate::{
    bridge::{self, CANCELLED, Channels, Completion, FAILED, Request, TIMEOUT},
    config::{Limits, MAX_ARGUMENT_BYTES, MAX_TRANSFER_BYTES},
};

struct State<'js> {
    pending: HashMap<usize, (Function<'js>, Function<'js>)>,
    requested: usize,
    argument_bytes: usize,
    output: String,
    output_truncated: bool,
    error: Option<String>,
    closed: bool,
}

impl State<'_> {
    fn fail(&mut self, error: String, stop: &AtomicU8) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        let _ = stop.compare_exchange(0, FAILED, Ordering::AcqRel, Ordering::Acquire);
    }
}

struct Outcome {
    output: String,
    error: Option<String>,
    requested: usize,
    output_truncated: bool,
}

pub(crate) fn execute(
    call_id: String,
    code: &str,
    limits: Limits,
    timeout: Duration,
    host: &dyn ToolModuleHost,
) -> ToolResult {
    let started = Instant::now();
    if host.is_cancelled().unwrap_or(true) {
        return ToolResult::error(call_id, "code_mode cancelled");
    }
    let (outcome, stats) = bridge::with_workers(host, &limits, started + timeout, |channels| {
        run(code, &limits, started + timeout, channels)
    });
    ToolResult::new(
        call_id,
        outcome.error.is_none(),
        outcome.output,
        vec![],
        outcome.error,
        json!({
            "tool":"code_mode", "engine":"quickjs", "tool_calls_requested":outcome.requested,
            "tool_calls":stats.attempted.load(Ordering::Acquire),
            "completed_tool_calls":stats.completed.load(Ordering::Acquire),
            "failed_tool_calls":stats.failed.load(Ordering::Acquire),
            "host_errors":stats.host_errors.load(Ordering::Acquire),
            "output_truncated":outcome.output_truncated,
            "duration_ms":started.elapsed().as_millis() as u64,
        }),
    )
}

fn run(code: &str, limits: &Limits, deadline: Instant, channels: Channels) -> Outcome {
    let empty = |error: String| Outcome {
        output: String::new(),
        error: Some(error),
        requested: 0,
        output_truncated: false,
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => return empty(format!("cannot create JavaScript runtime: {error}")),
    };
    runtime.set_memory_limit(limits.memory_limit_bytes);
    runtime.set_max_stack_size(512 * 1024);
    let stop = channels.stop.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        if Instant::now() >= deadline {
            let _ = stop.compare_exchange(0, TIMEOUT, Ordering::AcqRel, Ordering::Acquire);
        }
        stop.load(Ordering::Acquire) != 0
    })));
    let unhandled = Arc::new(AtomicUsize::new(0));
    let rejection_count = unhandled.clone();
    runtime.set_host_promise_rejection_tracker(Some(Box::new(
        move |_ctx, _promise, _reason, handled| {
            if handled {
                let _ = rejection_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n.saturating_sub(1))
                });
            } else {
                rejection_count.fetch_add(1, Ordering::Relaxed);
            }
        },
    )));
    let context = match Context::full(&runtime) {
        Ok(context) => context,
        Err(error) => return empty(format!("cannot create JavaScript context: {error}")),
    };
    context.with(|ctx| {
        let state = Rc::new(RefCell::new(State {
            pending: HashMap::new(),
            requested: 0,
            argument_bytes: 0,
            output: String::new(),
            output_truncated: false,
            error: None,
            closed: false,
        }));
        let result = install(&ctx, state.clone(), limits, &channels)
            .and_then(|()| {
                ctx.eval::<Promise, _>(format!("(async () => {{\n'use strict';\n{code}\n}})()"))
            })
            .map_err(|error| js_error(&ctx, error))
            .and_then(|promise| drive(&ctx, &promise, &state, &channels, &unhandled));
        let mut state = state.borrow_mut();
        // Break the native callback -> pending resolver -> context cycle before
        // context destruction, including errors/OOM/interrupt paths.
        state.pending.clear();
        let error = state
            .error
            .take()
            .or_else(|| match channels.stop.load(Ordering::Acquire) {
                TIMEOUT | CANCELLED => stop_error(&channels.stop),
                _ => None,
            })
            .or_else(|| result.err());
        if error.is_some() {
            let _ = channels
                .stop
                .compare_exchange(0, FAILED, Ordering::AcqRel, Ordering::Acquire);
        }
        Outcome {
            output: std::mem::take(&mut state.output),
            error,
            requested: state.requested,
            output_truncated: state.output_truncated,
        }
    })
}

fn install<'js>(
    ctx: &Ctx<'js>,
    state: Rc<RefCell<State<'js>>>,
    limits: &Limits,
    channels: &Channels,
) -> rquickjs::Result<()> {
    let caller = state.clone();
    let requests = channels.requests.clone();
    let stop = channels.stop.clone();
    let max_calls = limits.max_calls;
    let call_prefix = uuid::Uuid::new_v4().to_string();
    ctx.globals().set("__codeModeCall", Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String, args: String| {
        let mut state = caller.borrow_mut();
        if state.closed || stop.load(Ordering::Acquire) != 0 {
            state.fail("code_mode cannot start work after script completion or cancellation; await all calls".into(), &stop);
            return Err(Exception::throw_message(&ctx, "code_mode execution is closed"));
        }
        let problem = if name == "code_mode" {
            Some("recursive code_mode calls are not supported".to_owned())
        } else if name.len() > 256 {
            Some("tool name exceeds 256 bytes".to_owned())
        } else if state.requested >= max_calls {
            Some(format!("code_mode exceeds max_calls ({max_calls})"))
        } else if args.len() > MAX_ARGUMENT_BYTES || state.argument_bytes.saturating_add(args.len()) > MAX_TRANSFER_BYTES {
            Some(format!("code_mode tool arguments exceed {MAX_ARGUMENT_BYTES} bytes per call or {MAX_TRANSFER_BYTES} total bytes"))
        } else { None };
        if let Some(problem) = problem {
            state.fail(problem.clone(), &stop);
            return Err(Exception::throw_message(&ctx, &problem));
        }
        let arguments = serde_json::from_str(&args)
            .map_err(|_| Exception::throw_type(&ctx, "tool arguments must be valid JSON"))?;
        let sequence = state.requested + 1;
        let call = ToolCall::new(format!("code-mode-{call_prefix}-{sequence}"), name, arguments);
        let (promise, resolve, reject) = Promise::new(&ctx)?;
        requests.try_send(Request { sequence, call })
            .map_err(|_| Exception::throw_message(&ctx, "code_mode host request queue unavailable"))?;
        state.requested = sequence;
        state.argument_bytes += args.len();
        state.pending.insert(sequence, (resolve, reject));
        Ok(promise)
    })?)?;
    let stop = channels.stop.clone();
    let max_output = limits.max_output_bytes;
    ctx.globals().set(
        "__codeModePrint",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, value: String| {
            let mut state = state.borrow_mut();
            if state.closed {
                state.fail("code_mode has unawaited asynchronous output".into(), &stop);
                return Err(Exception::throw_message(&ctx, "script has completed"));
            }
            if value
                .len()
                .saturating_add(state.output.len())
                .saturating_add(1)
                > max_output
            {
                let mut end = max_output
                    .saturating_sub(state.output.len())
                    .min(value.len());
                while !value.is_char_boundary(end) {
                    end -= 1;
                }
                state.output.push_str(&value[..end]);
                state.output_truncated = true;
                let problem =
                    format!("code_mode printed output exceeds max_output_bytes ({max_output})");
                state.fail(problem.clone(), &stop);
                return Err(Exception::throw_message(&ctx, &problem));
            }
            state.output.push_str(&value);
            state.output.push('\n');
            Ok(())
        })?,
    )?;
    ctx.eval::<(), _>(include_str!("bootstrap.js"))
}

fn drive<'js>(
    ctx: &Ctx<'js>,
    promise: &Promise<'js>,
    state: &Rc<RefCell<State<'js>>>,
    channels: &Channels,
    unhandled: &AtomicUsize,
) -> Result<(), String> {
    loop {
        if let Some(error) = stop_error(&channels.stop) {
            return Err(error);
        }
        for _ in 0..64 {
            if promise.state() != PromiseState::Pending {
                state.borrow_mut().closed = true;
                promise
                    .result::<Value>()
                    .expect("settled promise")
                    .map_err(|error| js_error(ctx, error))?;
                if !state.borrow().pending.is_empty() {
                    return Err(
                        "code_mode has unawaited tool calls; await every delegated operation"
                            .into(),
                    );
                }
            }
            if !ctx.execute_pending_job() {
                break;
            }
            if channels.stop.load(Ordering::Acquire) != 0 {
                break;
            }
        }
        while let Ok(completion) = channels.completions.try_recv() {
            deliver(ctx, state, channels, completion)?;
        }
        if let Some(error) = stop_error(&channels.stop) {
            return Err(error);
        }
        if promise.state() != PromiseState::Pending {
            state.borrow_mut().closed = true;
            promise
                .result::<Value>()
                .expect("settled promise")
                .map_err(|error| js_error(ctx, error))?;
            if !state.borrow().pending.is_empty() {
                return Err(
                    "code_mode has unawaited tool calls; await every delegated operation".into(),
                );
            }
            // Resolvers can enqueue reactions after the first job-drain pass.
            // Finish those with admission already closed before reporting success.
            if ctx.execute_pending_job() {
                continue;
            }
            if unhandled.load(Ordering::Relaxed) != 0 {
                return Err("code_mode has an unhandled Promise rejection".into());
            }
            return Ok(());
        }
        if state.borrow().pending.is_empty() {
            // There are no timers or external events which can settle this
            // promise. Continue queued microtasks, otherwise fail immediately.
            if ctx.execute_pending_job() {
                continue;
            }
            return Err(
                "code_mode script Promise cannot settle: no pending tool calls or JavaScript jobs"
                    .into(),
            );
        }
        match channels.completions.recv_timeout(Duration::from_millis(5)) {
            Ok(completion) => deliver(ctx, state, channels, completion)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("code_mode host workers disconnected".into());
            }
        }
    }
}

fn deliver<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State<'js>>>,
    channels: &Channels,
    completion: Completion,
) -> Result<(), String> {
    let Some((resolve, reject)) = state.borrow_mut().pending.remove(&completion.sequence) else {
        return Err("code_mode received an unknown host completion".into());
    };
    match completion.result {
        Ok(response) => {
            let value = ctx
                .json_parse(response)
                .map_err(|error| js_error(ctx, error))?;
            resolve
                .call::<_, ()>((value,))
                .map_err(|error| js_error(ctx, error))
        }
        Err(error) => {
            // Host/protocol failures are not successful scripts, even if a
            // user catches the rejected callback Promise.
            let _ = reject.call::<_, ()>((error.as_str(),));
            state.borrow_mut().fail(
                format!("code_mode host callback failed: {error}"),
                &channels.stop,
            );
            Err(error)
        }
    }
}

fn stop_error(stop: &AtomicU8) -> Option<String> {
    match stop.load(Ordering::Acquire) {
        TIMEOUT => Some("code_mode timeout".into()),
        CANCELLED => Some("code_mode cancelled".into()),
        FAILED => Some("code_mode budget or host callback failure".into()),
        _ => None,
    }
}

fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    let mut message = rquickjs::CaughtError::from_error(ctx, error).to_string();
    if message.len() > 2048 {
        let mut end = 2048;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    format!("JavaScript error: {message}")
}

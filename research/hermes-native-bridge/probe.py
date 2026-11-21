#!/usr/bin/env python3
"""Zero-inference counterexamples for incomplete native Hermes host bridges."""
import argparse
from contextlib import contextmanager, nullcontext
from dataclasses import dataclass
import json
import logging
from pathlib import Path
import sys
import time
from types import ModuleType, SimpleNamespace

from native import NativeSource, SOURCES


@contextmanager
def injected_modules(entries):
    """Replace only external dependencies of the selected native AST functions."""
    before = {name: sys.modules.get(name) for name in entries}
    try:
        for name, attributes in entries.items():
            module = ModuleType(name)
            module.__dict__.update(attributes)
            sys.modules[name] = module
        yield
    finally:
        for name, module in before.items():
            if module is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = module


def sequential_tool_bypass(native):
    """An _invoke_tool-only bridge does not own native sequential todo effects."""
    todo = native.todo()
    executor, context = native.inline_todo(todo)
    dispatch = native.execute("agent/tool_executor.py", [
        "_SequentialDispatch", "_resolve_sequential_dispatch",
    ], {"dataclass": dataclass, "INLINE_TOOL_EXECUTORS": {"todo_list": executor},
        "InlineToolContext": context})
    host_calls = []

    def denied_invoke(*args, **kwargs):
        host_calls.append((args, kwargs))
        raise RuntimeError("Core tool invocation refused")

    agent = SimpleNamespace(_invoke_tool=denied_invoke, _todo_store=todo["TodoStore"]())
    args = {"todos": [{"id": "1", "content": "synthetic task", "status": "pending"}]}
    ref = SimpleNamespace(name="todo_list", args=args, task_id="probe", call_id="tool-1", trace=[])
    selected = dispatch["_resolve_sequential_dispatch"](agent, ref, [])
    result = json.loads(selected.execute(args))
    assert result["revision"] == 1 and agent._todo_store.read()[0]["content"] == "synthetic task"
    assert host_calls == []
    return {"native_todo_revision": result["revision"], "invoke_override_calls": len(host_calls),
            "incomplete_bridge_refused_effect": False}


def middleware_failure_is_open(native):
    """A failing host callback installed as ordinary plugin does not fail closed."""
    effects, failures = [], []

    def host_bridge(**kwargs):
        raise RuntimeError("Core checkpoint/authorization transport refused")

    manager = SimpleNamespace(_middleware={"tool_execution": [host_bridge]},
                              _report_hook_failure=lambda *args, **kwargs: failures.append(str(args[3])))
    dispatch = SimpleNamespace(_hook_callback_kwargs=lambda callback, kwargs: kwargs)
    chain = native.execute("hermes_cli/middleware.py", [
        "middleware_payload", "_DownstreamExecutionError", "_run_execution_chain",
        "run_tool_execution_middleware",
    ], {"MIDDLEWARE_SCHEMA_VERSION": "hermes.middleware.v1", "OBSERVER_SCHEMA_VERSION": "hermes.observer.v1",
        "TOOL_EXECUTION_MIDDLEWARE": "tool_execution"})
    with injected_modules({"hermes_cli.plugins": {"_delivery_manager": lambda: manager},
                           "hermes_cli.plugins_dispatch": {"PluginDispatchMixin": dispatch}}):
        result = chain["run_tool_execution_middleware"](
            "synthetic_tool", {"value": 1},
            lambda args: effects.append(args.copy()) or "native terminal executed",
        )
        # Positive control: a deliberate terminal error result does stop execution.
        manager._middleware["tool_execution"] = [lambda **kwargs: {"error": "Core denied"}]
        denied = chain["run_tool_execution_middleware"](
            "synthetic_tool", {"value": 2},
            lambda args: effects.append(args.copy()) or "unexpected second effect",
        )
    assert len(failures) == 1 and effects == [{"value": 1}]
    assert result == "native terminal executed"
    assert denied == {"error": "Core denied"}
    return {"bridge_exceptions_reported": len(failures), "terminal_effects": len(effects),
            "incomplete_bridge_refused_effect": False, "explicit_denial_control_prevented_effect": True}


def auxiliary_summary_bypass(native):
    """Replacing the primary client alone leaves the separate summary transport."""
    calls = []

    class AuxiliaryReached(Exception):
        pass

    def call_llm(**kwargs):
        calls.append({"task": kwargs["task"], "main_runtime": kwargs["main_runtime"]})
        raise AuxiliaryReached("Native auxiliary route reached without primary client")

    method = native.execute("agent/context_compressor.py", ["ContextCompressor._call_summary_llm"], {
        "call_llm": call_llm, "aux_interrupt_protection": nullcontext,
        "_pinned_summary_call_kwargs": lambda: {}, "time": time,
        "logger": logging.getLogger("hermes-native-probe"),
    })["_call_summary_llm"]
    agent = SimpleNamespace(model="fixture", provider="fixture", base_url="", api_key="", api_mode="chat_completions",
                            summary_model="", context_length=65536,
                            _apply_summary_route=lambda *args: None,
                            _record_aux_compression_call=lambda **kwargs: None)
    try:
        method(agent, "synthetic summary input", time.monotonic())
    except AuxiliaryReached:
        pass
    else:
        raise AssertionError("Expected native auxiliary transport boundary")
    assert len(calls) == 1 and calls[0]["task"] == "compression"
    return {"auxiliary_calls": len(calls), "primary_client_calls": 0,
            "call_role": calls[0]["task"], "network_requests": 0}


def persistence_is_not_core_ack(native):
    """A native no-DB flush is successful progress but is not a durable Core ACK."""
    flush = native.execute("agent/session_persistence.py", [
        "SessionPersistenceMixin._flush_messages_to_session_db_unlocked",
    ])["_flush_messages_to_session_db_unlocked"]
    progress = native.execute("agent/tool_executor.py", [
        "_flush_session_db_after_tool_progress",
    ])["_flush_session_db_after_tool_progress"]
    agent = SimpleNamespace(_session_db=None)
    agent._flush_messages_to_session_db = lambda messages: flush(agent, messages)
    messages = [{"role": "tool", "tool_call_id": "synthetic", "content": "done"}]
    assert agent._flush_messages_to_session_db(messages) is None
    with injected_modules({
        "agent.conversation_loop": {"_maybe_inject_run_budget_wrapup": lambda *args: None},
        "agent.turn_iteration_prep": {"_maybe_inject_iteration_budget_warning": lambda *args: None},
    }):
        acknowledged = progress(agent, messages, stage="probe")
    assert acknowledged is True
    return {"native_progress_acknowledged": acknowledged, "native_db_writes": 0,
            "core_checkpoint_calls": 0, "core_checkpoint_revision": None}


CHECKS = (sequential_tool_bypass, middleware_failure_is_open,
          auxiliary_summary_bypass, persistence_is_not_core_ack)


def run(upstream):
    native = NativeSource(upstream)
    checks = [{"name": check.__name__, "passed": True, "observed": check(native)} for check in CHECKS]
    return {"status": "counterexamples_reproduced", "upstream": SOURCES,
            "method": "unchanged selected native AST functions; external dependencies replaced with deterministic sentinels",
            "full_native_agent_executed": False, "installed_hermes_profile": False,
            "paid_model_requests": 0, "network_requests": 0, "checks": checks}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    receipt = run(args.upstream)
    text = json.dumps(receipt, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text)
    print(json.dumps({key: receipt[key] for key in ("status", "paid_model_requests", "full_native_agent_executed")}))


if __name__ == "__main__":
    main()

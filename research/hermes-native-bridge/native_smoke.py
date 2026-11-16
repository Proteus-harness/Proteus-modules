#!/usr/bin/env python3
"""Construct unmodified pinned AIAgent offline; this is not a Proteus module."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from native import NativeSource, SOURCES

PREFIX = "HERMES_NATIVE_SMOKE "


def child(upstream, state):
    sys.path.insert(0, str(upstream))
    from run_agent import AIAgent
    from agent.background_review import load_background_review_settings
    from agent.turn_context import _tick_memory_nudge
    from hermes_cli.config_defaults import DEFAULT_CONFIG

    # Public constructor only: no subclass, function replacements or plugins.
    # The SDK credential is deliberately unusable; outer unshare-net also blocks
    # provider/catalog access. No run_conversation/chat or native tool is called.
    agent = AIAgent(
        base_url="http://127.0.0.1:9/v1", api_key="unused-offline-fixture",
        provider="openai", api_mode="chat_completions", model="gpt-4o-mini",
        enabled_toolsets=["hermes-cli"], quiet_mode=True, cwd=str(state / "workspace"),
    )
    try:
        triggers = [_tick_memory_nudge(agent) for _ in range(10)]
        enabled = load_background_review_settings()[0]
        assert DEFAULT_CONFIG["auxiliary"]["background_review"]["enabled"] is True
        assert enabled is True and agent.skip_background_review is False
        assert agent._memory_enabled and agent._memory_store is not None
        assert agent._memory_nudge_interval == 10 and agent._skill_nudge_interval == 10
        assert triggers == [False] * 9 + [True]
        assert {"memory", "skill_manage", "delegate_task", "read_file"}.issubset(agent.valid_tool_names)
        record = {
            "status": "native_constructor_toolset_smoke_passed",
            "python": sys.version.split()[0],
            "fixture_model": agent.model, "fixture_provider": agent.provider,
            "native_class": type(agent).__name__, "tool_names": sorted(agent.valid_tool_names),
            "tool_count": len(agent.tools), "native_compressor_class": type(agent.context_compressor).__name__,
            "memory_enabled": agent._memory_enabled, "memory_store_constructed": agent._memory_store is not None,
            "memory_nudge_interval": agent._memory_nudge_interval,
            "skill_nudge_interval": agent._skill_nudge_interval,
            "skip_background_review": agent.skip_background_review,
            "background_review_enabled_default": True, "background_review_settings_actual": enabled,
            "native_memory_tick_trigger_sequence": triggers,
            "full_conversation_executed": False, "native_tool_invocations": 0,
            "background_review_started": False, "paid_model_requests": 0,
            "installed_hermes_profile": False,
        }
        print(PREFIX + json.dumps(record), flush=True)
    finally:
        agent.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path)
    parser.add_argument("--python", type=Path, help="Interpreter prepared from the pinned upstream uv.lock")
    parser.add_argument("--state", required=True, type=Path, help="Fresh disposable state directory")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    upstream, state = args.upstream.resolve(), args.state.resolve()
    if args.child:
        if os.environ.get("HERMES_HOME") != str(state):
            parser.error("child requires its dedicated HERMES_HOME")
        return child(upstream, state)
    if args.python is None or args.output is None:
        parser.error("--python and --output are required")
    NativeSource(upstream)
    dirty = subprocess.check_output(["git", "status", "--porcelain"], cwd=upstream, text=True)
    if dirty:
        raise ValueError("Native import smoke requires an entirely clean pinned checkout")
    state.mkdir(parents=True, exist_ok=False, mode=0o700)
    (state / "workspace").mkdir(mode=0o700)
    command = [
        "bwrap", "--unshare-net", "--ro-bind", "/", "/", "--bind", str(state), str(state),
        "--clearenv", "--setenv", "PATH", "/usr/bin:/bin", "--setenv", "LANG", "C.UTF-8",
        "--setenv", "HERMES_HOME", str(state), "--setenv", "HERMES_DISABLE_LAZY_INSTALLS", "1",
        "--chdir", str(state / "workspace"), str(args.python.absolute()), "-B", str(Path(__file__).resolve()),
        "--child", "--upstream", str(upstream), "--state", str(state),
    ]
    result = subprocess.run(command, capture_output=True, text=True, timeout=90)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    log = args.output.with_suffix(".log")
    log.write_text(result.stdout + result.stderr)
    if result.returncode != 0:
        raise RuntimeError(f"Native constructor smoke failed ({result.returncode}); see {log}")
    record = json.loads(next(line[len(PREFIX):] for line in result.stdout.splitlines() if line.startswith(PREFIX)))
    record.update(upstream=SOURCES, external_network="outer bubblewrap --unshare-net",
                  filesystem="read-only root; only fresh dedicated state directory writable",
                  credentials="cleared inherited environment; deliberately unusable fixture SDK credential",
                  source_checkout_clean=True, driver_python=sys.version.split()[0])
    args.output.write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({key: record[key] for key in ("status", "tool_count", "paid_model_requests")}))


if __name__ == "__main__":
    main()

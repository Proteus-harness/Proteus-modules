# Native Codex exec bridge — research prototype

This isolated crate implements `ToolModule` / the current tool/v6 callback surface,
using the actual external `codex-code-mode-host` V8 executable. It is not installed
or included in a profile. `NativeSyncTool` is explicitly named `codex_exec_sync`.

The supported subset accepts JavaScript, trusted setup-time function definitions,
and invocation-bound tool callbacks. It consumes native `Yielded` observations
internally until completion. All exits kill and join the native process and its
reader thread. Callback admission checks the enclosing host cancellation and local
deadline; already started bound callbacks finish before the invocation returns.
The Core host preserves the authority, policy, journal and execution lineage.
No host pointer or callback worker is stored after invocation. Configured tool
names restrict admission; scripts cannot introduce new tool definitions.

This is **not full Codex exec/wait parity**: there are no cells shared across model
calls, `store/load` persistence across calls, concurrent callback dispatch,
notification delivery, multimodal output translation or public `wait`/terminate.
The sync result preserves the native response envelope as JSON. Native stderr is
discarded. Resource/output limits and diagnostics are prototype choices.

Source pin: OpenAI Codex `086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a`.
Authoritative files: `codex-rs/code-mode-protocol/src/host/{codec,message,payload}.rs`
and `codex-rs/code-mode-protocol/src/session.rs`. The stdio protocol uses a four-byte
little-endian length followed by JSON, negotiated protocol version 1.

`black_box_yield_returns_before_new_callback_request` proves the lifetime gap with
the real binary: `execute/initialResponse: Yielded` precedes a later tool/invoke
delegate request from the same cell. Returning that cell from tool/v6 would end
the borrowed `ToolModuleHost` invocation before a fresh callback arrives. Keeping
that host or transplanting the callback into a later unrelated invocation is not
safe. The existing v6 surface also cannot enumerate the effective registry.

Returning live cells from tool/v6 needs an explicit Core-owned capability with an
owned callback lease, original execution lineage and policy authority, session
and turn cancellation/deadline revocation, bounded concurrency, durable settlement,
and a defined wait/terminate authorization policy. It also needs an authorized
registry-schema snapshot to construct the native tool definitions. These are
contract proposals, not changes made by this prototype.

The workflow/v20 alternative was subsequently checked separately:
[WORKFLOW_BOUNDARY.md](WORKFLOW_BOUNDARY.md). It removes the tool/v6 lifetime
constraint for **one live workflow turn**, while native freeform, causal journal,
replay and durable across-turn state still have explicit boundaries. The test
suite now includes five checks against current Core validators as well as seven
real native-host tests.

Run real executable tests (no model calls or credentials):

```sh
cargo test --manifest-path tool/codex-exec-bridge/Cargo.toml -- --nocapture
```

Default binary: `/usr/lib/chatgpt/resources/codex-code-mode-host`; override with
`CODEX_CODE_MODE_HOST`. A missing binary fails tests rather than silently skipping.

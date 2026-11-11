# Workflow/v20 alternative: eligible lifetime, incomplete parity

Verdict: a native runtime and turn-local exec/wait cells can live inside a single
workflow invocation without an owned tool/v6 lease. The borrowed workflow host
remains alive across all model rounds. This solves the earlier **lifetime**
problem, but does not by itself provide full native freeform surface, causal
nested-operation journal or replay of arbitrary native JavaScript.

This investigation did not register any workflow operation as a host effect and
did not modify Core contracts or shared Core implementation. Tests import the
current production Core validator bodies read-only at build time.

## Eligibility through existing contracts

| Area | Existing capability | Boundary |
| --- | --- | --- |
| Lifetime | `WorkflowModuleHost` is borrowed for all of `run_json` | Native event-loop workers must be scoped inside this call and joined on every exit. Cells cannot retain host references after the workflow returns. |
| Tool discovery | `visible_tools_json` and `select_tools_json` | Snapshot only policy-visible eligible registry tools. Provider-hosted tools are not local callbacks. Workflow meta-tools must not recurse through the registry. Phase restrictions still apply. |
| Effects | `execute_tool_json(task, call)` / `execute_tools_json` | These call the ordinary Core orchestrator, registry, policy, approval, hooks, cancellation and journal path. No direct file, shell or network implementation belongs in the runtime bridge. |
| Model surface | `ToolSurface::WorkflowFunction` | Supports a JSON function wrapper. Native raw/freeform `exec` has no workflow-owned freeform variant. Ordinary `Freeform` must match a real registered tool in model-request validation. |
| Root result binding | `WorkflowToolResultBinding` | One unique history call ID, one exact execution call with the same ID, one reserved result identity. Name/arguments may be explicitly adapted, as for `proteus_tool_call`. This is not a one-to-many native callback binding. |
| Child effects | Unbound workflow registry callbacks | Still execute and journal normally. They are not automatically inserted as conversation results by `HistoryCapture`. Adding child bindings without corresponding history calls fails. |
| Causality | Current host callbacks default to `OperationOwner::Workflow` | Call ID prefixes do not create an `OperationOwner::Tool { outer_exec_id }` edge. A workflow cannot select another operation owner through v20. |
| Cancellation | Current workflow host checks its original turn cancellation | All native cells and callback workers must terminate/drain before workflow exit. A subsequent turn must not inherit live callbacks authorized by an expired turn. |
| Replay | Registered tools/models are replay oracles | Native JavaScript running directly inside the workflow is re-executed. Its random/timing-dependent output is not an existing Core oracle. Native observations/cell transitions need recorded replayable outcomes, or the supported language must be restricted to deterministic behavior. |
| Across-turn state | No native session/checkpoint state in workflow result contracts | Turn-local cells are feasible. A resident component process is not durable state or a guaranteed process affinity. Preserving arbitrary native `store/load` state across process restart/turn continuation needs a defined snapshot/recovery path. |

## Checked blockers

`workflow_boundary_tests` exercise the actual current `HistoryCapture` and
workflow replay normalization implementation:

1. Binding `exec-parent/tool-1` under a history containing only `exec-parent`
   rejects with `checkpoint binding exec-parent/tool-1 has no history call`.
2. Reusing `exec-parent` to bind two different nested operations rejects with
   `duplicate checkpoint tool binding exec-parent`. Executing a different call
   against an accepted binding rejects the exact-operation mismatch.
3. Unbound callbacks pass the capture's binding check but their result is not
   added to conversation history. This does **not** mean their execution journal
   is absent: normal registry execution records it independently.
4. Two executions of actual native `text(Math.random())` produce different
   output. The current Core `messages_equal` rejects those ToolResult histories
   even after its normal ID normalization. Consequently merely re-running the
   native runtime cannot guarantee current workflow replay.
5. `WorkflowFunction.call_surface()` is Function; native Freeform is distinct.

## Handoff

A useful existing-contract experiment can expose explicitly named **function**
`exec_turn`/`wait_turn`, keep cells in one workflow `run_json`, execute only normal
host callbacks, preserve per-turn cancellation, and record callbacks as workflow
operations. It must describe the replay/determinism and persistence limits.
That experiment is not the full Codex profile requested by native parity.

For full parity, decisions are still needed on a workflow-owned freeform surface;
recording/replaying native exec/wait observations and cell state; and whether
workflow-native callbacks intentionally remain Workflow-owned or receive a
Core-authorized causal parent operation. A new v6 lease is not required merely
to keep cells alive within the same workflow turn. No contract changes were made.

Relevant Core sources:

- `crates/proteus-contracts/src/process_module.rs`: workflow host lifetime/API.
- `crates/proteus-contracts/src/domain/tool.rs`: tool surfaces.
- `crates/proteus-core/src/core/bound_tools.rs`: model surface validation and operation ownership.
- `crates/proteus-core/src/core/workflow_host.rs`: normal registry/cancellation path.
- `crates/proteus-core/src/core/session_journal/history_capture.rs`: checkpoint eligibility.
- `crates/proteus-core/src/core/model_call_scope.rs`: Core-owned operation scope.
- `crates/proteus-core/src/core/workflow_replay/{normalize.rs,fixture/operations.rs}`: exact output and causal validation.

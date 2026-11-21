//! Plan tool как reference process module.
//!
//! Регистрирует tool `update_plan` в духе Codex `update_plan` и Claude Code
//! TodoWrite: модель ведёт пошаговый план со статусами, сервер только
//! валидирует и нормализует. Состояние плана живёт в transcript как
//! последовательность tool calls — клиенты рендерят карточку плана из
//! аргументов, отдельного runtime-состояния и протокольных событий нет.

use proteus_contracts::{
    process_module::{
        ModuleRegistry, ProcessModuleError, ToolModule, ToolModuleHostMut, ToolModuleObject,
    },
    tool_support::parse_invocation_context,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub struct PlanTool;

impl ToolModule for PlanTool {
    fn spec_json(&self) -> String {
        let spec = json!({
            "name": "update_plan",
            "description": "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "explanation": {
                        "type": "string",
                        "description": "Optional short note about why the plan changed."
                    },
                    "plan": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["plan"],
                "additionalProperties": false
            },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "ReadOnly",
            "supports_parallel_tool_calls": false,
            "timeout_ms": null,
            "metadata": {
                "category": "planning",
                "hot": true,
                "tags": ["plan", "todo", "steps", "progress"],
                "aliases": ["todo list", "task plan", "update plan"]
            }
        });
        String::from(spec.to_string())
    }

    fn invoke_json(
        &self,
        call_json: String,
        context_json: String,
        _host: &mut ToolModuleHostMut<'_>,
    ) -> Result<String, ProcessModuleError> {
        if let Err(error) = parse_invocation_context(context_json.as_str()) {
            return Err(ProcessModuleError::new(error));
        }
        match invoke_impl(call_json.as_str()) {
            Ok(result_json) => Ok(String::from(result_json)),
            Err(error) => Err(ProcessModuleError::new(error)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanStep {
    step: String,
    status: PlanStepStatus,
}

impl PlanStep {
    fn into_result_value(self) -> Value {
        result_object([
            ("step", Value::String(self.step)),
            (
                "status",
                serde_json::to_value(self.status).expect("plan status serializes as a string"),
            ),
        ])
    }
}

// Insert in the same order as the original json! and PlanStep serialization.
// Unlike json!, this takes ownership of each of the already constructed values.
fn result_object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let mut object = Map::with_capacity(N);
    for (key, value) in entries {
        object.insert(key.to_owned(), value);
    }
    Value::Object(object)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    plan: Vec<PlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

fn invoke_impl(call_json: &str) -> Result<String, String> {
    let mut call: Value = serde_json::from_str(call_json)
        .map_err(|error| format!("invalid ToolCall JSON: {error}"))?;
    let args = call.get_mut("args").map(Value::take).unwrap_or(Value::Null);
    let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");

    match validate_plan(args) {
        Ok(plan_args) => {
            let counts = status_counts(&plan_args.plan);
            let plan = plan_args
                .plan
                .into_iter()
                .map(PlanStep::into_result_value)
                .collect();
            let result = result_object([
                ("call_id", call_id.into()),
                ("ok", true.into()),
                ("output", "Plan updated".into()),
                ("content", Value::Array(Vec::new())),
                ("error", Value::Null),
                (
                    "metadata",
                    result_object([
                        ("plan", Value::Array(plan)),
                        ("explanation", plan_args.explanation.into()),
                        ("completed", counts.completed.into()),
                        ("in_progress", counts.in_progress.into()),
                        ("pending", counts.pending.into()),
                    ]),
                ),
            ]);
            Ok(result.to_string())
        }
        Err(problem) => {
            // Ошибка валидации возвращается как failed tool result, а не как
            // invoke-ошибка: модель видит текст и может поправить план.
            let result = json!({
                "call_id": call_id,
                "ok": false,
                "output": "",
                "content": [],
                "error": problem,
                "metadata": {}
            });
            Ok(result.to_string())
        }
    }
}

#[derive(Default)]
struct StatusCounts {
    pending: usize,
    in_progress: usize,
    completed: usize,
}

fn status_counts(steps: &[PlanStep]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for step in steps {
        match step.status {
            PlanStepStatus::Pending => counts.pending += 1,
            PlanStepStatus::InProgress => counts.in_progress += 1,
            PlanStepStatus::Completed => counts.completed += 1,
        }
    }
    counts
}

fn validate_plan(args: Value) -> Result<PlanArgs, String> {
    serde_json::from_value(args).map_err(|error| {
        format!("plan entries must be {{step, status: pending|in_progress|completed}}: {error}")
    })
}

pub fn register_modules(registry: &mut dyn ModuleRegistry) -> Result<(), ProcessModuleError> {
    let tool: ToolModuleObject = Box::new(PlanTool);
    registry.register_tool(tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(args: Value) -> Value {
        let call = json!({ "id": "call_plan", "name": "update_plan", "args": args });
        let result = invoke_impl(&call.to_string()).expect("invoke");
        serde_json::from_str(&result).expect("tool result")
    }

    #[test]
    fn accepts_valid_plan_and_reports_counts() {
        let result = invoke(json!({
            "explanation": "  начало\n",
            "plan": [
                { "step": "Read \"test\" at C:\\repo\n", "status": "completed" },
                { "step": "Fix the parser", "status": "in_progress" },
                { "step": "Run the suite", "status": "pending" }
            ]
        }));

        assert_eq!(result["ok"], true);
        assert_eq!(result["metadata"]["completed"], 1);
        assert_eq!(result["metadata"]["in_progress"], 1);
        assert_eq!(result["metadata"]["pending"], 1);
        assert_eq!(result["metadata"]["plan"][1]["status"], "in_progress");
        assert_eq!(result["metadata"]["explanation"], "  начало\n");
        assert_eq!(
            result["metadata"]["plan"][0]["step"],
            "Read \"test\" at C:\\repo\n"
        );
        assert_eq!(result["output"], "Plan updated");
    }

    fn assert_two_step_counts(status: &str, [pending, in_progress, completed]: [u64; 3]) {
        let result = invoke(json!({
            "plan": [
                { "step": "a", "status": status },
                { "step": "b", "status": status }
            ]
        }));

        assert_eq!(result["ok"], true);
        let metadata = &result["metadata"];
        assert_eq!(metadata["pending"], pending);
        assert_eq!(metadata["in_progress"], in_progress);
        assert_eq!(metadata["completed"], completed);
    }

    #[test]
    fn allows_fully_completed_plan_without_in_progress() {
        assert_two_step_counts("completed", [0, 0, 2]);
    }

    #[test]
    fn allows_pending_plan_without_in_progress_step() {
        assert_two_step_counts("pending", [2, 0, 0]);
    }

    #[test]
    fn accepts_multiple_in_progress_steps_like_upstream_handler() {
        assert_two_step_counts("in_progress", [0, 2, 0]);
    }

    #[test]
    fn accepts_empty_blank_and_long_plans() {
        assert_eq!(invoke(json!({ "plan": [] }))["ok"], true);
        assert_eq!(
            invoke(json!({ "plan": [{ "step": "", "status": "pending" }] }))["ok"],
            true
        );
        let long = (0..21)
            .map(|index| json!({ "step": format!("step {index}"), "status": "pending" }))
            .collect::<Vec<_>>();
        assert_eq!(invoke(json!({ "plan": long }))["ok"], true);
    }

    #[test]
    fn rejects_bad_status_and_unknown_fields() {
        assert_eq!(
            invoke(json!({ "plan": [{ "step": "a", "status": "done" }] }))["ok"],
            false
        );
        assert_eq!(
            invoke(json!({
                "plan": [{ "step": "a", "status": "pending", "extra": true }]
            }))["ok"],
            false
        );
        assert_eq!(
            invoke(json!({ "plan": [], "unexpected": true }))["ok"],
            false
        );
    }

    #[test]
    fn owned_plan_result_keeps_wire_order_and_failure_shape() {
        let step = PlanStep {
            step: "Line 1\nСтрока 2 🙂".into(),
            status: PlanStepStatus::InProgress,
        };
        let call =
            json!({"id":"call-ю", "args":{"plan":[step], "explanation": "\talso"}}).to_string();
        let legacy = json!({
            "call_id": "call-ю",
            "ok": true,
            "output": "Plan updated",
            "content": [],
            "error": null,
            "metadata": {
                "plan": [step],
                "explanation": "\talso",
                "completed": 0,
                "in_progress": 1,
                "pending": 0,
            }
        })
        .to_string();
        assert_eq!(invoke_impl(&call).unwrap(), legacy);

        let failure = invoke(json!({"plan":[{}]}));
        assert_eq!(failure["ok"], false);
        assert_eq!(
            failure["error"],
            "plan entries must be {step, status: pending|in_progress|completed}: missing field `step`"
        );
    }
}

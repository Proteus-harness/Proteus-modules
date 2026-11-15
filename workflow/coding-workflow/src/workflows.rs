use proteus_contracts::{
    contracts::WorkflowFailure,
    process_module::{
        WorkflowModule, WorkflowModuleHostMut, WorkflowModuleInput, WorkflowModuleOutput,
    },
};

use super::{
    CODEX_LOOP_MODULE_ID, MAX_TOOL_ROUNDS, project_check::run_project_check, run_codex_loop,
    run_plan_execute_review, run_single_loop,
};

pub struct CodingSingleLoopWorkflow {
    pub max_tool_rounds: usize,
}

impl Default for CodingSingleLoopWorkflow {
    fn default() -> Self {
        Self {
            max_tool_rounds: MAX_TOOL_ROUNDS,
        }
    }
}

pub struct CodingPlanExecuteReviewWorkflow;
pub struct CodingCodexLoopWorkflow;
pub struct CodingProjectCheckWorkflow;

impl WorkflowModule for CodingSingleLoopWorkflow {
    fn run_json(
        &self,
        input_json: String,
        host: &mut WorkflowModuleHostMut<'_>,
    ) -> Result<String, WorkflowFailure> {
        let input = decode_input(&input_json)?;
        let output = run_single_loop(input, host, self.max_tool_rounds)?;
        encode_output(&output)
    }
}

impl WorkflowModule for CodingCodexLoopWorkflow {
    fn run_json(
        &self,
        input_json: String,
        host: &mut WorkflowModuleHostMut<'_>,
    ) -> Result<String, WorkflowFailure> {
        let input = decode_input(&input_json)?;
        let output = run_codex_loop(input, host, CODEX_LOOP_MODULE_ID)?;
        encode_output(&output)
    }
}

impl WorkflowModule for CodingPlanExecuteReviewWorkflow {
    fn run_json(
        &self,
        input_json: String,
        host: &mut WorkflowModuleHostMut<'_>,
    ) -> Result<String, WorkflowFailure> {
        let input = decode_input(&input_json)?;
        let output = run_plan_execute_review(input, host)?;
        encode_output(&output)
    }
}

impl WorkflowModule for CodingProjectCheckWorkflow {
    fn run_json(
        &self,
        input_json: String,
        host: &mut WorkflowModuleHostMut<'_>,
    ) -> Result<String, WorkflowFailure> {
        let input = decode_input(&input_json)?;
        let output = run_project_check(input, host)?;
        encode_output(&output)
    }
}

fn decode_input(input_json: &str) -> Result<WorkflowModuleInput, WorkflowFailure> {
    serde_json::from_str(input_json).map_err(|error| WorkflowFailure::new(error.to_string()))
}

fn encode_output(output: &WorkflowModuleOutput) -> Result<String, WorkflowFailure> {
    serde_json::to_string(output).map_err(|error| WorkflowFailure::new(error.to_string()))
}

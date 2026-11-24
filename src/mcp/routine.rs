//! MCP tools for the daily routine: the user's plan, and (later) the market
//! snapshot, Roman setups and spot status built on it.

use super::{OrbitTapeMcp, to_text};
use data::config::plan::Plan;
use rmcp::{tool, tool_router};
use serde_json::json;

impl OrbitTapeMcp {
    pub(crate) fn plan(&self) -> Result<Plan, String> {
        Plan::load_from(&self.paths.plan)
    }
}

#[tool_router(router = routine_tool_router, vis = "pub(crate)")]
impl OrbitTapeMcp {
    #[tool(
        description = "The user's trading plan (plan.toml): prop-firm rules (risk %, counter-trend risk %, max correlated positions, max stop %, leverage by coin), the coin universe and exchange, Roman Setup input overrides, the spot accumulation plan (holdings, goal, budget, reference price, heads-up and buy levels with amounts and which were used, monthly-buy dates and skipped dates) and report settings. Read it before market_snapshot, roman_setups or spot_status."
    )]
    pub async fn get_plan(&self) -> Result<String, String> {
        let plan = self.plan()?;
        to_text(&json!({
            "path": self.paths.plan.display().to_string(),
            "plan": plan,
        }))
    }
}

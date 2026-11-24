//! The `daily_report` MCP prompt: the instructions for one run of the daily
//! routine. Generic: every number, coin, level and the closing line come from
//! the user's plan (get_plan) and the tools at run time. `orbit-tape report`
//! sends the same text to `claude -p`.

use super::OrbitTapeMcp;
use rmcp::model::{PromptMessage, Role};
use rmcp::{prompt, prompt_router};

pub const DAILY_REPORT: &str = "\
Run the daily trading routine with the Orbit Tape tools (mcp__orbit-tape__*). \
This is analysis and alerts only: place no trades, and state levels as the \
script's or the plan's output, leaving every decision to the user. Not \
financial advice.

1. Call get_plan. If it fails, reply with its error in one line and stop.
2. Call market_snapshot, roman_setups and, if the plan has a spot section, \
spot_status. If a tool fails, keep going with the others and name what failed \
in the report.
3. Alerts. Call list_alerts first. Then call create_alert at most the plan's \
report.max_new_alerts times in this run, each with exchange set to the plan's \
universe exchange, channel to the plan's report.alert_channel, frequency once, \
timeframe 1h, and the level as price. \
Never create one that duplicates an active alert on the same symbol and \
direction within 0.5% of the level, and never create spot alerts (the user \
keeps those). Priority:
   a. The closest FLIP-WATCH flip level on BTC or ETH: crosses_above when it \
flips the bias to bullish, crosses_below when to bearish. Message: \
\"12H close beyond <level> flips the bias - wait for the 12H close, then the \
pullback\".
   b. Then the closest one or two WATCH primary levels: crosses_above for a \
short level above price, crosses_below for a long level below price. Message: \
the level, the stop, T1 and T2, and \"only enter after a 1H MSS\".
4. Reply with the report, short and phone-readable, plain text, no tables:
   - One line on the market regime: BTC and ETH 12H bias, daily RSI and week %, \
plus BTC's and ETH's bias-flip levels with their distance.
   - LIVE setups first: pair, direction, entry, stop, T1 and T2 with their R \
multiples, notional per $10 risked, and the prop max leverage. Then WATCH \
pairs (primary level and distance %), then FLIP-WATCH pairs (flip level and \
distance), then COUNTER-TREND notes (entry after the next 1H MSS that way, \
stop, target, at the plan's counter-trend risk; never call one the main \
trade). If more LIVE setups point the same way than the plan's \
max_correlated, say so. Say which alerts you created, or that none were \
needed.
   - Then \"SPOT:\" with one line (price, vs the reference price, next level \
below and its distance, RSI 1D and 1W, vs SMA200 1W, next monthly buy and how \
many are done, lump-sum parts left), or on a SPOT ZONE day the zone reasons: \
which buy, the amount, and what to do.
   - If nothing is LIVE, WATCH, FLIP-WATCH, COUNTER-TREND or SPOT ZONE, reply \
with only three lines: \"No Roman Setup trades near a level today.\", BTC's flip \
level with its distance, and the one-line SPOT status.
   - End with the plan's report.footer, word for word.";

#[prompt_router(router = "report_prompt_router", vis = "pub(crate)")]
impl OrbitTapeMcp {
    #[prompt(
        name = "daily_report",
        description = "The daily trading routine: the plan, a market snapshot, Roman setups and the spot status, up to the plan's number of new alerts, and a short phone-readable report."
    )]
    pub async fn daily_report(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, DAILY_REPORT)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_only_tools_that_exist() {
        let server = crate::mcp::tests::server();
        let tools: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for tool in [
            "get_plan",
            "market_snapshot",
            "roman_setups",
            "spot_status",
            "list_alerts",
            "create_alert",
        ] {
            assert!(DAILY_REPORT.contains(tool), "{tool}");
            assert!(
                tools.iter().any(|name| name == tool),
                "{tool} is not a tool"
            );
        }
        // The condition names the prompt uses are the create_alert ones.
        for condition in ["crosses_above", "crosses_below"] {
            assert!(DAILY_REPORT.contains(condition));
        }
    }

    #[test]
    fn the_prompt_has_no_dashes_that_read_as_typography() {
        assert!(!DAILY_REPORT.contains('\u{2014}') && !DAILY_REPORT.contains('\u{2013}'));
    }
}

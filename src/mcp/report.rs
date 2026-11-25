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
report.max_new_alerts times in this run. Take each one from a coin's alerts \
list in roman_setups and pass its symbol, condition, price and message \
exactly as given (do not shorten or reword the message), with exchange set to \
the plan's universe exchange, channel to the plan's report.alert_channel, \
frequency once and timeframe 1h. \
Never create one that duplicates an active alert on the same symbol and \
direction within 0.5% of the price, and never create spot alerts (the user \
keeps those). Priority:
   a. The closest kind flip alert on BTC or ETH.
   b. Then the closest one or two kind watch alerts.
4. Reply with the report, short and phone-readable, plain text, no tables:
   - One line on the market regime: BTC and ETH 12H bias, daily RSI and week %, \
plus BTC's and ETH's bias-flip levels with their distance.
   - LIVE setups first, in roman_setups' live order (it is ranked). The first \
three in full: pair, direction, entry, stop, T1 and T2 with their R multiples, \
notional per $10 risked, and the prop max leverage. Any others in one line: \
\"Also LIVE: <pair> <direction>, ...\". Then RAN pairs in one line (\"Ran, \
skip: <pair> <direction>, ...\"). Then WATCH pairs (direction, primary level \
with its name, e.g. bear-slot breaker, and distance %), then FLIP-WATCH pairs \
(flip level and distance), then COUNTER-TREND notes (entry after the next 1H MSS that way, \
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

/// The only tools an unattended run may use: reading the plan and the
/// market, listing alerts, and creating new ones. Not changing, deleting or
/// pausing alerts, and not the indicator tools that read files.
pub const REPORT_TOOLS: [&str; 6] = [
    "mcp__orbit-tape__get_plan",
    "mcp__orbit-tape__market_snapshot",
    "mcp__orbit-tape__roman_setups",
    "mcp__orbit-tape__spot_status",
    "mcp__orbit-tape__list_alerts",
    "mcp__orbit-tape__create_alert",
];

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
        // `orbit-tape report` allows exactly the tools the prompt uses.
        for tool in REPORT_TOOLS {
            let name = tool.trim_start_matches("mcp__orbit-tape__");
            assert!(DAILY_REPORT.contains(name), "{name} unused by the prompt");
            assert!(tools.iter().any(|t| t == name), "{name} is not a tool");
        }
        // Alerts come ready from roman_setups (kinds watch and flip) and go
        // to create_alert as they are.
        for part in [
            "alerts list in roman_setups",
            "symbol, condition, price and message",
            "kind flip",
            "kind watch",
        ] {
            assert!(DAILY_REPORT.contains(part), "{part}");
        }
    }

    #[test]
    fn the_prompt_has_no_dashes_that_read_as_typography() {
        assert!(!DAILY_REPORT.contains('\u{2014}') && !DAILY_REPORT.contains('\u{2013}'));
    }
}

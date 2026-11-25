//! The user's trading plan for the daily report: prop-firm rules, the coin
//! universe, Roman Setup input overrides and a spot accumulation plan.
//!
//! It is personal, so it lives outside the data folder and outside any
//! repository: `$ORBIT_TAPE_PLAN`, else `$XDG_CONFIG_HOME/orbit-tape/plan.toml`
//! (`~/.config/orbit-tape/plan.toml`). `docs/plan.example.toml` (in the
//! `.deb`: `/usr/share/doc/orbit-tape/plan.example.toml`) shows every field.
//! Orbit Tape only reads it.

use chrono::{Datelike, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PLAN_ENV: &str = "ORBIT_TAPE_PLAN";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub prop: Prop,
    pub universe: Universe,
    #[serde(default)]
    pub roman: Roman,
    pub spot: Option<Spot>,
    #[serde(default)]
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prop {
    /// Shown in the report, e.g. the prop firm's name.
    #[serde(default)]
    pub name: String,
    /// Risk per trade, % of the account.
    pub risk_pct: f64,
    /// Risk per counter-trend trade, % of the account.
    pub counter_trend_risk_pct: f64,
    /// Most correlated positions open at once.
    pub max_correlated: u32,
    /// Setups with a wider stop are skipped, %.
    pub max_stop_pct: f64,
    /// Max leverage for coins not in `leverage`.
    pub default_leverage: f64,
    /// Max leverage by coin, e.g. `BTC = 10`.
    #[serde(default)]
    pub leverage: BTreeMap<String, f64>,
}

impl Prop {
    pub fn leverage_of(&self, coin: &str) -> f64 {
        self.leverage
            .get(&coin.to_uppercase())
            .copied()
            .unwrap_or(self.default_leverage)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Universe {
    /// `okx` or `binance` (USDT perpetuals).
    #[serde(default = "default_exchange")]
    pub exchange: String,
    /// Coins, e.g. `["BTC", "ETH"]`.
    pub coins: Vec<String>,
}

fn default_exchange() -> String {
    "okx".to_owned()
}

/// A TOML value as indicator input text: `5`, `2.5`, `true` or a string.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum InputText {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl std::fmt::Display for InputText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Text(value) => f.write_str(value),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Roman {
    /// Inputs for every coin, by input ID or label.
    #[serde(default)]
    pub inputs: BTreeMap<String, InputText>,
    /// Extra inputs for some coins (e.g. a major-coin profile).
    #[serde(default)]
    pub overrides: Vec<RomanOverride>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RomanOverride {
    pub coins: Vec<String>,
    pub inputs: BTreeMap<String, InputText>,
}

impl Roman {
    /// The inputs for one coin: the shared ones, then its overrides.
    pub fn inputs_for(&self, coin: &str) -> BTreeMap<String, String> {
        let mut inputs: BTreeMap<String, String> = self
            .inputs
            .iter()
            .map(|(key, value)| (key.clone(), value.to_string()))
            .collect();
        for over in &self.overrides {
            if over.coins.iter().any(|c| c.eq_ignore_ascii_case(coin)) {
                inputs.extend(
                    over.inputs
                        .iter()
                        .map(|(key, value)| (key.clone(), value.to_string())),
                );
            }
        }
        inputs
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spot {
    /// `okx` (spot).
    #[serde(default = "default_exchange")]
    pub exchange: String,
    /// The spot pair, e.g. `BTC-USDT`.
    pub symbol: String,
    /// Coins held now.
    pub holdings: f64,
    pub goal: Option<f64>,
    pub stretch_goal: Option<f64>,
    /// The plan's whole budget, in the quote currency.
    pub budget: Option<f64>,
    /// Reference price the report compares with (e.g. the on-chain realized
    /// price), and where it came from.
    pub realized_price: Option<f64>,
    #[serde(default)]
    pub realized_price_note: String,
    /// Price levels: heads-ups and lump-sum buys.
    #[serde(default)]
    pub levels: Vec<SpotLevel>,
    pub monthly: Option<Monthly>,
    /// SPOT ZONE when price is this close (%) to an unused buy level.
    #[serde(default = "default_zone_pct")]
    pub zone_pct: f64,
}

fn default_zone_pct() -> f64 {
    3.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelKind {
    /// A warning only, no buy.
    HeadsUp,
    /// A lump-sum buy part, used once.
    Buy,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpotLevel {
    pub price: f64,
    pub kind: LevelKind,
    #[serde(default)]
    pub label: String,
    /// Quote amount to buy (buy levels).
    pub amount: Option<f64>,
    /// The buy was done (buy levels).
    #[serde(default)]
    pub used: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Monthly {
    /// Quote amount per monthly buy.
    pub amount: f64,
    /// Planned buy dates.
    #[serde(deserialize_with = "dates")]
    pub dates: Vec<NaiveDate>,
    /// Planned dates that were skipped: not counted as done.
    #[serde(default, deserialize_with = "dates")]
    pub skipped: Vec<NaiveDate>,
    /// After the last date, unused lump-sum parts are spread over monthly
    /// buys of `amount` on the first Monday of each month.
    #[serde(default = "yes")]
    pub spread_leftover: bool,
}

/// Dates written as TOML dates (`2027-01-04`) or strings (`"2027-01-04"`).
fn dates<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<NaiveDate>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Toml(toml::value::Datetime),
        Text(String),
    }
    Vec::<Written>::deserialize(deserializer)?
        .into_iter()
        .map(|written| {
            let text = match written {
                Written::Toml(date) => date.to_string(),
                Written::Text(text) => text,
            };
            NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").map_err(|_| {
                serde::de::Error::custom(format!("'{text}' is not a date like 2027-01-04"))
            })
        })
        .collect()
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// Most alerts the daily report may create in one run.
    #[serde(default = "default_max_alerts")]
    pub max_new_alerts: u32,
    /// Where the report's alerts go: desktop, ntfy or both.
    #[serde(default = "default_channel")]
    pub alert_channel: String,
    /// The report's last line.
    #[serde(default = "default_footer")]
    pub footer: String,
    /// The Claude model `orbit-tape report` runs with (`claude --model`):
    /// an alias such as sonnet, opus or haiku, or a full model name.
    #[serde(default = "default_model")]
    pub model: String,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            max_new_alerts: default_max_alerts(),
            alert_channel: default_channel(),
            footer: default_footer(),
            model: default_model(),
        }
    }
}

fn default_max_alerts() -> u32 {
    2
}

fn default_channel() -> String {
    "both".to_owned()
}

/// Cheaper than the default model; the routine is mostly tool calls.
fn default_model() -> String {
    "sonnet".to_owned()
}

fn default_footer() -> String {
    "Script / plan output, not financial advice - the decision is yours.".to_owned()
}

/// Where the plan is read from.
pub fn path() -> PathBuf {
    if let Some(path) = std::env::var_os(PLAN_ENV).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("orbit-tape")
        .join("plan.toml")
}

impl Plan {
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "no plan at {}: copy plan.example.toml (/usr/share/doc/orbit-tape/ or docs/ in the source) there, or set {PLAN_ENV}, and fill it in",
                    path.display()
                )
            } else {
                format!("could not read {}: {error}", path.display())
            }
        })?;
        Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut plan: Plan = toml::from_str(text).map_err(|error| error.to_string())?;
        plan.normalize();
        plan.check()?;
        Ok(plan)
    }

    fn normalize(&mut self) {
        for coin in &mut self.universe.coins {
            *coin = coin.trim().to_uppercase();
        }
        self.prop.leverage = std::mem::take(&mut self.prop.leverage)
            .into_iter()
            .map(|(coin, leverage)| (coin.trim().to_uppercase(), leverage))
            .collect();
        self.universe.exchange = self.universe.exchange.trim().to_lowercase();
        if let Some(spot) = &mut self.spot {
            spot.exchange = spot.exchange.trim().to_lowercase();
            spot.symbol = spot.symbol.trim().to_uppercase();
            spot.levels.sort_by(|a, b| b.price.total_cmp(&a.price));
            if let Some(monthly) = &mut spot.monthly {
                monthly.dates.sort();
                monthly.dates.dedup();
            }
        }
    }

    fn check(&self) -> Result<(), String> {
        let positive = |what: &str, value: f64| {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(format!("{what} must be a positive number, got {value}"))
            }
        };
        let prop = &self.prop;
        positive("prop.risk_pct", prop.risk_pct)?;
        positive("prop.counter_trend_risk_pct", prop.counter_trend_risk_pct)?;
        positive("prop.max_stop_pct", prop.max_stop_pct)?;
        positive("prop.default_leverage", prop.default_leverage)?;
        for (coin, leverage) in &prop.leverage {
            positive(&format!("prop.leverage.{coin}"), *leverage)?;
        }
        if self.universe.coins.is_empty() {
            return Err("universe.coins is empty".to_owned());
        }
        if !matches!(self.universe.exchange.as_str(), "okx" | "binance") {
            return Err(format!(
                "universe.exchange must be okx or binance, got '{}'",
                self.universe.exchange
            ));
        }
        if !matches!(
            self.report.alert_channel.as_str(),
            "desktop" | "ntfy" | "both"
        ) {
            return Err(format!(
                "report.alert_channel must be desktop, ntfy or both, got '{}'",
                self.report.alert_channel
            ));
        }
        if self.report.model.trim().is_empty() || self.report.model.chars().any(char::is_whitespace)
        {
            return Err(format!(
                "report.model must be a model name such as sonnet, got '{}'",
                self.report.model
            ));
        }
        if let Some(spot) = &self.spot {
            if spot.exchange != "okx" {
                return Err(format!(
                    "spot.exchange must be okx, got '{}'",
                    spot.exchange
                ));
            }
            if spot.symbol.split('-').count() != 2 {
                return Err(format!(
                    "spot.symbol must look like BTC-USDT, got '{}'",
                    spot.symbol
                ));
            }
            if !(spot.holdings.is_finite() && spot.holdings >= 0.0) {
                return Err("spot.holdings must be 0 or more".to_owned());
            }
            if let Some(price) = spot.realized_price {
                positive("spot.realized_price", price)?;
            }
            positive("spot.zone_pct", spot.zone_pct)?;
            for level in &spot.levels {
                positive("spot.levels price", level.price)?;
                match (level.kind, level.amount) {
                    (LevelKind::Buy, Some(amount)) => positive("spot.levels amount", amount)?,
                    (LevelKind::Buy, None) => {
                        return Err(format!(
                            "spot level {} is a buy level but has no amount",
                            level.price
                        ));
                    }
                    (LevelKind::HeadsUp, _) => {}
                }
            }
            if let Some(monthly) = &spot.monthly {
                positive("spot.monthly.amount", monthly.amount)?;
                if let Some(date) = monthly.skipped.iter().find(|d| !monthly.dates.contains(d)) {
                    return Err(format!(
                        "spot.monthly.skipped has {date}, which is not in spot.monthly.dates"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The first Monday of the month after `date`.
pub fn first_monday_after_month_of(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid date");
    let offset = (7 + Weekday::Mon.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    first + chrono::Duration::days(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = include_str!("../../../docs/plan.example.toml");

    #[test]
    fn the_example_plan_parses() {
        let plan = Plan::parse(EXAMPLE).expect("example plan is valid");
        assert!(!plan.universe.coins.is_empty());
        assert_eq!(plan.prop.leverage_of("btc"), plan.prop.leverage["BTC"]);
        assert_eq!(plan.prop.leverage_of("NOPE"), plan.prop.default_leverage);
        let spot = plan.spot.expect("example has a spot plan");
        // Levels sorted high to low.
        assert!(spot.levels.windows(2).all(|w| w[0].price >= w[1].price));
    }

    #[test]
    fn report_model_defaults_to_sonnet() {
        let plan = Plan::parse(EXAMPLE).unwrap();
        assert_eq!(plan.report.model, "sonnet");
        let without = EXAMPLE.replace("model = \"sonnet\"", "");
        assert_eq!(Plan::parse(&without).unwrap().report.model, "sonnet");
        let opus = EXAMPLE.replace("model = \"sonnet\"", "model = \"opus\"");
        assert_eq!(Plan::parse(&opus).unwrap().report.model, "opus");
        let bad = EXAMPLE.replace("model = \"sonnet\"", "model = \"two words\"");
        assert!(Plan::parse(&bad).unwrap_err().contains("report.model"));
    }

    #[test]
    fn roman_inputs_merge_overrides() {
        let plan = Plan::parse(EXAMPLE).unwrap();
        let btc = plan.roman.inputs_for("BTC");
        let alt = plan.roman.inputs_for("ALTX");
        assert_eq!(btc["mssGate"], "Zone tap or SFP");
        assert_eq!(btc["maxDist"], "5");
        assert!(!alt.contains_key("maxDist"));
        assert_eq!(alt["ctMode"], "true");
    }

    #[test]
    fn mistakes_are_named() {
        let error = |text: &str| Plan::parse(text).unwrap_err();
        assert!(error("").contains("missing field"));
        let base = r#"
            [prop]
            risk_pct = 0.5
            counter_trend_risk_pct = 0.25
            max_correlated = 2
            max_stop_pct = 5
            default_leverage = 5
            [universe]
            coins = ["BTC"]
        "#;
        Plan::parse(base).unwrap();
        assert!(error(&base.replace("risk_pct = 0.5", "risk_pct = -1")).contains("prop.risk_pct"));
        assert!(error(&format!("{base}\nsurprise = 1")).contains("surprise"));
        assert!(
            error(&format!(
                "{base}\n[spot]\nsymbol = \"BTC-USDT\"\nholdings = 0.1\n[[spot.levels]]\nprice = 50\nkind = \"buy\""
            ))
            .contains("has no amount")
        );
        assert!(
            error(&format!(
                "{base}\n[spot]\nsymbol = \"BTC-USDT\"\nholdings = 0.1\n[spot.monthly]\namount = 1\ndates = [2026-12-07]\nskipped = [2026-12-14]"
            ))
            .contains("not in spot.monthly.dates")
        );
    }

    #[test]
    fn a_missing_plan_says_where_to_put_it() {
        let error = Plan::load_from(Path::new("/nonexistent/plan.toml")).unwrap_err();
        assert!(error.contains("plan.example.toml"), "{error}");
    }

    #[test]
    fn first_monday_of_next_month() {
        let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(
            first_monday_after_month_of(date(2027, 5, 3)),
            date(2027, 6, 7)
        );
        assert_eq!(
            first_monday_after_month_of(date(2027, 1, 4)),
            date(2027, 2, 1)
        );
        assert_eq!(
            first_monday_after_month_of(date(2026, 12, 7)),
            date(2027, 1, 4)
        );
    }
}

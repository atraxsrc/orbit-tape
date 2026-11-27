//! The watchlist: the favorited tickers in named sections, in the user's
//! order. The favorites (`tickers_table::Settings::favorited_tickers`) say
//! what is on it; this keeps the sections and the order. Starring a ticker
//! adds it to the last section, unstarring removes it.

use crate::config::theme::ThemeColor;
use crate::layout::pane::LinkGroup;
use exchange::Ticker;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Name of the section that new installs and new favorites start in.
pub const DEFAULT_SECTION: &str = "Favorites";

/// Name of a section added with "+ Section".
pub const NEW_SECTION: &str = "New section";

/// Panel width in pixels: default and the drag limits.
pub const DEFAULT_WIDTH: f32 = 340.0;
pub const MIN_WIDTH: f32 = 240.0;
pub const MAX_WIDTH: f32 = 720.0;

/// Widths of the Last, Chg and Chg% columns; Symbol takes the rest.
pub const DEFAULT_COLUMNS: [f32; 3] = [82.0, 72.0, 64.0];
pub const MIN_COLUMN: f32 = 40.0;
pub const MAX_COLUMN: f32 = 240.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watchlist {
    pub sections: Vec<Section>,
    /// Whether the panel is open; kept so it reopens on the next launch.
    pub open: bool,
    /// Panel width, set by dragging its edge.
    pub width: f32,
    /// Clicking a ticker loads it into the panes of this link group (the
    /// number on each pane); with none, or no pane in it, the focused pane.
    pub link_group: Option<LinkGroup>,
    /// Row order inside each section; `None` keeps the user's order.
    pub sort: Option<Sort>,
    /// Last, Chg and Chg% column widths, set by dragging the header borders.
    pub columns: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortColumn {
    Symbol,
    Last,
    Change,
    ChangePct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub column: SortColumn,
    pub order: SortOrder,
}

/// What a row sorts by. A missing number (no price yet) sorts last in
/// both orders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SortKeys {
    pub symbol: String,
    pub last: Option<f64>,
    pub change: Option<f64>,
    pub change_pct: Option<f64>,
}

/// Sorts rows by `sort`; ties and missing numbers fall back to the symbol.
pub fn sort_rows<T>(rows: &mut [(T, SortKeys)], sort: Sort) {
    use std::cmp::Ordering;

    let number = |keys: &SortKeys| match sort.column {
        SortColumn::Symbol => None,
        SortColumn::Last => keys.last,
        SortColumn::Change => keys.change,
        SortColumn::ChangePct => keys.change_pct,
    };
    let ordered = |ordering: Ordering| match sort.order {
        SortOrder::Ascending => ordering,
        SortOrder::Descending => ordering.reverse(),
    };

    rows.sort_by(|(_, a), (_, b)| {
        let by_symbol = || a.symbol.cmp(&b.symbol);
        if sort.column == SortColumn::Symbol {
            return ordered(by_symbol());
        }
        match (number(a), number(b)) {
            (Some(x), Some(y)) => ordered(x.total_cmp(&y)).then_with(by_symbol),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => by_symbol(),
        }
    });
}

impl Default for Watchlist {
    fn default() -> Self {
        Self {
            sections: Vec::new(),
            open: false,
            width: DEFAULT_WIDTH,
            link_group: Some(LinkGroup::A),
            sort: None,
            columns: DEFAULT_COLUMNS,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Section {
    pub name: String,
    pub collapsed: bool,
    pub tickers: Vec<Ticker>,
    /// Color of the section title; `None` is the normal text color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ThemeColor>,
}

impl Section {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// A short symbol like TradingView's: ("AAVE", "USDT.P") for the OKX perp
/// `AAVE-USDT-SWAP` or Binance's `AAVEUSDT`, ("BTC", "USDT") for spot.
pub fn short_label(symbol: &str, perp: bool) -> (String, String) {
    const QUOTES: [&str; 3] = ["USDT", "USDC", "USD"];
    let symbol = symbol.strip_suffix("-SWAP").unwrap_or(symbol);
    let (base, quote) = match symbol.split_once('-') {
        Some((base, quote)) => (base.to_owned(), quote.replace('-', "")),
        None => QUOTES
            .iter()
            .find_map(|quote| {
                symbol
                    .strip_suffix(quote)
                    .filter(|base| !base.is_empty())
                    .map(|base| (base.to_owned(), (*quote).to_owned()))
            })
            .unwrap_or_else(|| (symbol.to_owned(), String::new())),
    };
    let suffix = if perp { format!("{quote}.P") } else { quote };
    (base, suffix)
}

/// Writes a run of 4 or more zeros after "0." as a subscript count, like
/// TradingView: "0.000002205" -> "0.0₅2205". Keeps a leading sign.
pub fn compact_zeros(value: &str) -> String {
    const SUBSCRIPTS: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
    let (sign, digits) = match value.chars().next() {
        Some(c @ ('+' | '-')) => (c.to_string(), &value[1..]),
        _ => (String::new(), value),
    };
    let Some(fraction) = digits.strip_prefix("0.") else {
        return value.to_owned();
    };
    let zeros = fraction.chars().take_while(|c| *c == '0').count();
    if zeros < 4 || zeros == fraction.len() {
        return value.to_owned();
    }
    let count: String = zeros
        .to_string()
        .chars()
        .map(|d| SUBSCRIPTS[d.to_digit(10).unwrap_or(0) as usize])
        .collect();
    format!("{sign}0.0{count}{}", &fraction[zeros..])
}

impl Watchlist {
    /// A header click: ascending, then descending, then the user's order.
    /// Another column starts ascending.
    pub fn cycle_sort(&mut self, column: SortColumn) {
        self.sort = match self.sort {
            Some(Sort {
                column: current,
                order: SortOrder::Ascending,
            }) if current == column => Some(Sort {
                column,
                order: SortOrder::Descending,
            }),
            Some(Sort {
                column: current,
                order: SortOrder::Descending,
            }) if current == column => None,
            _ => Some(Sort {
                column,
                order: SortOrder::Ascending,
            }),
        };
    }

    /// A drag on the header border left of number column `index` (0 Last,
    /// 1 Chg, 2 Chg%): the numbers are right-aligned, so dragging the border
    /// left (negative) widens the column.
    pub fn resize_column(&mut self, index: usize, dx: f32) -> bool {
        let Some(width) = self.columns.get_mut(index) else {
            return false;
        };
        let new = (*width - dx).clamp(MIN_COLUMN, MAX_COLUMN);
        let changed = new != *width;
        *width = new;
        changed
    }

    /// Widens (positive) or narrows the panel, within the limits.
    pub fn resize(&mut self, dx: f32) -> bool {
        let width = (self.width + dx).clamp(MIN_WIDTH, MAX_WIDTH);
        let changed = width != self.width;
        self.width = width;
        changed
    }

    /// Brings the list in line with the favorites: drops tickers that are
    /// no longer favorites (and repeats), and adds new favorites to the last
    /// section, sorted by name. Returns whether anything changed.
    pub fn sync<'a>(&mut self, favorites: impl IntoIterator<Item = &'a Ticker>) -> bool {
        let favorites: HashSet<Ticker> = favorites.into_iter().copied().collect();
        let before = self.sections.clone();

        let mut seen = HashSet::new();
        for section in &mut self.sections {
            section
                .tickers
                .retain(|ticker| favorites.contains(ticker) && seen.insert(*ticker));
        }

        let mut new: Vec<Ticker> = favorites
            .into_iter()
            .filter(|ticker| !seen.contains(ticker))
            .collect();
        new.sort_by_key(|ticker| ticker.to_string());

        if self.sections.is_empty() {
            self.sections.push(Section::named(DEFAULT_SECTION));
        }
        if let Some(last) = self.sections.last_mut() {
            last.tickers.extend(new);
        }

        self.sections != before
    }

    pub fn len(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.tickers.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn position(&self, ticker: Ticker) -> Option<(usize, usize)> {
        self.sections.iter().enumerate().find_map(|(s, section)| {
            section
                .tickers
                .iter()
                .position(|t| *t == ticker)
                .map(|i| (s, i))
        })
    }

    /// Moves a ticker one row up or down; past the top or bottom of its
    /// section it moves into the section before or after.
    pub fn move_ticker(&mut self, ticker: Ticker, direction: Direction) -> bool {
        let Some((s, i)) = self.position(ticker) else {
            return false;
        };
        let len = self.sections[s].tickers.len();
        match direction {
            Direction::Up if i > 0 => self.sections[s].tickers.swap(i - 1, i),
            Direction::Up if s > 0 => {
                let ticker = self.sections[s].tickers.remove(i);
                self.sections[s - 1].tickers.push(ticker);
            }
            Direction::Down if i + 1 < len => self.sections[s].tickers.swap(i, i + 1),
            Direction::Down if s + 1 < self.sections.len() => {
                let ticker = self.sections[s].tickers.remove(i);
                self.sections[s + 1].tickers.insert(0, ticker);
            }
            _ => return false,
        }
        true
    }

    /// A section dragged by its title and dropped at `target` (the drag
    /// list's drop index).
    pub fn drop_section(&mut self, index: usize, target: usize) -> bool {
        // Dropping just above or below itself keeps the order.
        if target == index || target == index + 1 {
            return false;
        }
        crate::util::reorder_vec(&mut self.sections, index, target).is_some()
    }

    pub fn set_section_color(&mut self, index: usize, color: Option<ThemeColor>) -> bool {
        match self.sections.get_mut(index) {
            Some(section) if section.color != color => {
                section.color = color;
                true
            }
            _ => false,
        }
    }

    pub fn move_section(&mut self, index: usize, direction: Direction) -> bool {
        let other = match direction {
            Direction::Up if index > 0 => index - 1,
            Direction::Down if index + 1 < self.sections.len() => index + 1,
            _ => return false,
        };
        self.sections.swap(index, other);
        true
    }

    pub fn add_section(&mut self) {
        self.sections.push(Section::named(NEW_SECTION));
    }

    pub fn rename_section(&mut self, index: usize, name: String) -> bool {
        match self.sections.get_mut(index) {
            Some(section) if section.name != name => {
                section.name = name;
                true
            }
            _ => false,
        }
    }

    /// Removes a section; its tickers go to the end of the section before
    /// it (the start of the next one for the first). The last section stays.
    pub fn remove_section(&mut self, index: usize) -> bool {
        if self.sections.len() < 2 || index >= self.sections.len() {
            return false;
        }
        let removed = self.sections.remove(index);
        if index > 0 {
            self.sections[index - 1].tickers.extend(removed.tickers);
        } else {
            self.sections[0].tickers.splice(0..0, removed.tickers);
        }
        true
    }

    pub fn toggle_collapsed(&mut self, index: usize) -> bool {
        match self.sections.get_mut(index) {
            Some(section) => {
                section.collapsed = !section.collapsed;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::adapter::Exchange;

    fn t(symbol: &str) -> Ticker {
        Ticker::new(symbol, Exchange::OkexLinear)
    }

    fn names(list: &Watchlist) -> Vec<Vec<String>> {
        list.sections
            .iter()
            .map(|section| {
                section
                    .tickers
                    .iter()
                    .map(|ticker| ticker.display_symbol_and_type().0)
                    .collect()
            })
            .collect()
    }

    fn two_sections() -> Watchlist {
        Watchlist {
            sections: vec![
                Section {
                    name: "Crypto".into(),
                    collapsed: false,
                    tickers: vec![t("BTC-USDT-SWAP"), t("ETH-USDT-SWAP")],
                    color: None,
                },
                Section {
                    name: "Metals".into(),
                    collapsed: false,
                    tickers: vec![t("XAU-USDT-SWAP"), t("XAG-USDT-SWAP")],
                    color: None,
                },
            ],
            open: true,
            width: DEFAULT_WIDTH,
            link_group: Some(LinkGroup::A),
            sort: None,
            columns: DEFAULT_COLUMNS,
        }
    }

    #[test]
    fn sync_starts_a_section_from_the_favorites() {
        let mut list = Watchlist::default();
        let favorites = [t("XAU-USDT-SWAP"), t("BTC-USDT-SWAP")];
        assert!(list.sync(&favorites));
        assert_eq!(list.sections.len(), 1);
        assert_eq!(list.sections[0].name, DEFAULT_SECTION);
        assert_eq!(list.len(), 2);
        // Sorted by name.
        assert_eq!(list.sections[0].tickers[0], t("BTC-USDT-SWAP"));
        // Nothing new: no change.
        assert!(!list.sync(&favorites));
    }

    #[test]
    fn sync_drops_unstarred_and_appends_new_favorites() {
        let mut list = two_sections();
        let favorites = [
            t("BTC-USDT-SWAP"),
            t("XAU-USDT-SWAP"),
            t("XAG-USDT-SWAP"),
            t("NVDA-USDT-SWAP"),
        ];
        assert!(list.sync(&favorites));
        assert_eq!(names(&list)[0].len(), 1, "ETH was unstarred");
        assert_eq!(list.sections[1].tickers.last(), Some(&t("NVDA-USDT-SWAP")));
        assert_eq!(list.sections[0].name, "Crypto", "sections keep their names");
    }

    #[test]
    fn sync_removes_repeats() {
        let mut list = two_sections();
        list.sections[1].tickers.push(t("BTC-USDT-SWAP"));
        let favorites: Vec<Ticker> = two_sections().sections[0]
            .tickers
            .iter()
            .chain(&two_sections().sections[1].tickers)
            .copied()
            .collect();
        assert!(list.sync(&favorites));
        assert_eq!(list, two_sections());
    }

    #[test]
    fn moving_a_ticker_crosses_section_edges() {
        let mut list = two_sections();
        // Down past the end of Crypto: first in Metals.
        assert!(list.move_ticker(t("ETH-USDT-SWAP"), Direction::Down));
        assert_eq!(list.sections[1].tickers[0], t("ETH-USDT-SWAP"));
        // Up past the top of Metals: last in Crypto again.
        assert!(list.move_ticker(t("ETH-USDT-SWAP"), Direction::Up));
        assert_eq!(list, two_sections());
        // Within a section.
        assert!(list.move_ticker(t("XAG-USDT-SWAP"), Direction::Up));
        assert_eq!(list.sections[1].tickers[0], t("XAG-USDT-SWAP"));
        // Nowhere to go.
        assert!(!list.move_ticker(t("BTC-USDT-SWAP"), Direction::Up));
        assert!(!list.move_ticker(t("NOPE-USDT-SWAP"), Direction::Down));
    }

    #[test]
    fn sections_move_rename_and_remove() {
        let mut list = two_sections();
        assert!(list.move_section(1, Direction::Up));
        assert_eq!(list.sections[0].name, "Metals");
        assert!(!list.move_section(0, Direction::Up));

        assert!(list.rename_section(0, "Gold & co".into()));
        assert!(!list.rename_section(0, "Gold & co".into()));

        list.add_section();
        assert_eq!(list.sections[2].name, NEW_SECTION);

        // Removing the first section moves its tickers to the next one's top.
        assert!(list.remove_section(0));
        assert_eq!(
            names(&list)[0],
            [
                "XAU-USDT-SWAP",
                "XAG-USDT-SWAP",
                "BTC-USDT-SWAP",
                "ETH-USDT-SWAP"
            ]
        );
        // Removing a later one moves them to the end of the one before.
        list.sections[1].tickers.push(t("SOL-USDT-SWAP"));
        assert!(list.remove_section(1));
        assert_eq!(list.sections.len(), 1);
        assert_eq!(list.len(), 5);
        // The last section stays.
        assert!(!list.remove_section(0));
    }

    #[test]
    fn short_labels_read_like_tradingview() {
        let label = |s: &str, perp| short_label(s, perp);
        assert_eq!(
            label("AAVE-USDT-SWAP", true),
            ("AAVE".into(), "USDT.P".into())
        );
        assert_eq!(label("BTC-USDT", false), ("BTC".into(), "USDT".into()));
        assert_eq!(label("AAVEUSDT", true), ("AAVE".into(), "USDT.P".into()));
        assert_eq!(label("BTCUSDC", false), ("BTC".into(), "USDC".into()));
        assert_eq!(
            label("BTCUSD_PERP", true),
            ("BTCUSD_PERP".into(), ".P".into())
        );
        assert_eq!(label("HYPE", true), ("HYPE".into(), ".P".into()));
        assert_eq!(label("USDT", false), ("USDT".into(), String::new()));
    }

    #[test]
    fn long_zero_runs_get_a_subscript_count() {
        assert_eq!(compact_zeros("0.000002205"), "0.0₅2205");
        assert_eq!(compact_zeros("+0.000000030"), "+0.0₇30");
        assert_eq!(compact_zeros("-0.00001683"), "-0.0₄1683");
        assert_eq!(compact_zeros("0.000000000012"), "0.0₁₀12");
        // Short runs, whole numbers and zero stay.
        assert_eq!(compact_zeros("0.0001"), "0.0001");
        assert_eq!(compact_zeros("61405.9"), "61405.9");
        assert_eq!(compact_zeros("0.00000000"), "0.00000000");
    }

    #[test]
    fn width_stays_within_limits() {
        let mut list = Watchlist::default();
        assert_eq!(list.width, DEFAULT_WIDTH);
        assert!(list.resize(50.0));
        assert_eq!(list.width, DEFAULT_WIDTH + 50.0);
        list.resize(-10_000.0);
        assert_eq!(list.width, MIN_WIDTH);
        assert!(!list.resize(-1.0));
        list.resize(10_000.0);
        assert_eq!(list.width, MAX_WIDTH);
        // Saved states from before the width keep the default.
        let old: Watchlist = serde_json::from_str(r#"{"open":true}"#).unwrap();
        assert_eq!(old.width, DEFAULT_WIDTH);
        assert_eq!(old.link_group, Some(LinkGroup::A), "defaults to group 1");
    }

    #[test]
    fn header_clicks_cycle_the_sort() {
        let mut list = Watchlist::default();
        let sort = |column, order| Some(Sort { column, order });
        list.cycle_sort(SortColumn::Last);
        assert_eq!(list.sort, sort(SortColumn::Last, SortOrder::Ascending));
        list.cycle_sort(SortColumn::Last);
        assert_eq!(list.sort, sort(SortColumn::Last, SortOrder::Descending));
        list.cycle_sort(SortColumn::Last);
        assert_eq!(list.sort, None);
        list.cycle_sort(SortColumn::Last);
        list.cycle_sort(SortColumn::Symbol);
        assert_eq!(list.sort, sort(SortColumn::Symbol, SortOrder::Ascending));
    }

    #[test]
    fn rows_sort_by_each_column_with_missing_prices_last() {
        let keys = |symbol: &str, last: Option<f64>, change_pct: Option<f64>| SortKeys {
            symbol: symbol.into(),
            last,
            change: last.map(|last| last / 100.0),
            change_pct,
        };
        let rows = vec![
            ("eth", keys("ETH", Some(1720.0), Some(1.9))),
            ("aave", keys("AAVE", Some(97.0), Some(-0.4))),
            ("new", keys("NEW", None, None)),
            ("btc", keys("BTC", Some(61405.0), Some(1.4))),
        ];
        let order = |column, order| {
            let mut rows = rows.clone();
            sort_rows(&mut rows, Sort { column, order });
            rows.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        };
        use SortColumn::*;
        use SortOrder::*;
        assert_eq!(order(Symbol, Ascending), ["aave", "btc", "eth", "new"]);
        assert_eq!(order(Symbol, Descending), ["new", "eth", "btc", "aave"]);
        assert_eq!(order(Last, Ascending), ["aave", "eth", "btc", "new"]);
        assert_eq!(order(Last, Descending), ["btc", "eth", "aave", "new"]);
        assert_eq!(order(Change, Descending), ["btc", "eth", "aave", "new"]);
        assert_eq!(order(ChangePct, Ascending), ["aave", "btc", "eth", "new"]);
        assert_eq!(order(ChangePct, Descending), ["eth", "btc", "aave", "new"]);
    }

    #[test]
    fn column_borders_resize_the_column_to_their_right() {
        let mut list = Watchlist::default();
        assert!(list.resize_column(0, -20.0), "dragged left: Last wider");
        assert_eq!(list.columns[0], DEFAULT_COLUMNS[0] + 20.0);
        assert!(list.resize_column(2, 10.0), "dragged right: Chg% narrower");
        assert_eq!(list.columns[2], DEFAULT_COLUMNS[2] - 10.0);
        list.resize_column(1, 1_000.0);
        assert_eq!(list.columns[1], MIN_COLUMN);
        list.resize_column(1, -1_000.0);
        assert_eq!(list.columns[1], MAX_COLUMN);
        assert!(!list.resize_column(3, 5.0));
        let old: Watchlist = serde_json::from_str(r#"{"open":true}"#).unwrap();
        assert_eq!(old.columns, DEFAULT_COLUMNS);
    }

    #[test]
    fn sections_drop_where_dragged_and_take_a_color() {
        use crate::config::theme::{ThemeColorRole, ThemeColorShade};
        let mut list = two_sections();
        list.add_section();
        let names = |list: &Watchlist| {
            list.sections
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        };
        // The last section dragged to the top.
        assert!(list.drop_section(2, 0));
        assert_eq!(names(&list), [NEW_SECTION, "Crypto", "Metals"]);
        // Dropped where it was: no change.
        assert!(!list.drop_section(1, 1));
        assert!(!list.drop_section(9, 0));
        assert_eq!(list.len(), 4, "tickers move with their sections");

        let red = ThemeColor {
            role: ThemeColorRole::Danger,
            shade: ThemeColorShade::Base,
            accent: None,
        };
        assert!(list.set_section_color(1, Some(red)));
        assert!(!list.set_section_color(1, Some(red)));
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<Watchlist>(&json).unwrap(), list);
        assert!(list.set_section_color(1, None));
    }

    #[test]
    fn collapse_toggles() {
        let mut list = two_sections();
        assert!(list.toggle_collapsed(1));
        assert!(list.sections[1].collapsed);
        assert!(!list.toggle_collapsed(9));
    }

    #[test]
    fn saved_form_round_trips_and_old_state_loads() {
        let list = two_sections();
        let json = serde_json::to_string(&list).unwrap();
        assert!(json.contains("OKXLinear:XAU-USDT-SWAP"), "{json}");
        assert_eq!(serde_json::from_str::<Watchlist>(&json).unwrap(), list);
        assert_eq!(
            serde_json::from_str::<Watchlist>("{}").unwrap(),
            Watchlist::default()
        );
    }
}

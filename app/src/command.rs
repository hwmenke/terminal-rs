//! Bloomberg-style command line: `<SECURITY> [<FUNCTION> args] <GO>`.
//!
//! Examples: `AAPL`, `AAPL US EQUITY <GO>`, `5`, `1W`, `RSI`, `SMA 50`, `ICHIMOKU`, `IND MOMENTUM`,
//! `CMP MSFT`, `AAPL CONG`, `CONG`, `POL PELOSI`, `TRACK PELOSI`, `LAYOUT 4`, `FIB`, `HELP`.

use tr_chart::drawings::Tool;
use tr_chart::scene::ChartType;
pub use tr_core::indicators::registry::Spec as IndicatorSpec;
use tr_core::{Security, Timeframe, YellowKey};

/// Bound search, shaping and command parsing work for pasted input.
pub const MAX_INPUT_BYTES: usize = 4096;

/// Indicators on a new chart: the 20 and 50 bar simple moving averages.
pub fn default_indicators() -> Vec<IndicatorSpec> {
    [20.0, 50.0].iter().filter_map(|n| IndicatorSpec::new("SMA", &[*n])).collect()
}

/// Views of the brokerage portfolio pane (`PORT`, `ACCT`, `POS`, `ORD`, `ACTV`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PortView {
    #[default]
    Summary,
    Accounts,
    Positions,
    Orders,
    Activity,
}

impl PortView {
    pub const ALL: [PortView; 5] =
        [PortView::Summary, PortView::Accounts, PortView::Positions, PortView::Orders, PortView::Activity];

    pub fn label(self) -> &'static str {
        match self {
            PortView::Summary => "Summary",
            PortView::Accounts => "Accounts",
            PortView::Positions => "Positions",
            PortView::Orders => "Orders",
            PortView::Activity => "Activity",
        }
    }

    /// The function code shown on the tab and typed on the command line.
    pub fn code(self) -> &'static str {
        match self {
            PortView::Summary => "PORT",
            PortView::Accounts => "ACCT",
            PortView::Positions => "POS",
            PortView::Orders => "ORD",
            PortView::Activity => "ACTV",
        }
    }
}

/// `BUY AAPL 10`, `SELL 5 MSFT LMT 410`, `BUY NVDA $500`, `TRADE AAPL`: fields for the order
/// ticket. A command only fills the ticket; orders are placed from the ticket after a preview
/// and a confirmation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeArgs {
    /// `Some(true)` buy, `Some(false)` sell, `None` the ticket's default.
    pub buy: Option<bool>,
    /// Market key (`AAPL US Equity`) of the security, when one was given.
    pub symbol: Option<String>,
    pub units: Option<f64>,
    /// Dollar amount (`$500`).
    pub notional: Option<f64>,
    pub limit: Option<f64>,
    pub stop: Option<f64>,
    /// `SELL ALL TSLA`: the whole position.
    pub all: bool,
}

impl TradeArgs {
    /// Parse the words after `BUY`/`SELL`/`TRADE`. Unknown words make the command invalid so
    /// a typo never fills the ticket with something unintended.
    fn parse(buy: Option<bool>, rest: &[&str]) -> Option<TradeArgs> {
        let mut t = TradeArgs { buy, ..Default::default() };
        let price =
            |s: &str| s.trim_start_matches(['@', '$']).replace(',', "").parse::<f64>().ok().filter(|x| *x > 0.0);
        let mut words = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            let w = rest[i];
            let up = w.to_ascii_uppercase();
            match up.as_str() {
                "LMT" | "LIMIT" | "@" => {
                    t.limit = Some(price(rest.get(i + 1)?)?);
                    i += 1;
                }
                "STP" | "STOP" => {
                    t.stop = Some(price(rest.get(i + 1)?)?);
                    i += 1;
                }
                "MKT" | "MARKET" | "SHARES" | "SHS" | "SH" => {}
                _ if up.starts_with('@') => t.limit = Some(price(&up)?),
                _ if up.starts_with('$') => t.notional = Some(price(&up)?),
                _ if up.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') => {
                    if t.units.is_some() {
                        return None;
                    }
                    t.units = Some(price(&up)?);
                }
                _ => words.push(w),
            }
            i += 1;
        }
        if t.units.is_some() && t.notional.is_some() {
            return None;
        }
        // `ALL` means the whole position when another word names the security (`ALL` alone
        // is Allstate's ticker).
        if words.len() > 1
            && let Some(i) = words.iter().position(|w| w.eq_ignore_ascii_case("ALL"))
        {
            if t.buy == Some(true) || t.units.is_some() || t.notional.is_some() {
                return None;
            }
            words.remove(i);
            t.all = true;
            t.buy = Some(false);
        }
        if !words.is_empty() {
            let (sec, used) = Security::resolve_tokens(&words)?;
            if used != words.len() {
                return None;
            }
            t.symbol = Some(sec.key());
        }
        Some(t)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A market key, e.g. `BTCUSD Curncy`.
    Symbol(String),
    SymbolFunction(String, Function),
    Function(Function),
}

/// Quick range: span of history plus the interval used to show it (mirrors common charting presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    D1,
    D5,
    M1,
    M3,
    M6,
    Ytd,
    Y1,
    Y5,
    All,
}

impl Range {
    pub const ALL: [Range; 9] =
        [Range::D1, Range::D5, Range::M1, Range::M3, Range::M6, Range::Ytd, Range::Y1, Range::Y5, Range::All];

    pub fn label(self) -> &'static str {
        match self {
            Range::D1 => "1D",
            Range::D5 => "5D",
            Range::M1 => "1M",
            Range::M3 => "3M",
            Range::M6 => "6M",
            Range::Ytd => "YTD",
            Range::Y1 => "1Y",
            Range::Y5 => "5Y",
            Range::All => "ALL",
        }
    }

    pub fn timeframe(self) -> Timeframe {
        match self {
            Range::D1 => Timeframe::M1,
            Range::D5 => Timeframe::M5,
            Range::M1 => Timeframe::M30,
            Range::M3 => Timeframe::H1,
            Range::M6 => Timeframe::H4,
            Range::Ytd | Range::Y1 => Timeframe::D1,
            Range::Y5 => Timeframe::W1,
            Range::All => Timeframe::MN1,
        }
    }

    /// Approximate seconds of history to show (`None` = everything).
    pub fn span_secs(self, now: i64) -> Option<i64> {
        const D: i64 = 86_400;
        Some(match self {
            Range::D1 => D,
            Range::D5 => 5 * D,
            Range::M1 => 30 * D,
            Range::M3 => 91 * D,
            Range::M6 => 182 * D,
            Range::Ytd => {
                use chrono::Datelike;
                let y = chrono::DateTime::from_timestamp(now, 0).unwrap_or_default().year();
                now - chrono::NaiveDate::from_ymd_opt(y, 1, 1)?.and_hms_opt(0, 0, 0)?.and_utc().timestamp()
            }
            Range::Y1 => 365 * D,
            Range::Y5 => 5 * 365 * D,
            Range::All => return None,
        })
    }

    fn parse(s: &str) -> Option<Range> {
        Range::ALL.into_iter().find(|r| r.label().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Function {
    Timeframe(Timeframe),
    ChartType(ChartType),
    /// Add or remove an indicator (any code from the catalog: `RSI`, `BB 20 2.5`, `ICHIMOKU`).
    Indicator(IndicatorSpec),
    /// Open the indicator picker, pre-filled with a search (`IND`, `IND VOLUME`).
    IndicatorPicker(String),
    ClearIndicators,
    Compare(Option<String>),
    LogScale,
    Range(Range),
    Draw(Tool),
    ClearDrawings,
    /// Restore the last deleted or cleared drawings for the current symbol.
    UndoDrawings,
    Magnet,
    /// Congress trades: feed when used alone, per-ticker list after a symbol.
    Congress,
    Politician(String),
    Track(String),
    Untrack(String),
    Alerts,
    /// `AAPL ALERT 150` / `AAPL ALERT VWAP` / `AAPL ALERT RVOL 2` / `AAPL ALERT SETUP NEAR_HIGH`.
    SetPriceAlert(tr_data::price_alerts::AlertKind),
    /// `UNALERT` / `UNALERT AAPL` — drop price, VWAP, RVOL, and setup rules.
    ClearPriceAlert(String),
    Refresh,
    History(String),
    Universe,
    Scan(String),
    Watchlist,
    /// Earnings dates for the selected watchlist group (`EARNINGS`).
    Earnings,
    /// Session RVOL, VWAP, and return versus SPY for the selected watchlist group (`ISCAN`).
    Intraday,
    /// Saved 2×2 desk: 5-minute chart, ISCAN, breadth, and volume pace (`IDESK`).
    IntradayDesk,
    WatchCommand(String),
    /// `LAYOUT 1|2|2H|3|4|6` preset, `LAYOUT <name>` switch, or a layout subcommand
    /// (`SAVE`, `SAVEAS`, `NEW`, `RENAME`, `DUP`, `DEL`, `EXPORT`, `IMPORT`).
    Layout(String),
    /// List saved layouts.
    Layouts,
    /// Add a panel (`NEW NEWS`, `NEW BOOK BTCUSD`); empty adds a chart.
    NewPanel(String),
    /// Split the focused pane, adding a panel to the right (or below).
    Split {
        down: bool,
        panel: String,
    },
    PopOut,
    /// Put the focused pane in a colour link group (`None` = unlink).
    Link(Option<crate::layouts::Link>),
    /// Watchlist / symbol headlines (`NEWS`, `AAPL CN`).
    News,
    /// General market headlines (`TOP`).
    TopNews,
    /// Order book depth (levels).
    OrderBook(Option<usize>),
    /// Option chain monitor (Bloomberg OMON).
    Options,
    /// `SIDEBAR LEFT|RIGHT|TOGGLE|SHOW|HIDE|SWAP`.
    Sidebar(String),
    Settings,
    /// Check for a newer release of the app.
    Update,
    /// `THEME LIGHT|DARK|SYSTEM|COMPACT|REGULAR|COMFORTABLE`.
    Theme(String),
    /// Rename the focused tab (empty resets).
    Title(String),
    ClosePane,
    Help,
    /// World equity indices / markets overview.
    Wei,
    /// US Treasury yield curve.
    Curve,
    /// Security search across asset classes.
    Search(String),
    /// Brokerage portfolio view, with an optional subcommand (`REFRESH`, `CONNECT`, `SETUP`).
    Portfolio(PortView, String),
    /// Open the order ticket (`BUY`, `SELL`, `TRADE`).
    Trade(TradeArgs),
    /// Switch between the demo account and live accounts (`DEMO`, `MODE LIVE`); `true` = live.
    AccountMode(bool),
    /// Open linked daily + weekly archive charts for candidate review.
    Review,
    /// Watchlist-scoped PM Desk: leaders, group performance, position size.
    Desk,
    /// Trade journal / checklist (`JOURNAL`, `AAPL JOURNAL`, `AAPL JOURNAL 2026-10-24`).
    Journal(String),
    /// Historical risk/return and conditional forward returns (`STATS`, `AAPL COND RSI < 30`).
    Stats {
        condition: String,
        conditional: bool,
    },
    /// Paper strategy backtest vs buy-and-hold (`BACKTEST`, `AAPL BT ADTREND`).
    Backtest {
        spec: String,
    },
    /// Paper intraday VWAP and opening-range backtest (`IBT`, `AAPL IBT VWAP`).
    IntradayBacktest {
        spec: String,
    },
    /// Multi-symbol intraday compare, percent from each 09:30 open (`ICMP`, `AAPL ICMP MSFT`).
    IntradayCompare {
        spec: String,
    },
    /// Today versus the typical path of prior sessions (`SPATH`, `AAPL SPATH 20 15m`).
    SessionPath {
        spec: String,
    },
    /// Share of a list at or above the open and session VWAP (`BRD`, `AAPL BRD MSFT`).
    Breadth {
        spec: String,
    },
    /// Cumulative volume versus prior sessions (`VPACE`, `AAPL VPACE 20 15m`).
    VolumePace {
        spec: String,
    },
    /// Open versus the prior cash close, and how often that gap had filled (`GFILL`).
    GapFill {
        spec: String,
    },
    /// When the session high and low print (`XTRM`, `AAPL XTRM 20 15m`).
    SessionExtreme {
        spec: String,
    },
    /// Percent from the 09:30 open minus SPY (`RPATH`, `AAPL RPATH 20 15m`).
    RelativePath {
        spec: String,
    },
    /// Historical lookalikes / nearest neighbors (`LOOKALIKE`, `AAPL KNN 2024-01-15 K=15`).
    Lookalike {
        spec: String,
    },
    /// Jump the focused (or named) archive chart to a session date (`GOTO 2024-01-15`).
    Goto {
        date: String,
    },
    /// Attach a note to a historical chart date (`NOTE`, `AAPL NOTE 2026-10-24 text`).
    JournalNote {
        date: String,
        text: String,
    },
    /// Pan the focused chart to the latest candle without changing zoom.
    Latest,
    /// Toggle a persisted review overlay on the focused chart.
    ReviewOverlay(ReviewOverlay),
    /// `EXPORT SCAN|JOURNAL|HIST|STATS|BACKTEST|LOOKALIKE|WORKSPACE [path]`.
    Export(ExportKind, String),
    /// `BACKUP [path]` — portable workspace backup (no credentials).
    Backup(String),
    /// `RESTORE path` — workspace, journal, or price-history file.
    Restore(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Auto,
    Scan,
    Journal,
    History,
    Workspace,
    Stats,
    Backtest,
    Lookalike,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOverlay {
    BoxLevels,
    Strength,
    Volume,
}

fn num<T: std::str::FromStr>(t: Option<&&str>) -> Option<T> {
    t.and_then(|s| s.parse().ok())
}

fn parse_setup_name(s: &str) -> Option<String> {
    tr_data::price_alerts::normalize_setup(s)
}

/// `2026-10-24 held 3 days` → date + text; otherwise the whole remainder is the note.
fn split_note_args(rest: &[&str]) -> (String, String) {
    match rest.split_first() {
        Some((first, tail)) if tr_data::journal::parse_chart_date(first).is_ok() => (first.to_string(), tail.join(" ")),
        _ => (String::new(), rest.join(" ")),
    }
}

/// `150` · `ABOVE 150` · `BELOW 150` · `VWAP` · `VWAP BELOW` · `RVOL 2` · `NEAR_HIGH` · `SETUP NEAR_HIGH EXIT`.
fn parse_alert_kind(rest: &[&str]) -> Option<tr_data::price_alerts::AlertKind> {
    use tr_data::price_alerts::{AlertKind, PriceSide, SetupChange};
    if rest.is_empty() {
        return None;
    }
    let up: Vec<String> = rest.iter().map(|s| s.to_ascii_uppercase()).collect();
    let first = up[0].as_str();
    let change = |i: usize| match up.get(i).map(String::as_str) {
        Some("EXIT" | "LEAVE" | "OFF") => SetupChange::Exit,
        Some("EITHER" | "CHANGE" | "FLIP") => SetupChange::Either,
        _ => SetupChange::Enter,
    };
    if first == "SETUP" {
        let setup = parse_setup_name(up.get(1)?)?;
        return Some(AlertKind::Setup { setup, change: change(2) });
    }
    if first == "VWAP" {
        let side = match up.get(1).map(String::as_str) {
            Some("BELOW" | "UNDER" | "LT") => PriceSide::Below,
            Some("ABOVE" | "OVER" | "GT") | None => PriceSide::Above,
            Some(_) => return None,
        };
        return Some(AlertKind::Vwap { side });
    }
    if first == "RVOL" {
        let multiple: f64 = rest.get(1)?.parse().ok()?;
        if !multiple.is_finite() || multiple <= 0.0 || up.len() != 2 {
            return None;
        }
        return Some(AlertKind::Rvol { multiple });
    }
    if let Some(signal) = tr_data::session_alerts::SessionSignal::parse(first) {
        if up.len() != 1 {
            return None;
        }
        return Some(AlertKind::Session { signal });
    }
    if matches!(first, "ABOVE" | "OVER" | "GT") {
        let level: f64 = rest.get(1)?.parse().ok()?;
        return Some(AlertKind::Price { side: PriceSide::Above, level });
    }
    if matches!(first, "BELOW" | "UNDER" | "LT") {
        let level: f64 = rest.get(1)?.parse().ok()?;
        return Some(AlertKind::Price { side: PriceSide::Below, level });
    }
    if let Some(setup) = parse_setup_name(first) {
        return Some(AlertKind::Setup { setup, change: change(1) });
    }
    let level: f64 = first.parse().ok()?;
    (level.is_finite() && level > 0.0).then_some(AlertKind::Price { side: PriceSide::Above, level })
}

fn parse_function(head: &str, rest: &[&str]) -> Option<Function> {
    let up = head.to_ascii_uppercase();
    let words = || rest.iter().map(|s| s.to_ascii_uppercase()).collect::<Vec<_>>();
    let joined = || rest.join(" ");
    Some(match up.as_str() {
        "CANDLE" | "CANDLES" => Function::ChartType(ChartType::Candles),
        "BAR" | "BARS" | "OHLC" => Function::ChartType(ChartType::Bars),
        "LINE" => Function::ChartType(ChartType::Line),
        "AREA" | "MOUNTAIN" => Function::ChartType(ChartType::Area),
        // `IND RSI 21` adds directly; anything else searches the picker.
        "IND" | "INDICATOR" | "INDICATORS" | "STUDY" | "STUDIES" => {
            match rest.split_first().and_then(|(code, args)| IndicatorSpec::parse(code, args)) {
                Some(spec) => Function::Indicator(spec),
                None => Function::IndicatorPicker(joined()),
            }
        }
        "CLEAR" | "CLR" => match words().first().map(String::as_str) {
            Some("DRAW") | Some("DRAWINGS") => Function::ClearDrawings,
            _ => Function::ClearIndicators,
        },
        "UNDO" => match words().first().map(String::as_str) {
            Some("DRAW") | Some("DRAWINGS") | None => Function::UndoDrawings,
            _ => return None,
        },
        "UNDODRAW" | "UNDODRAWS" => Function::UndoDrawings,
        "CMP" | "COMPARE" | "COMP" => Function::Compare(rest.first().map(|s| s.to_ascii_uppercase())),
        "LOG" => Function::LogScale,
        "TL" | "TREND" | "TRENDLINE" => Function::Draw(Tool::Trendline),
        "HL" | "HLINE" => Function::Draw(Tool::Horizontal),
        "VL" | "VLINE" => Function::Draw(Tool::Vertical),
        "FIB" => Function::Draw(Tool::Fib),
        "MAGNET" | "MAG" => Function::Magnet,
        "HIST" | "HISTORY" => Function::History(words().join(" ")),
        "UNIV" if rest.is_empty() => Function::Universe,
        "SCAN" => Function::Scan(words().join(" ")),
        "CONG" | "CONGRESS" => Function::Congress,
        "POL" | "PROFILE" => Function::Politician(joined()),
        "TRACK" => Function::Track(joined()),
        "UNTRACK" => Function::Untrack(joined()),
        "ALRT" | "ALERTS" => Function::Alerts,
        "ALERT" if rest.is_empty() => Function::Alerts,
        "ALERT" => Function::SetPriceAlert(parse_alert_kind(rest)?),
        "UNALERT" | "RMALERT" | "DELALERT" => Function::ClearPriceAlert(joined()),
        "REFRESH" | "SYNC" => Function::Refresh,
        "EARN" | "EARNINGS" | "ECAL" => Function::Earnings,
        "ISCAN" | "INTRADAY" => Function::Intraday,
        "IDESK" | "IDASH" => Function::IntradayDesk,
        "W" | "WL" | "WATCH" | "WATCHLIST" | "MON" => {
            if rest.is_empty() {
                Function::Watchlist
            } else {
                Function::WatchCommand(joined())
            }
        }
        "LAYOUT" | "GRID" | "LP" => Function::Layout(joined()),
        "LAYOUTS" => Function::Layouts,
        "NEW" | "ADD" => Function::NewPanel(joined()),
        "SPLIT" | "SPLITV" | "SPLITH" => {
            let w = words();
            let down = up == "SPLITH" || matches!(w.first().map(String::as_str), Some("DOWN" | "H" | "BELOW"));
            let skip =
                usize::from(matches!(w.first().map(String::as_str), Some("DOWN" | "H" | "BELOW" | "RIGHT" | "V")));
            Function::Split { down, panel: rest[skip.min(rest.len())..].join(" ") }
        }
        "POP" | "POPOUT" | "UNDOCK" => Function::PopOut,
        "LINK" => Function::Link(match words().first().map(String::as_str) {
            None | Some("OFF" | "NONE" | "0") => None,
            Some(c) => Some(crate::layouts::Link::ALL.into_iter().find(|l| l.label().eq_ignore_ascii_case(c))?),
        }),
        "N" | "CN" | "NEWS" => Function::News,
        "TOP" => Function::TopNews,
        "OB" | "BOOK" | "DEPTH" | "DOM" => Function::OrderBook(num(rest.first())),
        "OMON" | "OMST" | "OPT" | "OPTIONS" | "CHAIN" => Function::Options,
        "SIDEBAR" | "SB" => Function::Sidebar(words().join(" ")),
        "PREFS" | "SETTINGS" | "PDF" | "CONFIG" => Function::Settings,
        "UPDATE" | "UPDATES" => Function::Update,
        "THEME" => Function::Theme(words().join(" ")),
        "TITLE" | "RENAME" => Function::Title(joined()),
        "CLOSE" => Function::ClosePane,
        "HELP" | "?" | "H" => Function::Help,
        "WEI" | "MARKETS" | "WORLD" => Function::Wei,
        "GC" | "YCRV" | "CURVE" | "YC" => Function::Curve,
        "SECF" | "SEARCH" | "FIND" => Function::Search(joined()),
        "PORT" | "PORTFOLIO" | "PRTU" => Function::Portfolio(PortView::Summary, words().join(" ")),
        "ACCT" | "ACCOUNTS" | "ACCTS" | "BAL" | "BALANCES" => {
            Function::Portfolio(PortView::Accounts, words().join(" "))
        }
        "POS" | "POSITIONS" | "HOLDINGS" => Function::Portfolio(PortView::Positions, words().join(" ")),
        "ORD" | "ORDERS" | "BLTR" => Function::Portfolio(PortView::Orders, words().join(" ")),
        "ACTV" | "ACTIVITY" | "TXNS" | "TRANSACTIONS" => Function::Portfolio(PortView::Activity, words().join(" ")),
        "BROKER" | "BROKERAGE" | "SNAPTRADE" => Function::Portfolio(PortView::Summary, "SETUP".into()),
        "BUY" => Function::Trade(TradeArgs::parse(Some(true), rest)?),
        "SELL" => Function::Trade(TradeArgs::parse(Some(false), rest)?),
        "TRADE" | "TICKET" | "EMSX" => Function::Trade(TradeArgs::parse(None, rest)?),
        "DEMO" | "PAPER" if rest.is_empty() => Function::AccountMode(false),
        "MODE" => match words().join(" ").as_str() {
            "DEMO" | "PAPER" => Function::AccountMode(false),
            "LIVE" | "REAL" => Function::AccountMode(true),
            _ => return None,
        },
        "REVIEW" | "REV" => Function::Review,
        "DESK" | "PMDESK" | "RISK" => Function::Desk,
        "JOURNAL" | "JRNL" | "CHECKLIST" | "TRADELOG" => Function::Journal(joined()),
        "STATS" | "STAT" | "STATISTICS" => Function::Stats { condition: joined(), conditional: !rest.is_empty() },
        "COND" | "CDIST" | "CONDRET" => Function::Stats { condition: joined(), conditional: true },
        "BACKTEST" | "BT" | "BTEST" => Function::Backtest { spec: joined() },
        "IBT" | "IBACKTEST" | "SETUPBT" => Function::IntradayBacktest { spec: joined() },
        "LOOKALIKE" | "LOOKALIKES" | "KNN" | "ANALOG" | "ANALOGUE" | "NN" => Function::Lookalike { spec: joined() },
        "ICMP" | "ICOMPARE" | "ICOMP" => Function::IntradayCompare { spec: joined() },
        "SPATH" | "SESSIONPATH" | "TPATH" => Function::SessionPath { spec: joined() },
        "BRD" | "BREADTH" | "SBREADTH" => Function::Breadth { spec: joined() },
        "VPACE" | "VOLPACE" | "CUMVOL" => Function::VolumePace { spec: joined() },
        "GFILL" | "GAPFILL" => Function::GapFill { spec: joined() },
        "XTRM" | "EXTREME" => Function::SessionExtreme { spec: joined() },
        "RPATH" | "RELPATH" => Function::RelativePath { spec: joined() },
        "GOTO" | "JUMP" | "GTO" => Function::Goto { date: joined() },
        "NOTE" | "JNOTE" => {
            let (date, text) = split_note_args(rest);
            Function::JournalNote { date, text }
        }
        "LATEST" | "END" => Function::Latest,
        "BOX" => Function::ReviewOverlay(ReviewOverlay::BoxLevels),
        "RS" | "STRENGTH" => Function::ReviewOverlay(ReviewOverlay::Strength),
        "VCTX" | "VOLCTX" => Function::ReviewOverlay(ReviewOverlay::Volume),
        "EXPORT" => {
            let w = words();
            let (kind, path) = match w.first().map(String::as_str) {
                Some("SCAN" | "UNIV") => (ExportKind::Scan, rest.get(1..).unwrap_or(&[]).join(" ")),
                Some("JOURNAL" | "JRNL" | "CHECKLIST") => (ExportKind::Journal, rest.get(1..).unwrap_or(&[]).join(" ")),
                Some("HIST" | "HISTORY") => (ExportKind::History, rest.get(1..).unwrap_or(&[]).join(" ")),
                Some("STATS" | "STAT" | "COND" | "CDIST") => {
                    (ExportKind::Stats, rest.get(1..).unwrap_or(&[]).join(" "))
                }
                Some("BACKTEST" | "BT" | "BTEST") => (ExportKind::Backtest, rest.get(1..).unwrap_or(&[]).join(" ")),
                Some("LOOKALIKE" | "LOOKALIKES" | "KNN" | "ANALOG" | "ANALOGUE") => {
                    (ExportKind::Lookalike, rest.get(1..).unwrap_or(&[]).join(" "))
                }
                Some("WORKSPACE" | "BACKUP" | "WS") => (ExportKind::Workspace, rest.get(1..).unwrap_or(&[]).join(" ")),
                _ => (ExportKind::Auto, joined()),
            };
            Function::Export(kind, path)
        }
        "BACKUP" => Function::Backup(joined()),
        "RESTORE" => Function::Restore(joined()),
        "RANGE" => Function::Range(Range::parse(rest.first()?)?),
        // Timeframes win where spellings overlap (`1D`, `1M`); otherwise try a range (`5D`, `YTD`),
        // then the indicator catalog (`RSI`, `BB 20 2.5`).
        _ => {
            return head
                .parse::<Timeframe>()
                .ok()
                .map(Function::Timeframe)
                .or_else(|| Range::parse(&up).map(Function::Range))
                .or_else(|| IndicatorSpec::parse(head, rest).map(Function::Indicator));
        }
    })
}

/// Parse one command line. Returns `None` for empty or unrecognised input.
pub fn parse(input: &str) -> Option<Command> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let tokens: Vec<&str> = input.split_whitespace().filter(|t| !t.eq_ignore_ascii_case("<GO>")).collect();
    let (&head, rest) = tokens.split_first()?;
    // `<TICKER> <YELLOW KEY>` is always a security, even if the ticker spells a function.
    let keyed = match rest {
        [k, ..] if YellowKey::parse(k).is_some() => true,
        [us, k, ..] if us.eq_ignore_ascii_case("US") && YellowKey::parse(k).is_some() => true,
        _ => false,
    };
    if !keyed && let Some(f) = parse_function(head, rest) {
        return Some(Command::Function(f));
    }
    let (sec, used) = Security::resolve_tokens(&tokens)?;
    match tokens[used..].split_first() {
        None => Some(Command::Symbol(sec.key())),
        // `TSLA SELL ALL`: the security is already named, so `ALL` is the whole position.
        Some((&f, args))
            if ["SELL", "TRADE"].iter().any(|h| f.eq_ignore_ascii_case(h))
                && args.iter().any(|a| a.eq_ignore_ascii_case("ALL")) =>
        {
            let rest: Vec<&str> = args.iter().copied().filter(|a| !a.eq_ignore_ascii_case("ALL")).collect();
            match parse_function(f, &rest)? {
                Function::Trade(mut t) if t.symbol.is_none() && t.units.is_none() && t.notional.is_none() => {
                    t.all = true;
                    t.buy = Some(false);
                    Some(Command::SymbolFunction(sec.key(), Function::Trade(t)))
                }
                _ => None,
            }
        }
        Some((&f, args)) => parse_function(f, args).map(|func| Command::SymbolFunction(sec.key(), func)),
    }
}

/// One-line live interpretation of partially typed input, shown under the command line.
pub fn hint(input: &str) -> String {
    match parse(input) {
        None if input.trim().is_empty() => "Type a ticker and press <GO> (Enter).  HELP for functions.".into(),
        None => "Unrecognised command".into(),
        Some(Command::Symbol(s)) => match Security::parse(&s).and_then(|x| x.name()) {
            Some(name) => format!("Load {s} — {name}"),
            None => format!("Load {s}"),
        },
        Some(Command::SymbolFunction(s, f)) => format!("{s}: {}", describe(&f)),
        Some(Command::Function(f)) => describe(&f),
    }
}

fn describe(f: &Function) -> String {
    match f {
        Function::Timeframe(tf) => format!(
            "Interval {}",
            match tf {
                Timeframe::M1 => "1 minute",
                Timeframe::M5 => "5 minutes",
                Timeframe::M15 => "15 minutes",
                Timeframe::M30 => "30 minutes",
                Timeframe::H1 => "1 hour",
                Timeframe::H4 => "4 hours",
                Timeframe::D1 => "1 day",
                Timeframe::W1 => "1 week",
                Timeframe::MN1 => "1 month",
            }
        ),
        Function::ChartType(c) => format!("Chart type {}", c.label()),
        Function::Indicator(i) => format!("Toggle {} — {}", i.label(), i.name()),
        Function::IndicatorPicker(q) if q.is_empty() => "Browse indicators".into(),
        Function::IndicatorPicker(q) => format!("Search indicators for \"{q}\""),
        Function::ClearIndicators => "Remove all indicators".into(),
        Function::Compare(Some(s)) => format!("Compare with {s}"),
        Function::Compare(None) => "Remove comparison".into(),
        Function::LogScale => "Toggle log scale".into(),
        Function::Range(r) => format!("Show {} at {}", r.label(), r.timeframe()),
        Function::Draw(t) => format!("Draw {}", t.label()),
        Function::ClearDrawings => "Remove all drawings".into(),
        Function::UndoDrawings => "Undo the last drawing delete or clear on this symbol".into(),
        Function::Magnet => "Toggle magnet (snap to OHLC)".into(),
        Function::Congress => "Congressional trades".into(),
        Function::Politician(n) => format!("Politician profile: {n}"),
        Function::Track(n) => format!("Track {n} (alert on new filings)"),
        Function::Untrack(n) => format!("Stop tracking {n}"),
        Function::Alerts => {
            "Alerts: price levels, session VWAP crosses, session RVOL, session path/pace/gap/extreme/relative, setup changes, and tracked filings".into()
        }
        Function::SetPriceAlert(kind) => format!("Watch {}", kind.label()),
        Function::ClearPriceAlert(q) if q.is_empty() => "Remove price/setup alerts for the focused name".into(),
        Function::ClearPriceAlert(q) => format!("Remove price/setup alerts for {q}"),
        Function::Refresh => "Refresh congressional disclosures now".into(),
        Function::History(args) => format!("Price history {args}"),
        Function::Universe => "Browse Whats-News archive coverage".into(),
        Function::Scan(name) => format!("Scan Whats-News archive: {name}"),
        Function::Watchlist => "Watchlist".into(),
        Function::Earnings => "Earnings calendar for the selected watchlist".into(),
        Function::Intraday => {
            "Intraday scan of the selected watchlist: session RVOL, close versus VWAP, and return versus SPY".into()
        }
        Function::IntradayDesk => {
            "Intraday desk: 5-minute chart, ISCAN, breadth, and volume pace, saved as the Intraday layout".into()
        }
        Function::WatchCommand(s) => format!("Watchlists: {s}"),
        Function::Layout(l) if l.trim().is_empty() => "Saved layouts".into(),
        Function::Layout(l) => format!("Layout {l}"),
        Function::Layouts => "Saved layouts".into(),
        Function::NewPanel(p) if p.trim().is_empty() => "Add a chart pane".into(),
        Function::NewPanel(p) => format!("Add panel: {p}"),
        Function::Split { down, panel } => {
            format!(
                "Split {} with {}",
                if *down { "down" } else { "right" },
                if panel.is_empty() { "a chart" } else { panel }
            )
        }
        Function::PopOut => "Pop the focused pane out into its own window".into(),
        Function::Link(Some(l)) => format!("Link focused pane to the {} group", l.label()),
        Function::Link(None) => "Unlink focused pane".into(),
        Function::News => "Watchlist news (AAPL CN for one name)".into(),
        Function::TopNews => "Top market headlines".into(),
        Function::OrderBook(_) => "Order book (depth)".into(),
        Function::Options => "Option chain: calls and puts by strike, IV and greeks".into(),
        Function::Sidebar(s) if s.is_empty() => "Toggle the sidebar".into(),
        Function::Sidebar(s) => format!("Sidebar {}", s.to_lowercase()),
        Function::Settings => "Settings: theme, fonts, density, hotkeys, panels, layouts, backup".into(),
        Function::Update => "Check for updates".into(),
        Function::Theme(t) => format!("Theme {t}"),
        Function::Title(t) => format!("Rename tab to {t:?}"),
        Function::ClosePane => "Close focused pane".into(),
        Function::Help => "Help".into(),
        Function::Wei => "World markets overview".into(),
        Function::Curve => "US Treasury yield curve".into(),
        Function::Search(q) => format!("Search securities: {q}"),
        Function::Portfolio(v, a) if a.is_empty() => match v {
            PortView::Summary => "Brokerage portfolio: value, cash, P&L and holdings (SnapTrade)".into(),
            PortView::Accounts => "Brokerage accounts and balances".into(),
            PortView::Positions => "Brokerage positions with unrealised P&L".into(),
            PortView::Orders => "Brokerage orders (last 30 days)".into(),
            PortView::Activity => "Brokerage activity: trades, dividends, transfers".into(),
        },
        Function::Portfolio(_, a) => match a.as_str() {
            "REFRESH" | "SYNC" => "Reload brokerage accounts now".into(),
            "CONNECT" | "LINK" => "Connect a brokerage in your browser (SnapTrade, read-only)".into(),
            _ => "Brokerage connection settings".into(),
        },
        Function::Trade(t) => {
            let side = match t.buy {
                Some(true) => "Buy",
                Some(false) => "Sell",
                None => "Trade",
            };
            let mut s = format!("Order ticket: {side}");
            if let Some(u) = t.units {
                s += &format!(" {}", tr_broker::trading::fmt_units(u));
            }
            if let Some(n) = t.notional {
                s += &format!(" ${n:.2} of");
            }
            if let Some(k) = &t.symbol {
                s += &format!(" {}", Security::parse(k).map_or(k.clone(), |x| x.ticker));
            }
            if let Some(p) = t.stop {
                s += &format!(" stop {p}");
            }
            if let Some(p) = t.limit {
                s += &format!(" limit {p}");
            }
            if t.all {
                s += " (whole position)";
            }
            s
        }
        Function::AccountMode(false) => "Switch to the DEMO account (paper trading)".into(),
        Function::AccountMode(true) => "Switch to LIVE brokerage accounts (asks first)".into(),
        Function::Review => "Linked daily and weekly archive charts for candidate review".into(),
        Function::Desk => {
            "PM Desk: watchlist leaders, group performance, and position size from entry/stop/target/$ risk".into()
        }
        Function::Journal(q) if q.is_empty() => {
            "Trade journal: setups, checklist, planned risk, outcomes in $ and R".into()
        }
        Function::Journal(q) => format!("Trade journal: {q}"),
        Function::Stats { condition, conditional } if condition.is_empty() => {
            if *conditional {
                "Conditional returns: what followed a filter vs the unconditional baseline".into()
            } else {
                "Statistics: historical risk, returns, RSI/KAMA bins and seasonality".into()
            }
        }
        Function::Stats { condition, .. } => format!("Conditional returns after {condition}"),
        Function::Backtest { spec } if spec.is_empty() => {
            "Paper backtest: declared strategies vs buy-and-hold on a chronological holdout".into()
        }
        Function::Backtest { spec } => format!("Paper backtest vs buy-and-hold ({spec})"),
        Function::IntradayBacktest { spec } if spec.is_empty() => {
            "Paper intraday backtest: session VWAP and opening range vs holding the cash session".into()
        }
        Function::IntradayBacktest { spec } => format!("Paper intraday backtest ({spec})"),
        Function::IntradayCompare { spec } if spec.is_empty() => {
            "Intraday compare: each name as percent from its own 09:30 open, with SPY on the same clock".into()
        }
        Function::IntradayCompare { spec } => format!("Intraday compare from the 09:30 open ({spec})"),
        Function::SessionPath { spec } if spec.is_empty() => {
            "Session path: today versus the mean of prior sessions, each percent from its own 09:30 open".into()
        }
        Function::SessionPath { spec } => format!("Session path versus prior 09:30 opens ({spec})"),
        Function::Breadth { spec } if spec.is_empty() => {
            "Session breadth: share of the watchlist at or above the 09:30 open and session VWAP".into()
        }
        Function::Breadth { spec } => format!("Session breadth at or above the open and VWAP ({spec})"),
        Function::VolumePace { spec } if spec.is_empty() => {
            "Volume pace: today's cumulative shares versus the mean of prior sessions".into()
        }
        Function::VolumePace { spec } => format!("Volume pace versus prior sessions ({spec})"),
        Function::GapFill { spec } if spec.is_empty() => {
            "Gap fill: today's 09:30 open versus the prior cash close, and how often same-direction gaps had filled"
                .into()
        }
        Function::GapFill { spec } => format!("Gap fill versus the prior cash close ({spec})"),
        Function::SessionExtreme { spec } if spec.is_empty() => {
            "Session extreme: when the high and the low print, against completed prior sessions".into()
        }
        Function::SessionExtreme { spec } => format!("Session extreme versus completed sessions ({spec})"),
        Function::RelativePath { spec } if spec.is_empty() => {
            "Relative path: percent from the 09:30 open minus SPY, against prior sessions".into()
        }
        Function::RelativePath { spec } => format!("Relative path versus SPY ({spec})"),
        Function::Lookalike { spec } if spec.is_empty() => {
            "Historical lookalikes: nearest past conditions, per-horizon counts, jump to neighbor dates".into()
        }
        Function::Lookalike { spec } => format!("Historical lookalikes ({spec})"),
        Function::Goto { date } if date.is_empty() => "Jump the focused archive chart to a YYYY-MM-DD session".into(),
        Function::Goto { date } => format!("Jump the archive chart to {date}"),
        Function::JournalNote { date, text } if date.is_empty() && text.is_empty() => {
            "Attach a note to the focused chart date".into()
        }
        Function::JournalNote { date, text } if date.is_empty() => format!("Note on the focused chart date: {text}"),
        Function::JournalNote { date, text } if text.is_empty() => format!("Open journal notes for {date}"),
        Function::JournalNote { date, text } => format!("Note on {date}: {text}"),
        Function::Latest => "Jump the focused chart to the latest candle".into(),
        Function::ReviewOverlay(ReviewOverlay::BoxLevels) => {
            "Toggle box levels (20-bar / 3-bar confirmation on this interval)".into()
        }
        Function::ReviewOverlay(ReviewOverlay::Strength) => {
            "Toggle 21-bar close return (this series, not SPY-relative)".into()
        }
        Function::ReviewOverlay(ReviewOverlay::Volume) => {
            "Toggle volume context (this bar / prior 20, plus 5/20)".into()
        }
        Function::Export(ExportKind::Auto, p) if p.is_empty() => {
            "Export the focused scan, history or journal (or open Backup settings)".into()
        }
        Function::Export(ExportKind::Auto, p) => format!("Export the focused pane to {p}"),
        Function::Export(ExportKind::Scan, p) if p.is_empty() => {
            "Export scan / universe results with source and date".into()
        }
        Function::Export(ExportKind::Scan, p) => format!("Export scan results to {p}"),
        Function::Export(ExportKind::Journal, p) if p.is_empty() => "Export trade journal entries".into(),
        Function::Export(ExportKind::Journal, p) => format!("Export journal to {p}"),
        Function::Export(ExportKind::History, p) if p.is_empty() => {
            "Export archived daily price history with source and dates".into()
        }
        Function::Export(ExportKind::History, p) => format!("Export price history to {p}"),
        Function::Export(ExportKind::Stats, p) if p.is_empty() => {
            "Export statistics and conditional returns with source, as-of and provenance".into()
        }
        Function::Export(ExportKind::Stats, p) => format!("Export statistics to {p}"),
        Function::Export(ExportKind::Backtest, p) if p.is_empty() => {
            "Export the paper backtest with costs, holdout, execution and provenance".into()
        }
        Function::Export(ExportKind::Backtest, p) => format!("Export backtest to {p}"),
        Function::Export(ExportKind::Lookalike, p) if p.is_empty() => {
            "Export lookalikes with source, as-of, per-horizon counts and overlap".into()
        }
        Function::Export(ExportKind::Lookalike, p) => format!("Export lookalikes to {p}"),
        Function::Export(ExportKind::Workspace, p) if p.is_empty() => {
            "Back up personal workspace data (credentials excluded)".into()
        }
        Function::Export(ExportKind::Workspace, p) => format!("Back up workspace to {p}"),
        Function::Backup(p) if p.is_empty() => "Back up personal workspace data (credentials excluded)".into(),
        Function::Backup(p) => format!("Back up workspace to {p}"),
        Function::Restore(p) if p.is_empty() => "Restore a workspace, journal or history export".into(),
        Function::Restore(p) => format!("Restore {p}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols() {
        let sym = |s: &str| Some(Command::Symbol(s.into()));
        assert_eq!(parse("aapl"), sym("AAPL US Equity"));
        assert_eq!(parse("AAPL US EQUITY <GO>"), sym("AAPL US Equity"));
        assert_eq!(parse("brk/b"), sym("BRK.B US Equity"));
        assert_eq!(parse("BTCUSD <CRNCY>"), sym("BTCUSD Curncy"));
        assert_eq!(parse("EURUSD <CRNCY>"), sym("EURUSD Curncy"));
        assert_eq!(parse("US10Y <GOVT>"), sym("US10Y Govt"));
        assert_eq!(parse("CL1 <CMDTY>"), sym("CL1 Comdty"));
        assert_eq!(parse("SPX <INDEX>"), sym("SPX Index"));
        assert_eq!(parse("AAPL <EQUITY>"), sym("AAPL US Equity"));
        assert_eq!(parse("btcusd"), sym("BTCUSD Curncy"));
        // A yellow key makes a function-like word a ticker.
        assert_eq!(parse("LINE <EQUITY>"), sym("LINE US Equity"));
        assert_eq!(parse("LINE"), Some(Command::Function(Function::ChartType(ChartType::Line))));
        assert_eq!(parse("   "), None);
        assert_eq!(parse("123abc"), None);
    }

    #[test]
    fn timeframes_and_ranges() {
        assert_eq!(parse("5"), Some(Command::Function(Function::Timeframe(Timeframe::M5))));
        assert_eq!(parse("1W"), Some(Command::Function(Function::Timeframe(Timeframe::W1))));
        assert_eq!(parse("1M"), Some(Command::Function(Function::Timeframe(Timeframe::MN1))));
        assert_eq!(parse("1m"), Some(Command::Function(Function::Timeframe(Timeframe::M1))));
        assert_eq!(parse("YTD"), Some(Command::Function(Function::Range(Range::Ytd))));
        assert_eq!(parse("RANGE 5D"), Some(Command::Function(Function::Range(Range::D5))));
        assert_eq!(Range::Y5.timeframe(), Timeframe::W1);
    }

    #[test]
    fn research_commands() {
        assert_eq!(parse("hist"), Some(Command::Function(Function::History(String::new()))));
        assert_eq!(parse("HIST ADD aapl,msft"), Some(Command::Function(Function::History("ADD AAPL,MSFT".into()))));
        for action in ["REFRESH", "CANCEL", "RESUME", "RETRY"] {
            assert_eq!(parse(&format!("HIST {action}")), Some(Command::Function(Function::History(action.into()))));
        }
        assert_eq!(parse("UNIV <GO>"), Some(Command::Function(Function::Universe)));
        assert_eq!(parse("scan darvas_breakout"), Some(Command::Function(Function::Scan("DARVAS_BREAKOUT".into()))));
        assert_eq!(parse("SCAN"), Some(Command::Function(Function::Scan(String::new()))));
        assert_eq!(parse("REVIEW"), Some(Command::Function(Function::Review)));
        assert_eq!(parse("DESK"), Some(Command::Function(Function::Desk)));
        assert_eq!(parse("PMDESK"), Some(Command::Function(Function::Desk)));
        assert_eq!(parse("AAPL DESK"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Desk)));
        assert_eq!(parse("JOURNAL"), Some(Command::Function(Function::Journal(String::new()))));
        assert_eq!(parse("JRNL"), Some(Command::Function(Function::Journal(String::new()))));
        assert_eq!(parse("CHECKLIST"), Some(Command::Function(Function::Journal(String::new()))));
        assert_eq!(
            parse("AAPL JOURNAL"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Journal(String::new())))
        );
        assert_eq!(
            parse("AAPL JOURNAL 2026-10-24"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Journal("2026-10-24".into())))
        );
        assert_eq!(
            parse("NOTE 2026-10-24 gap and go"),
            Some(Command::Function(Function::JournalNote { date: "2026-10-24".into(), text: "gap and go".into() }))
        );
        assert_eq!(
            parse("AAPL NOTE 2026-03-15 wait for 20-day"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::JournalNote { date: "2026-03-15".into(), text: "wait for 20-day".into() }
            ))
        );
        assert_eq!(parse("LATEST"), Some(Command::Function(Function::Latest)));
        assert_eq!(parse("END"), Some(Command::Function(Function::Latest)));
        assert_eq!(parse("AAPL REVIEW"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Review)));
        assert_eq!(parse("BOX"), Some(Command::Function(Function::ReviewOverlay(ReviewOverlay::BoxLevels))));
        assert_eq!(parse("RS"), Some(Command::Function(Function::ReviewOverlay(ReviewOverlay::Strength))));
        assert_eq!(parse("VCTX"), Some(Command::Function(Function::ReviewOverlay(ReviewOverlay::Volume))));
        assert_eq!(parse("EXPORT"), Some(Command::Function(Function::Export(ExportKind::Auto, String::new()))));
        assert_eq!(
            parse("EXPORT SCAN ~/Downloads/scan.json"),
            Some(Command::Function(Function::Export(ExportKind::Scan, "~/Downloads/scan.json".into())))
        );
        assert_eq!(
            parse("EXPORT JOURNAL"),
            Some(Command::Function(Function::Export(ExportKind::Journal, String::new())))
        );
        assert_eq!(parse("EXPORT HIST"), Some(Command::Function(Function::Export(ExportKind::History, String::new()))));
        assert_eq!(
            parse("STATS"),
            Some(Command::Function(Function::Stats { condition: String::new(), conditional: false }))
        );
        assert_eq!(
            parse("AAPL STATS"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Stats { condition: String::new(), conditional: false }
            ))
        );
        assert_eq!(
            parse("COND RSI(14) < 30"),
            Some(Command::Function(Function::Stats { condition: "RSI(14) < 30".into(), conditional: true }))
        );
        assert_eq!(parse("EXPORT STATS"), Some(Command::Function(Function::Export(ExportKind::Stats, String::new()))));
        assert_eq!(parse("BACKTEST"), Some(Command::Function(Function::Backtest { spec: String::new() })));
        assert_eq!(
            parse("AAPL BT ADTREND"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Backtest { spec: "ADTREND".into() }))
        );
        assert_eq!(
            parse("EXPORT BACKTEST"),
            Some(Command::Function(Function::Export(ExportKind::Backtest, String::new())))
        );
        assert_eq!(parse("LOOKALIKE"), Some(Command::Function(Function::Lookalike { spec: String::new() })));
        assert_eq!(
            parse("AAPL KNN 2024-01-15 K=15"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Lookalike { spec: "2024-01-15 K=15".into() }
            ))
        );
        assert_eq!(parse("GOTO 2024-01-15"), Some(Command::Function(Function::Goto { date: "2024-01-15".into() })));
        assert_eq!(
            parse("EXPORT LOOKALIKE"),
            Some(Command::Function(Function::Export(ExportKind::Lookalike, String::new())))
        );
        assert_eq!(parse("BACKUP"), Some(Command::Function(Function::Backup(String::new()))));
        assert_eq!(
            parse("RESTORE ~/Downloads/workspace.json"),
            Some(Command::Function(Function::Restore("~/Downloads/workspace.json".into())))
        );
    }

    #[test]
    fn functions() {
        let ind = |code: &str, params: &[f64]| {
            Some(Command::Function(Function::Indicator(IndicatorSpec::new(code, params).unwrap())))
        };
        assert_eq!(parse("rsi"), ind("RSI", &[14.0]));
        assert_eq!(parse("SMA 50"), ind("SMA", &[50.0]));
        assert_eq!(parse("bb 20 2.5"), ind("BB", &[20.0, 2.5]));
        assert_eq!(parse("MACD"), ind("MACD", &[12.0, 26.0, 9.0]));
        assert_eq!(parse("ichimoku"), ind("ICHIMOKU", &[]));
        assert_eq!(parse("ADTREND"), ind("ADTREND", &[]));
        assert_eq!(parse("ATREND"), ind("ADTREND", &[]));
        assert_eq!(parse("ADAPTIVE"), ind("ADTREND", &[]));
        assert_eq!(parse("IND STOCH 5 3 3"), ind("STOCH", &[5.0, 3.0, 3.0]));
        assert_eq!(parse("IND"), Some(Command::Function(Function::IndicatorPicker(String::new()))));
        assert_eq!(parse("study volume"), Some(Command::Function(Function::IndicatorPicker("volume".into()))));
        let Some(Command::SymbolFunction(_, Function::Indicator(adx))) = parse("AAPL ADX") else { panic!("AAPL ADX") };
        assert_eq!(adx.label(), "ADX 14 14");
        assert_eq!(parse("CMP msft"), Some(Command::Function(Function::Compare(Some("MSFT".into())))));
        assert_eq!(parse("CMP"), Some(Command::Function(Function::Compare(None))));
        assert_eq!(parse("FIB"), Some(Command::Function(Function::Draw(Tool::Fib))));
        assert_eq!(parse("CLEAR DRAW"), Some(Command::Function(Function::ClearDrawings)));
        assert_eq!(parse("UNDO DRAW"), Some(Command::Function(Function::UndoDrawings)));
        assert_eq!(parse("UNDO"), Some(Command::Function(Function::UndoDrawings)));
        assert_eq!(parse("POL Nancy Pelosi"), Some(Command::Function(Function::Politician("Nancy Pelosi".into()))));
        assert_eq!(parse("WL ADD msft nvda"), Some(Command::Function(Function::WatchCommand("ADD msft nvda".into()))));
        assert_eq!(parse("LAYOUT 4"), Some(Command::Function(Function::Layout("4".into()))));
        assert_eq!(
            parse("LAYOUT save Morning Desk"),
            Some(Command::Function(Function::Layout("save Morning Desk".into())))
        );
        assert_eq!(parse("NEW BOOK"), Some(Command::Function(Function::NewPanel("BOOK".into()))));
        assert_eq!(
            parse("SPLIT DOWN NEWS"),
            Some(Command::Function(Function::Split { down: true, panel: "NEWS".into() }))
        );
        assert_eq!(parse("SPLIT"), Some(Command::Function(Function::Split { down: false, panel: String::new() })));
        assert_eq!(parse("LINK BLUE"), Some(Command::Function(Function::Link(Some(crate::layouts::Link::Blue)))));
        assert_eq!(parse("LINK OFF"), Some(Command::Function(Function::Link(None))));
        assert_eq!(parse("LINK PINK"), None);
        assert_eq!(parse("AAPL CN"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::News)));
        assert_eq!(parse("NEWS"), Some(Command::Function(Function::News)));
        assert_eq!(parse("ALERTS"), Some(Command::Function(Function::Alerts)));
        assert_eq!(parse("EARNINGS"), Some(Command::Function(Function::Earnings)));
        assert_eq!(parse("ECAL"), Some(Command::Function(Function::Earnings)));
        assert_eq!(parse("EARN"), Some(Command::Function(Function::Earnings)));
        assert_eq!(parse("ISCAN"), Some(Command::Function(Function::Intraday)));
        assert_eq!(parse("INTRADAY"), Some(Command::Function(Function::Intraday)));
        assert!(hint("ISCAN").contains("session RVOL"));
        assert_eq!(parse("IDESK"), Some(Command::Function(Function::IntradayDesk)));
        assert_eq!(parse("IDASH"), Some(Command::Function(Function::IntradayDesk)));
        assert_eq!(parse("AAPL IDESK"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::IntradayDesk)));
        assert!(hint("IDESK").contains("Intraday desk"));
        assert_eq!(parse("DESK"), Some(Command::Function(Function::Desk)));
        assert_eq!(parse("ALERT"), Some(Command::Function(Function::Alerts)));
        assert_eq!(
            parse("AAPL ALERT 150"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Price {
                    side: tr_data::price_alerts::PriceSide::Above,
                    level: 150.0,
                })
            ))
        );
        assert_eq!(
            parse("AAPL ALERT BELOW 140"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Price {
                    side: tr_data::price_alerts::PriceSide::Below,
                    level: 140.0,
                })
            ))
        );
        assert_eq!(
            parse("AAPL ALERT VWAP"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Vwap {
                    side: tr_data::price_alerts::PriceSide::Above,
                })
            ))
        );
        assert_eq!(
            parse("AAPL US Equity ALERT VWAP BELOW"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Vwap {
                    side: tr_data::price_alerts::PriceSide::Below,
                })
            ))
        );
        assert_eq!(
            parse("AAPL ALERT VWAP ABOVE"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Vwap {
                    side: tr_data::price_alerts::PriceSide::Above,
                })
            ))
        );
        assert_eq!(parse("AAPL ALERT VWAP NOPE"), None);
        assert!(hint("AAPL ALERT VWAP").contains("above session VWAP"));
        assert_eq!(
            parse("AAPL ALERT RVOL 2"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Rvol { multiple: 2.0 })
            ))
        );
        assert_eq!(
            parse("AAPL US Equity ALERT RVOL 2.5"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Rvol { multiple: 2.5 })
            ))
        );
        assert_eq!(parse("AAPL ALERT RVOL"), None);
        assert_eq!(parse("AAPL ALERT RVOL 0"), None);
        assert_eq!(parse("AAPL ALERT RVOL 2 BELOW"), None);
        assert!(hint("AAPL ALERT RVOL 2").contains("session RVOL 2.00×"));
        assert_eq!(
            parse("AAPL ALERT PATH"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Session {
                    signal: tr_data::session_alerts::SessionSignal::Path,
                })
            ))
        );
        assert_eq!(
            parse("AAPL ALERT VPACE"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Session {
                    signal: tr_data::session_alerts::SessionSignal::Pace,
                })
            ))
        );
        assert!(matches!(
            parse("NVDA ALERT GFILL"),
            Some(Command::SymbolFunction(_, Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Session { signal })))
                if signal == tr_data::session_alerts::SessionSignal::Gap
        ));
        assert!(matches!(
            parse("AAPL ALERT XTRM"),
            Some(Command::SymbolFunction(_, Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Session { signal })))
                if signal == tr_data::session_alerts::SessionSignal::Extreme
        ));
        assert!(matches!(
            parse("MSFT ALERT RELPATH"),
            Some(Command::SymbolFunction(_, Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Session { signal })))
                if signal == tr_data::session_alerts::SessionSignal::Relative
        ));
        assert_eq!(parse("AAPL ALERT PATH 5"), None);
        assert_eq!(parse("AAPL ALERT HILO"), None);
        assert!(hint("AAPL ALERT PATH").contains("usual session path"));
        assert!(matches!(
            parse("AAPL RVOL"),
            Some(Command::SymbolFunction(_, Function::Indicator(ref spec))) if spec.code == "RVOL"
        ));
        assert!(matches!(
            parse("AAPL VWAP"),
            Some(Command::SymbolFunction(_, Function::Indicator(ref spec))) if spec.code == "VWAP"
        ));
        assert_eq!(
            parse("NVDA ALERT SETUP NEAR_HIGH EXIT"),
            Some(Command::SymbolFunction(
                "NVDA US Equity".into(),
                Function::SetPriceAlert(tr_data::price_alerts::AlertKind::Setup {
                    setup: "NEAR_HIGH".into(),
                    change: tr_data::price_alerts::SetupChange::Exit,
                })
            ))
        );
        assert_eq!(parse("UNALERT AAPL"), Some(Command::Function(Function::ClearPriceAlert("AAPL".into()))));
        assert_eq!(parse("TOP"), Some(Command::Function(Function::TopNews)));
        assert_eq!(
            parse("BTCUSD CRNCY OB 50"),
            Some(Command::SymbolFunction("BTCUSD Curncy".into(), Function::OrderBook(Some(50))))
        );
        assert_eq!(parse("PREFS"), Some(Command::Function(Function::Settings)));
        assert_eq!(parse("update"), Some(Command::Function(Function::Update)));
        assert_eq!(parse("AAPL OMON"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Options)));
        assert_eq!(parse("SPX INDEX OMON"), Some(Command::SymbolFunction("SPX Index".into(), Function::Options)));
        assert_eq!(parse("OMON"), Some(Command::Function(Function::Options)));
        assert_eq!(parse("SIDEBAR RIGHT"), Some(Command::Function(Function::Sidebar("RIGHT".into()))));
    }

    #[test]
    fn portfolio_commands() {
        let f = |v, a: &str| Some(Command::Function(Function::Portfolio(v, a.into())));
        assert_eq!(parse("PORT"), f(PortView::Summary, ""));
        assert_eq!(parse("port refresh"), f(PortView::Summary, "REFRESH"));
        assert_eq!(parse("ACCT"), f(PortView::Accounts, ""));
        assert_eq!(parse("POS"), f(PortView::Positions, ""));
        assert_eq!(parse("ORD"), f(PortView::Orders, ""));
        assert_eq!(parse("ACTV"), f(PortView::Activity, ""));
        assert_eq!(parse("PORT CONNECT"), f(PortView::Summary, "CONNECT"));
        assert_eq!(parse("BROKER"), f(PortView::Summary, "SETUP"));
        // After a security: positions (or orders) in that security.
        assert_eq!(
            parse("AAPL POS"),
            Some(Command::SymbolFunction(
                "AAPL US Equity".into(),
                Function::Portfolio(PortView::Positions, String::new())
            ))
        );
        // `ACT` stays a ticker (Enact Holdings), not the activity view.
        assert_eq!(parse("ACT"), Some(Command::Symbol("ACT US Equity".into())));
        for v in PortView::ALL {
            assert_eq!(parse(v.code()), f(v, ""), "{} round-trips", v.code());
        }
    }

    #[test]
    fn trade_commands() {
        let t = |s: &str| match parse(s) {
            Some(Command::Function(Function::Trade(t))) => Some(t),
            Some(Command::SymbolFunction(k, Function::Trade(mut t))) => {
                t.symbol = Some(k);
                Some(t)
            }
            _ => None,
        };
        let aapl = Some("AAPL US Equity".to_string());
        assert_eq!(
            t("BUY AAPL 10"),
            Some(TradeArgs { buy: Some(true), symbol: aapl.clone(), units: Some(10.0), ..Default::default() })
        );
        assert_eq!(t("buy 10 aapl"), t("BUY AAPL 10"), "quantity and symbol in either order");
        assert_eq!(t("AAPL BUY 10"), t("BUY AAPL 10"), "after a security");
        assert_eq!(t("AAPL US EQUITY BUY 10"), t("BUY AAPL 10"));
        let s = t("SELL MSFT 5 LMT 410.50").unwrap();
        assert_eq!((s.buy, s.units, s.limit), (Some(false), Some(5.0), Some(410.5)));
        assert_eq!(t("SELL MSFT 5 @410.5").unwrap().limit, Some(410.5));
        let st = t("SELL TSLA 3 STP 250 LMT 245").unwrap();
        assert_eq!((st.stop, st.limit), (Some(250.0), Some(245.0)));
        let n = t("BUY NVDA $1,500").unwrap();
        assert_eq!((n.notional, n.units), (Some(1500.0), None));
        assert_eq!(t("BUY 0.5 BRK/B").unwrap().symbol.as_deref(), Some("BRK.B US Equity"));
        let open = t("TRADE").unwrap();
        assert_eq!((open.buy, open.symbol), (None, None));
        assert_eq!(t("TRADE AAPL").unwrap().symbol, aapl);
        assert_eq!(t("BUY").unwrap().buy, Some(true));
        // Ambiguous or malformed orders are rejected rather than guessed.
        assert_eq!(t("BUY AAPL 10 20"), None);
        assert_eq!(t("BUY AAPL 10 $500"), None);
        assert_eq!(t("BUY AAPL LMT"), None);
        assert_eq!(t("BUY AAPL LMT -5"), None);
        assert!(hint("BUY AAPL 10 LMT 150").contains("Buy 10 AAPL limit 150"));
        let all = t("SELL ALL TSLA").unwrap();
        assert_eq!((all.buy, all.all, all.symbol.as_deref()), (Some(false), true, Some("TSLA US Equity")));
        assert_eq!(t("sell tsla all"), t("SELL ALL TSLA"));
        assert_eq!(t("TSLA SELL ALL").map(|a| a.all), Some(true));
        assert!(!t("SELL ALL").unwrap().all, "ALL alone is Allstate");
        assert_eq!(t("BUY ALL TSLA"), None);
        assert_eq!(t("SELL ALL TSLA 5"), None);
        assert_eq!(parse("DEMO"), Some(Command::Function(Function::AccountMode(false))));
        assert_eq!(parse("MODE LIVE"), Some(Command::Function(Function::AccountMode(true))));
    }

    #[test]
    fn symbol_with_function() {
        assert_eq!(parse("NVDA CONG <GO>"), Some(Command::SymbolFunction("NVDA US Equity".into(), Function::Congress)));
        assert_eq!(
            parse("BTCUSD CRNCY 5"),
            Some(Command::SymbolFunction("BTCUSD Curncy".into(), Function::Timeframe(Timeframe::M5)))
        );
        assert_eq!(parse("WEI"), Some(Command::Function(Function::Wei)));
        assert_eq!(parse("GC"), Some(Command::Function(Function::Curve)));
        assert_eq!(parse("SECF gold"), Some(Command::Function(Function::Search("gold".into()))));
        assert_eq!(
            parse("AAPL US EQUITY 1W"),
            Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Timeframe(Timeframe::W1)))
        );
        assert_eq!(parse("AAPL WHATEVER"), None);
    }

    #[test]
    fn hints() {
        assert_eq!(hint("5"), "Interval 5 minutes");
        assert_eq!(hint("msft"), "Load MSFT US Equity — Microsoft Corp");
        assert_eq!(hint("US10Y GOVT"), "Load US10Y Govt — US Treasury 10 Year");
        assert!(hint("").contains("ticker"));
    }

    #[test]
    fn indicator_identity() {
        let spec = |code: &str, params: &[f64]| IndicatorSpec::new(code, params).unwrap();
        assert!(spec("RSI", &[14.0]).same_kind(&spec("RSI", &[21.0])));
        assert!(!spec("SMA", &[20.0]).same_kind(&spec("SMA", &[50.0])));
        assert!(spec("MACD", &[]).is_pane());
    }

    /// Every indicator code and alias reaches the indicator: no function, timeframe or range
    /// spelling shadows it.
    #[test]
    fn every_indicator_code_parses_to_its_indicator() {
        for def in tr_core::indicators::registry::all() {
            for code in std::iter::once(&def.code).chain(def.aliases) {
                match parse(code) {
                    Some(Command::Function(Function::Indicator(s))) => assert_eq!(s.code, def.code, "{code}"),
                    other => panic!("{code} parses as {other:?}"),
                }
                assert_eq!(parse(&format!("IND {code}")), parse(code), "IND {code}");
            }
        }
    }
}

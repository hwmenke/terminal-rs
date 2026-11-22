//! Sidebar destinations and their exact active state.
//!
//! Selection is derived from the focused pane's kind, Studio lens and Quick flag — never from
//! constructing throw-away panes — so each row is lit only for the screen it opens.

use super::PaneKind;
use super::settings_view::SettingsTab;
use crate::command::Function;
use crate::widgets::Icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NavDest {
    Watchlist,
    Studio,
    Quick,
    When,
    Lab,
    Resid,
    Attr,
    Stats,
    Backtest,
    IntradayBt,
    Lookalike,
    TrendWall,
    Rotation,
    SessionCompare,
    SessionPath,
    Breadth,
    VolumePace,
    GapFill,
    SessionExtreme,
    SessionClock,
    RelativePath,
    SessionBrief,
    CashLevels,
    History,
    Wei,
    Curve,
    Options,
    Search,
    Desk,
    Journal,
    Feed,
    Politicians,
    Alerts,
    Settings,
    Help,
}

impl NavDest {
    pub(super) const RESEARCH: [Self; 12] = [
        Self::Studio,
        Self::Quick,
        Self::When,
        Self::Lab,
        Self::Resid,
        Self::Attr,
        Self::Stats,
        Self::Backtest,
        Self::IntradayBt,
        Self::Lookalike,
        Self::TrendWall,
        Self::Rotation,
    ];
    pub(super) const SESSIONS: [Self; 10] = [
        Self::SessionCompare,
        Self::SessionPath,
        Self::Breadth,
        Self::VolumePace,
        Self::GapFill,
        Self::SessionExtreme,
        Self::SessionClock,
        Self::RelativePath,
        Self::SessionBrief,
        Self::CashLevels,
    ];
    pub(super) const MARKETS: [Self; 5] = [Self::History, Self::Wei, Self::Curve, Self::Options, Self::Search];
    /// Every destination, in sidebar order.
    pub(super) const ALL: [Self; 35] = [
        Self::Watchlist,
        Self::Studio,
        Self::Quick,
        Self::When,
        Self::Lab,
        Self::Resid,
        Self::Attr,
        Self::Stats,
        Self::Backtest,
        Self::IntradayBt,
        Self::Lookalike,
        Self::TrendWall,
        Self::Rotation,
        Self::SessionCompare,
        Self::SessionPath,
        Self::Breadth,
        Self::VolumePace,
        Self::GapFill,
        Self::SessionExtreme,
        Self::SessionClock,
        Self::RelativePath,
        Self::SessionBrief,
        Self::CashLevels,
        Self::History,
        Self::Wei,
        Self::Curve,
        Self::Options,
        Self::Search,
        Self::Desk,
        Self::Journal,
        Self::Feed,
        Self::Politicians,
        Self::Alerts,
        Self::Settings,
        Self::Help,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Watchlist => "Open Watchlist Pane",
            Self::Studio => "Studio",
            Self::Quick => "Quick canvas",
            Self::When => "When",
            Self::Lab => "Regression Lab",
            Self::Resid => "Residual tickers",
            Self::Attr => "Attribution",
            Self::Stats => "Statistics",
            Self::Backtest => "Backtest",
            Self::IntradayBt => "Intraday BT",
            Self::Lookalike => "Lookalikes",
            Self::TrendWall => "Trend Wall",
            Self::Rotation => "Rotation",
            Self::SessionCompare => "Session compare",
            Self::SessionPath => "Session path",
            Self::Breadth => "Session breadth",
            Self::VolumePace => "Volume pace",
            Self::GapFill => "Gap fill",
            Self::SessionExtreme => "Session extreme",
            Self::SessionClock => "Session Clock",
            Self::RelativePath => "Relative path",
            Self::SessionBrief => "Session brief",
            // A chart action, not a screen: say so.
            Self::CashLevels => "Add cash levels to chart",
            Self::History => "Price history",
            Self::Wei => "World Markets",
            Self::Curve => "Yield Curve",
            Self::Options => "Options",
            Self::Search => "Security Search",
            Self::Desk => "PM Desk",
            Self::Journal => "Journal",
            Self::Feed => "Latest Trades",
            Self::Politicians => "Politicians",
            Self::Alerts => "Alerts",
            Self::Settings => "Settings",
            Self::Help => "Commands & Hotkeys",
        }
    }

    /// Command mnemonic shown as the quiet trailing hint (`None` where it would only confuse).
    pub(super) fn code(self) -> Option<&'static str> {
        Some(match self {
            Self::Watchlist => return None,
            Self::Studio => "STUDIO",
            Self::Quick => "QUICK",
            Self::When => "WHEN",
            Self::Lab => "STUDIO lab",
            Self::Resid => "STUDIO resid",
            Self::Attr => "STUDIO attr",
            Self::Stats => "STATS",
            Self::Backtest => "BT",
            Self::IntradayBt => "IBT",
            Self::Lookalike => "KNN",
            Self::TrendWall => "TWALL",
            Self::Rotation => "RRG",
            Self::SessionCompare => "ICMP",
            Self::SessionPath => "SPATH",
            Self::Breadth => "BRD",
            Self::VolumePace => "VPACE",
            Self::GapFill => "GFILL",
            Self::SessionExtreme => "XTRM",
            Self::SessionClock => "CLOCK",
            Self::RelativePath => "RPATH",
            Self::SessionBrief => "BRIEF",
            Self::CashLevels => "MARKS",
            Self::History => "HIST",
            Self::Wei => "WEI",
            Self::Curve => "GC",
            Self::Options => "OMON",
            Self::Search => "SECF",
            Self::Desk => "DESK",
            Self::Journal => "JRNL",
            Self::Feed => "CONG",
            Self::Politicians => "POL",
            Self::Alerts => "ALRT",
            // "PDF" was an unexplained mnemonic on Settings; Help carries the real one.
            Self::Settings => return None,
            Self::Help => "HELP",
        })
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Self::Watchlist | Self::History | Self::Journal | Self::Feed => Icon::List,
            Self::Stats => Icon::Gauge,
            Self::Lookalike | Self::Search => Icon::Search,
            Self::Desk => Icon::Grid,
            Self::Wei => Icon::Globe,
            Self::Curve => Icon::Curve,
            Self::Politicians => Icon::People,
            Self::Alerts => Icon::Bell,
            Self::Settings => Icon::Gear,
            Self::Help => Icon::Star,
            _ => Icon::Chart,
        }
    }

    /// What clicking the row runs (never a hidden mode switch: same as typing the command).
    pub(super) fn function(self) -> Function {
        use tr_data::studio::StudioRoute as R;
        let spec = String::new;
        match self {
            Self::Watchlist => Function::Watchlist,
            Self::Studio => Function::Studio { spec: spec() },
            Self::Quick => Function::StudioQuick { spec: spec() },
            Self::When => Function::StudioWhen { spec: spec() },
            Self::Lab => Function::StudioRoute(R::Lab),
            Self::Resid => Function::StudioRoute(R::Resid),
            Self::Attr => Function::StudioRoute(R::Attr),
            Self::Stats => Function::Stats { condition: spec(), conditional: false },
            Self::Backtest => Function::Backtest { spec: spec() },
            Self::IntradayBt => Function::IntradayBacktest { spec: spec() },
            Self::Lookalike => Function::Lookalike { spec: spec() },
            Self::TrendWall => Function::TrendWall { spec: spec() },
            Self::Rotation => Function::Rotation { spec: spec() },
            Self::SessionCompare => Function::IntradayCompare { spec: spec() },
            Self::SessionPath => Function::SessionPath { spec: spec() },
            Self::Breadth => Function::Breadth { spec: spec() },
            Self::VolumePace => Function::VolumePace { spec: spec() },
            Self::GapFill => Function::GapFill { spec: spec() },
            Self::SessionExtreme => Function::SessionExtreme { spec: spec() },
            Self::SessionClock => Function::SessionClock { spec: spec() },
            Self::RelativePath => Function::RelativePath { spec: spec() },
            Self::SessionBrief => Function::SessionBrief,
            Self::CashLevels => Function::CashLevels,
            Self::History => Function::History(spec()),
            Self::Wei => Function::Wei,
            Self::Curve => Function::Curve,
            Self::Options => Function::Options,
            Self::Search => Function::Search(spec()),
            Self::Desk => Function::Desk,
            Self::Journal => Function::Journal(spec()),
            Self::Feed => Function::Congress,
            Self::Politicians => Function::Politician(spec()),
            Self::Alerts => Function::Alerts,
            Self::Settings => Function::Settings,
            Self::Help => Function::Help,
        }
    }

    /// Whether `focused` (the focused pane, if any) is exactly this destination.
    pub(super) fn active(self, focused: Option<&PaneKind>, settings_tab: SettingsTab) -> bool {
        use tr_data::studio::StudioLens;
        let Some(kind) = focused else { return false };
        match (self, kind) {
            (Self::Watchlist, PaneKind::Watchlist) => true,
            // Studio is the lens hub; Quick and When are their own rows.
            (Self::Studio, PaneKind::Studio(s)) => !s.quick && s.lens != StudioLens::When,
            (Self::Quick, PaneKind::Studio(s)) => s.quick,
            (Self::When, PaneKind::Studio(s)) => !s.quick && s.lens == StudioLens::When,
            (Self::Lab, PaneKind::StudioLab(_)) => true,
            (Self::Resid, PaneKind::StudioResid(_)) => true,
            (Self::Attr, PaneKind::StudioAttr(_)) => true,
            (Self::Stats, PaneKind::Stats(_)) => true,
            (Self::Backtest, PaneKind::Backtest(b)) => !b.intraday,
            (Self::IntradayBt, PaneKind::Backtest(b)) => b.intraday,
            (Self::Lookalike, PaneKind::Lookalike(_)) => true,
            (Self::TrendWall, PaneKind::TrendWall(_)) => true,
            (Self::Rotation, PaneKind::Rotation(_)) => true,
            (Self::SessionCompare, PaneKind::IntradayCompare(_)) => true,
            (Self::SessionPath, PaneKind::SessionPath(_)) => true,
            (Self::Breadth, PaneKind::Breadth(_)) => true,
            (Self::VolumePace, PaneKind::VolumePace(_)) => true,
            (Self::GapFill, PaneKind::GapFill(_)) => true,
            (Self::SessionExtreme, PaneKind::SessionExtreme(_)) => true,
            (Self::SessionClock, PaneKind::SessionClock(_)) => true,
            (Self::RelativePath, PaneKind::RelativePath(_)) => true,
            (Self::SessionBrief, PaneKind::SessionBrief(_)) => true,
            // Price history shares `Research` with scan views; only the history flavour is lit.
            (Self::History, PaneKind::Research(r)) => r.history,
            (Self::Wei, PaneKind::Wei) => true,
            (Self::Curve, PaneKind::Curve) => true,
            (Self::Options, PaneKind::Options(_)) => true,
            (Self::Search, PaneKind::Search { .. }) => true,
            (Self::Desk, PaneKind::Desk(_)) => true,
            (Self::Journal, PaneKind::Journal(_)) => true,
            (Self::Feed, PaneKind::Feed { .. }) => true,
            (Self::Politicians, PaneKind::Politicians) => true,
            (Self::Alerts, PaneKind::Alerts) => true,
            // Brokerage settings has its own row; Settings is lit for every other tab.
            (Self::Settings, PaneKind::Settings) => settings_tab != SettingsTab::Brokerage,
            (Self::Help, PaneKind::Help) => true,
            // Cash levels is a chart action; it never owns a pane.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::studio_hub::{StudioPane, StudioViewSpec};
    use tr_data::studio::StudioLens;
    use PaneKind as K;

    const ALL: [NavDest; 35] = NavDest::ALL;

    #[test]
    fn sidebar_sections_cover_every_destination_once() {
        let mut shown: Vec<NavDest> = vec![NavDest::Watchlist, NavDest::Desk, NavDest::Journal];
        shown.extend(NavDest::RESEARCH);
        shown.extend(NavDest::SESSIONS);
        shown.extend(NavDest::MARKETS);
        shown.extend([NavDest::Feed, NavDest::Politicians, NavDest::Alerts, NavDest::Settings, NavDest::Help]);
        for d in ALL {
            assert_eq!(shown.iter().filter(|s| **s == d).count(), 1, "{d:?}");
        }
        assert_eq!(shown.len(), ALL.len());
    }

    fn studio(lens: StudioLens, quick: bool) -> K {
        K::Studio(Box::new(StudioPane::new(StudioViewSpec { lens, quick, ..Default::default() })))
    }
    fn lit(kind: &K) -> Vec<NavDest> {
        ALL.into_iter().filter(|d| d.active(Some(kind), SettingsTab::Appearance)).collect()
    }

    #[test]
    fn studio_rows_follow_lens_and_quick() {
        assert_eq!(lit(&studio(StudioLens::Dist, false)), [NavDest::Studio]);
        assert_eq!(lit(&studio(StudioLens::Relate, false)), [NavDest::Studio]);
        assert_eq!(lit(&studio(StudioLens::When, false)), [NavDest::When]);
        assert_eq!(lit(&studio(StudioLens::Dist, true)), [NavDest::Quick]);
        // Quick wins over the underlying lens.
        assert_eq!(lit(&studio(StudioLens::When, true)), [NavDest::Quick]);
    }

    #[test]
    fn every_destination_lights_exactly_one_row_for_its_pane() {
        use crate::app::*;
        let cases: Vec<(K, NavDest)> = vec![
            (K::Watchlist, NavDest::Watchlist),
            (
                K::StudioLab(Box::new(studio_lab::LabPane::new("", tr_core::Timeframe::D1, tr_data::studio_lab::LabSpec::default()))),
                NavDest::Lab,
            ),
            (K::StudioResid(Box::new(studio_residual::ResidPane::new(String::new()))), NavDest::Resid),
            (K::StudioAttr(Box::new(studio_attribution::AttrPane::new(String::new()))), NavDest::Attr),
            (K::Options(Box::new(OptionsPane::new("AAPL US Equity", 12, true))), NavDest::Options),
            (K::Stats(Box::new(stats::StatsPane::new("", "", false))), NavDest::Stats),
            (K::Backtest(Box::new(backtest::BacktestPane::new("", ""))), NavDest::Backtest),
            (K::Backtest(Box::new(backtest::BacktestPane::intraday("", ""))), NavDest::IntradayBt),
            (K::Lookalike(Box::new(lookalike::LookalikePane::new("", ""))), NavDest::Lookalike),
            (K::TrendWall(Box::new(trend_wall::TrendWallPane::new(""))), NavDest::TrendWall),
            (K::Rotation(Box::new(rotation::RotationPane::new(""))), NavDest::Rotation),
            (K::IntradayCompare(Box::new(compare::ComparePane::new("", ""))), NavDest::SessionCompare),
            (K::SessionPath(Box::new(session_path::SessionPathPane::new("", ""))), NavDest::SessionPath),
            (K::Breadth(Box::new(session_breadth::BreadthPane::new(""))), NavDest::Breadth),
            (K::VolumePace(Box::new(volume_pace::VolumePacePane::new("", ""))), NavDest::VolumePace),
            (K::GapFill(Box::new(gap_fill::GapFillPane::new("", ""))), NavDest::GapFill),
            (K::SessionExtreme(Box::new(session_extreme::SessionExtremePane::new("", ""))), NavDest::SessionExtreme),
            (K::SessionClock(Box::new(session_clock::SessionClockPane::new("", ""))), NavDest::SessionClock),
            (K::RelativePath(Box::new(relative_path::RelativePathPane::new("", ""))), NavDest::RelativePath),
            (K::SessionBrief(Box::new(session_brief::SessionBriefPane::new(""))), NavDest::SessionBrief),
            (K::Research(Box::new(research::ResearchPane::price_history())), NavDest::History),
            (K::Wei, NavDest::Wei),
            (K::Curve, NavDest::Curve),
            (K::Search { query: String::new() }, NavDest::Search),
            (K::Desk(Box::new(desk::DeskPane::new(String::new()))), NavDest::Desk),
            (K::Journal(Box::new(journal::JournalPane::new(String::new()))), NavDest::Journal),
            (K::Feed { filter: Default::default() }, NavDest::Feed),
            (K::Politicians, NavDest::Politicians),
            (K::Alerts, NavDest::Alerts),
            (K::Settings, NavDest::Settings),
            (K::Help, NavDest::Help),
        ];
        for (kind, dest) in cases {
            assert_eq!(lit(&kind), [dest], "{dest:?}");
        }
    }

    #[test]
    fn brokerage_settings_and_settings_do_not_both_light() {
        assert!(NavDest::Settings.active(Some(&K::Settings), SettingsTab::Appearance));
        assert!(!NavDest::Settings.active(Some(&K::Settings), SettingsTab::Brokerage));
        assert!(!NavDest::Settings.active(None, SettingsTab::Appearance));
    }

    #[test]
    fn cash_levels_is_an_action_never_active_and_labelled_as_one() {
        assert!(lit(&K::Watchlist).iter().all(|d| *d != NavDest::CashLevels));
        assert!(NavDest::CashLevels.label().starts_with("Add cash levels"));
        // The stray "PDF" mnemonic is gone.
        assert_eq!(NavDest::Settings.code(), None);
        assert!(ALL.iter().all(|d| d.code() != Some("PDF")));
    }
}

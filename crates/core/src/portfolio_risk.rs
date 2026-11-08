//! Planned downside at supplied stops, not a guaranteed loss limit.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldingKind {
    Stock,
    Etf,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Holding {
    pub account_id: String,
    pub symbol: String,
    pub kind: HoldingKind,
    pub quantity: f64,
    pub mark: Option<f64>,
    pub currency: String,
}

pub type Stops = HashMap<(String, String), f64>;

/// A proposed long position, denominated in the request's base currency.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub symbol: String,
    pub entry: f64,
    pub stop: f64,
    pub quantity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncoveredReason {
    MissingStop,
    MissingPrice,
    UnsupportedKind,
    Short,
    OtherCurrency,
    InvalidValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Covered,
    Uncovered(UncoveredReason),
    Breached,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HoldingRisk {
    pub holding: Holding,
    /// Zero for uncovered and breached rows; consult `status` for coverage.
    pub risk: f64,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskError {
    InvalidCandidate,
    InvalidBudget,
    /// Finite inputs produced an unrepresentable aggregate.
    InvalidTotal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioRisk {
    pub rows: Vec<HoldingRisk>,
    pub covered_risk: f64,
    pub candidate_risk: f64,
    pub total_with_candidate: f64,
    pub remaining_budget: Option<f64>,
    /// False when any holding selected by the account filter is uncovered.
    pub complete: bool,
}

pub struct RiskInput<'a> {
    pub holdings: &'a [Holding],
    pub stops: &'a Stops,
    pub candidate: Option<&'a Candidate>,
    pub budget: Option<f64>,
    /// None includes all accounts; Some selects exactly one account.
    pub account_filter: Option<&'a str>,
    /// Normally "USD". Currency matching is exact; no conversion is performed.
    pub base_currency: &'a str,
}

fn valid(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn holding_status(holding: &Holding, stop: Option<f64>, base: &str) -> Status {
    use UncoveredReason::*;
    let reason = if !holding.quantity.is_finite()
        || holding.mark.is_some_and(|value| !valid(value))
        || stop.is_some_and(|value| !valid(value))
    {
        InvalidValue
    } else if holding.quantity < 0.0 {
        Short
    } else if holding.kind == HoldingKind::Other {
        UnsupportedKind
    } else if holding.currency != base {
        OtherCurrency
    } else {
        match (holding.mark, stop) {
            (None, _) => MissingPrice,
            (_, None) => MissingStop,
            (Some(mark), Some(stop)) => {
                return if mark <= stop { Status::Breached } else { Status::Covered };
            }
        }
    };
    Status::Uncovered(reason)
}

/// Calculate planned downside using only the supplied snapshot and stops.
/// Invalid holding values become uncovered rows. Invalid candidate/budget values
/// reject the request; no misleading candidate or budget total is returned.
pub fn calculate(input: RiskInput<'_>) -> Result<PortfolioRisk, RiskError> {
    if input.budget.is_some_and(|value| !valid(value)) {
        return Err(RiskError::InvalidBudget);
    }
    let candidate_risk = match input.candidate {
        None => 0.0,
        Some(candidate) => {
            if !valid(candidate.entry)
                || !valid(candidate.stop)
                || !valid(candidate.quantity)
                || candidate.stop >= candidate.entry
            {
                return Err(RiskError::InvalidCandidate);
            }
            let risk = candidate.quantity * (candidate.entry - candidate.stop);
            if !risk.is_finite() {
                return Err(RiskError::InvalidCandidate);
            }
            risk
        }
    };
    let mut rows = Vec::new();
    let mut covered_risk = 0.0;
    let mut complete = true;
    for holding in
        input.holdings.iter().filter(|holding| input.account_filter.is_none_or(|account| holding.account_id == account))
    {
        let stop = input.stops.get(&(holding.account_id.clone(), holding.symbol.clone())).copied();
        let mut status = holding_status(holding, stop, input.base_currency);
        let mut risk = 0.0;
        if status == Status::Covered {
            risk = holding.quantity * (holding.mark.unwrap() - stop.unwrap());
            if !risk.is_finite() {
                status = Status::Uncovered(UncoveredReason::InvalidValue);
                risk = 0.0;
            }
        }
        complete &= !matches!(status, Status::Uncovered(_));
        covered_risk += risk;
        rows.push(HoldingRisk { holding: holding.clone(), risk, status });
    }
    let total_with_candidate = covered_risk + candidate_risk;
    if !total_with_candidate.is_finite() {
        return Err(RiskError::InvalidTotal);
    }
    Ok(PortfolioRisk {
        rows,
        covered_risk,
        candidate_risk,
        total_with_candidate,
        remaining_budget: input.budget.map(|budget| budget - total_with_candidate),
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holding(account: &str, symbol: &str, quantity: f64, mark: f64) -> Holding {
        Holding {
            account_id: account.into(),
            symbol: symbol.into(),
            kind: HoldingKind::Stock,
            quantity,
            mark: Some(mark),
            currency: "USD".into(),
        }
    }

    fn input<'a>(holdings: &'a [Holding], stops: &'a Stops) -> RiskInput<'a> {
        RiskInput { holdings, stops, candidate: None, budget: None, account_filter: None, base_currency: "USD" }
    }

    fn stops(values: &[(&str, &str, f64)]) -> Stops {
        values.iter().map(|&(account, symbol, stop)| ((account.into(), symbol.into()), stop)).collect()
    }

    #[test]
    fn hand_calculated_budget() {
        let mut holdings = [holding("a", "X", 100.0, 100.0), holding("a", "Y", 50.0, 80.0)];
        holdings[1].kind = HoldingKind::Etf;
        let stops = stops(&[("a", "X", 95.0), ("a", "Y", 76.0)]);
        let candidate = Candidate { symbol: "Z".into(), entry: 50.0, stop: 47.0, quantity: 20.0 };
        let mut request = input(&holdings, &stops);
        request.candidate = Some(&candidate);
        request.budget = Some(1000.0);
        let result = calculate(request).unwrap();
        assert_eq!(result.rows.iter().map(|row| row.risk).collect::<Vec<_>>(), [500.0, 200.0]);
        assert!(result.rows.iter().all(|row| row.status == Status::Covered));
        assert_eq!(result.covered_risk, 700.0);
        assert_eq!(result.candidate_risk, 60.0);
        assert_eq!(result.total_with_candidate, 760.0);
        assert_eq!(result.remaining_budget, Some(240.0));
        assert!(result.complete);
    }

    #[test]
    fn missing_coverage_and_breaches() {
        let mut holdings = vec![
            holding("a", "X", 100.0, 100.0),
            holding("a", "missing", 1.0, 10.0),
            holding("a", "price", 1.0, 10.0),
            holding("a", "below", 100.0, 90.0),
            holding("a", "equal", 100.0, 95.0),
        ];
        holdings[2].mark = None;
        let stops = stops(&[("a", "X", 95.0), ("a", "price", 5.0), ("a", "below", 95.0), ("a", "equal", 95.0)]);
        let result = calculate(input(&holdings, &stops)).unwrap();
        assert_eq!(result.rows[1].status, Status::Uncovered(UncoveredReason::MissingStop));
        assert_eq!(result.rows[2].status, Status::Uncovered(UncoveredReason::MissingPrice));
        for row in &result.rows[3..] {
            assert_eq!(row.status, Status::Breached);
            assert_eq!(row.risk, 0.0);
        }
        assert_eq!(result.covered_risk, 500.0);
        assert!(!result.complete);
        assert_eq!(result.remaining_budget, None);
        let breached = calculate(input(&holdings[3..], &stops)).unwrap();
        assert!(breached.complete);
    }

    #[test]
    fn accounts_have_separate_stops_and_filtering() {
        let holdings =
            [holding("a", "X", 100.0, 100.0), holding("b", "X", 100.0, 100.0), holding("c", "X", 1.0, 100.0)];
        let stops = stops(&[("a", "X", 95.0), ("b", "X", 90.0)]);
        let result = calculate(input(&holdings, &stops)).unwrap();
        assert_eq!(result.covered_risk, 1500.0);
        assert!(!result.complete);
        let mut request = input(&holdings, &stops);
        request.account_filter = Some("b");
        let result = calculate(request).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.covered_risk, 1000.0);
        assert!(result.complete);
    }

    #[test]
    fn unsupported_positions_are_uncovered() {
        let mut holdings = [
            holding("a", "short", -10.0, 100.0),
            holding("a", "option", 1.0, 10.0),
            holding("a", "foreign", 1.0, 10.0),
        ];
        holdings[1].kind = HoldingKind::Other;
        holdings[2].currency = "EUR".into();
        let result = calculate(input(&holdings, &Stops::new())).unwrap();
        assert_eq!(
            result.rows.iter().map(|row| row.status).collect::<Vec<_>>(),
            [
                Status::Uncovered(UncoveredReason::Short),
                Status::Uncovered(UncoveredReason::UnsupportedKind),
                Status::Uncovered(UncoveredReason::OtherCurrency)
            ]
        );
        assert_eq!(result.covered_risk, 0.0);
        assert!(!result.complete);
    }

    #[test]
    fn invalid_holding_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            for field in 0..3 {
                if field == 0 && value == -1.0 {
                    continue;
                } // Signed shorts have a specific reason.
                let mut holdings = [holding("a", "X", 1.0, 100.0)];
                let mut stops = stops(&[("a", "X", 95.0)]);
                match field {
                    0 => holdings[0].quantity = value,
                    1 => holdings[0].mark = Some(value),
                    _ => {
                        stops.insert(("a".into(), "X".into()), value);
                    }
                }
                let result = calculate(input(&holdings, &stops)).unwrap();
                assert_eq!(result.rows[0].status, Status::Uncovered(UncoveredReason::InvalidValue));
                assert!(!result.complete);
            }
        }
    }

    #[test]
    fn invalid_candidates_and_budgets_are_rejected() {
        let stops = Stops::new();
        for (entry, stop, quantity) in [
            (50.0, 50.0, 20.0),
            (50.0, 51.0, 20.0),
            (f64::NAN, 47.0, 20.0),
            (50.0, f64::INFINITY, 20.0),
            (50.0, 47.0, f64::NAN),
            (-50.0, 47.0, 20.0),
            (50.0, -47.0, 20.0),
            (50.0, 47.0, -20.0),
            (f64::MAX, 0.0, f64::MAX),
        ] {
            let candidate = Candidate { symbol: "X".into(), entry, stop, quantity };
            let mut request = input(&[], &stops);
            request.candidate = Some(&candidate);
            assert_eq!(calculate(request), Err(RiskError::InvalidCandidate));
        }
        for budget in [-1.0, f64::NAN, f64::INFINITY] {
            let mut request = input(&[], &stops);
            request.budget = Some(budget);
            assert_eq!(calculate(request), Err(RiskError::InvalidBudget));
        }
    }
}

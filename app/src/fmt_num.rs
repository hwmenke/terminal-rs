//! Typed number formatting for research tables and strips.
//!
//! Every function is pure and total: missing / non-finite input prints [`MISSING`], never `NaN`, `inf`
//! or a fake zero. Rounded displays have an [`exact`] twin for hover cards. Display scaling is always
//! disclosed as a unit tag (`×10⁻³`) that travels with the numbers, so a scaled value never reads as
//! a different unit.

pub const MISSING: &str = "—";
const MINUS: char = '\u{2212}';

/// Digit-grouped unsigned integer: `24841` → `24,841`.
pub fn count(n: u64) -> String {
    group(&n.to_string())
}

/// Insert thousands separators into the integer part of an unsigned decimal string.
fn group(s: &str) -> String {
    let (int, frac) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));
    let mut out = String::with_capacity(s.len() + int.len() / 3);
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

fn sign_of(v: f64) -> &'static str {
    if v.is_sign_negative() && v != 0. { "\u{2212}" } else { "+" }
}

/// Signed fixed-point with a real minus and thousands separators. A non-zero value that would
/// round to zero prints as `+<0.0001` (digits-wide), never as `+0.0000`.
pub fn signed(v: f64, digits: usize) -> String {
    if !v.is_finite() {
        return MISSING.into();
    }
    let floor = 0.5 * 10f64.powi(-(digits as i32));
    if v != 0. && v.abs() < floor {
        return format!("{}<{:.digits$}", sign_of(v), 10f64.powi(-(digits as i32)));
    }
    if v == 0. {
        return format!("{:.digits$}", 0.0);
    }
    format!("{}{}", sign_of(v), group(&format!("{:.digits$}", v.abs())))
}

/// [`signed`] for an optional value; `None` → `—`.
pub fn signed_opt(v: Option<f64>, digits: usize) -> String {
    v.map_or_else(|| MISSING.into(), |v| signed(v, digits))
}

/// A probability / q-value. Anything that would print `0.000` prints `<0.001`.
pub fn prob(p: Option<f64>) -> String {
    match p {
        Some(p) if p.is_finite() => {
            let p = p.clamp(0., 1.);
            if p < 0.0005 { "<0.001".into() } else { format!("{p:.3}") }
        }
        _ => MISSING.into(),
    }
}

/// A per-observation return (fractional, `0.0012` = 12 bp) with its unit: `+0.12 bp/obs`.
/// `per` is the observation noun (`obs`, `day`, `bar`). Test-only for now: every Lab/strip site this
/// slice touches already carries its unit in a header (`β [bp/bar]`), so a second per-cell unit would
/// repeat it; the helper stays tested for the first site that prints a bare return.
#[cfg(test)]
pub fn ret_bp(frac: f64, per: &str) -> String {
    if !frac.is_finite() {
        return MISSING.into();
    }
    let bp = frac * 1e4;
    if bp == 0. {
        return format!("0.00 bp/{per}");
    }
    format!("{} bp/{per}", signed(bp, 2))
}

/// Shared scale for a set of coefficient-like numbers (β, SE, CI ends). The numbers are divided by
/// `10^exp` (a multiple of three) when the largest magnitude is outside `[1e-3, 1e5)`, and printed with
/// one common number of decimals that keeps three significant digits for the smallest non-zero value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoefScale {
    pub exp: i32,
    pub decimals: usize,
}

impl CoefScale {
    /// Exponent and decimals both from `values`.
    #[cfg(test)]
    pub fn pick(values: impl IntoIterator<Item = f64> + Clone) -> Self {
        Self::pick_with(values.clone(), values)
    }
    /// Exponent from the largest of `values`; decimals from the smallest non-zero entry of `precision`
    /// (the standard errors). Values that are never printed as text (CI ends) or that may sit near
    /// zero by chance (a tiny insignificant β) therefore cannot force false precision. Decimals are
    /// capped at 4: [`signed`] prints `<0.0001` below that and the exact value is on hover.
    pub fn pick_with(values: impl IntoIterator<Item = f64>, precision: impl IntoIterator<Item = f64>) -> Self {
        let finite: Vec<f64> = values.into_iter().filter(|v| v.is_finite()).map(f64::abs).collect();
        let max = finite.iter().copied().fold(0., f64::max);
        let exp = if max == 0. || (1e-3..1e5).contains(&max) { 0 } else { 3 * (max.log10().floor() as i32).div_euclid(3) };
        let scale = 10f64.powi(exp);
        let min = precision.into_iter().filter(|v| v.is_finite()).map(f64::abs).filter(|v| *v > 0.).fold(f64::INFINITY, f64::min) / scale;
        let decimals = if min.is_finite() { (2 - min.log10().floor() as i32).clamp(2, 4) as usize } else { 3 };
        Self { exp, decimals }
    }
    /// `""` for an unscaled set, else `×10⁻³`-style, appended to the unit label.
    pub fn tag(&self) -> String {
        if self.exp == 0 {
            return String::new();
        }
        let sup: String = format!("{}", self.exp)
            .chars()
            .map(|c| match c {
                '-' => '\u{207B}',
                '0' => '\u{2070}',
                '1' => '\u{00B9}',
                '2' => '\u{00B2}',
                '3' => '\u{00B3}',
                d => char::from_u32(0x2070 + d.to_digit(10).unwrap_or(0)).unwrap_or(d),
            })
            .collect();
        format!("×10{sup}")
    }
    /// Scaled, signed value. Missing → `—`.
    pub fn signed(&self, v: Option<f64>) -> String {
        match v {
            Some(v) if v.is_finite() => signed(v / 10f64.powi(self.exp), self.decimals),
            _ => MISSING.into(),
        }
    }
    /// Scaled unsigned magnitude (standard errors).
    pub fn unsigned(&self, v: Option<f64>) -> String {
        match v {
            Some(v) if v.is_finite() => {
                let s = signed(v.abs() / 10f64.powi(self.exp), self.decimals);
                s.trim_start_matches('+').to_string()
            }
            _ => MISSING.into(),
        }
    }
    /// `[lo, hi]` printed with the shared scale.
    #[cfg(test)]
    pub fn interval(&self, ci: Option<(f64, f64)>) -> String {
        match ci {
            Some((a, b)) => format!("[{}, {}]", self.signed(Some(a)), self.signed(Some(b))),
            None => MISSING.into(),
        }
    }
}

/// Full-precision value for hover: shortest round-trip digits, real minus, or `—`.
pub fn exact(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:e}").replace('-', "\u{2212}"),
        _ => MISSING.into(),
    }
}

/// A plain level (σ, κ) in compact form without a forced exponent: fixed for `[1e-3, 1e7)`, else
/// scientific with three significant digits.
pub fn level(v: f64) -> String {
    if !v.is_finite() {
        return MISSING.into();
    }
    let a = v.abs();
    if a == 0. {
        "0".into()
    } else if (1e-3..1e7).contains(&a) {
        let dec = (3 - a.log10().floor() as i32 - 1).clamp(0, 6) as usize;
        let s = group(&format!("{a:.dec$}"));
        if v < 0. { format!("{MINUS}{s}") } else { s }
    } else {
        format!("{v:.2e}").replacen('-', "\u{2212}", 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(24_841), "24,841");
        assert_eq!(count(1_234_567_890), "1,234,567,890");
    }

    #[test]
    fn unavailable_values_are_dashes_never_nan_or_zero() {
        for s in [
            signed(f64::NAN, 2),
            signed(f64::INFINITY, 2),
            prob(None),
            prob(Some(f64::NAN)),
            ret_bp(f64::NEG_INFINITY, "obs"),
            level(f64::NAN),
            exact(None),
            exact(Some(f64::INFINITY)),
            CoefScale::pick([1.0]).signed(None),
            CoefScale::pick([1.0]).unsigned(Some(f64::NAN)),
            CoefScale::pick([1.0]).interval(None),
        ] {
            assert_eq!(s, MISSING);
        }
    }

    #[test]
    fn tiny_signed_values_never_round_to_zero() {
        assert_eq!(signed(1e-9, 4), "+<0.0001");
        assert_eq!(signed(-1e-9, 4), "\u{2212}<0.0001");
        assert_eq!(signed(0.00004, 4), "+<0.0001");
        assert_eq!(signed(0.00006, 4), "+0.0001");
        assert_eq!(signed(0., 3), "0.000");
        assert_eq!(signed(-0.5, 2), "\u{2212}0.50");
        assert_eq!(signed(12_345.6, 2), "+12,345.60");
        assert_eq!(signed(-1234567.891, 1), "\u{2212}1,234,567.9");
    }

    #[test]
    fn probabilities_never_print_zero() {
        assert_eq!(prob(Some(0.)), "<0.001");
        assert_eq!(prob(Some(1e-12)), "<0.001");
        assert_eq!(prob(Some(0.0004)), "<0.001");
        assert_eq!(prob(Some(0.0005)), "0.001");
        assert_eq!(prob(Some(0.043)), "0.043");
        assert_eq!(prob(Some(1.0)), "1.000");
        assert_eq!(prob(Some(1.2)), "1.000");
    }

    #[test]
    fn returns_carry_explicit_units() {
        assert_eq!(ret_bp(0.000012, "obs"), "+0.12 bp/obs");
        assert_eq!(ret_bp(-0.000012, "day"), "\u{2212}0.12 bp/day");
        assert_eq!(ret_bp(0., "obs"), "0.00 bp/obs");
        assert_eq!(ret_bp(1e-9, "obs"), "+<0.01 bp/obs");
        assert_eq!(ret_bp(-1e-9, "obs"), "\u{2212}<0.01 bp/obs");
        assert_eq!(ret_bp(0.1234, "obs"), "+1,234.00 bp/obs");
    }

    #[test]
    fn coefficient_sets_scale_together_and_keep_their_unit() {
        // β, SE and both CI ends share one exponent: ratios and signs are preserved.
        let set = [3.2e-4, 1.1e-5, 1.0e-4, 5.4e-4];
        let s = CoefScale::pick(set);
        assert_eq!(s.exp, -6);
        assert_eq!(s.tag(), "×10\u{207B}\u{2076}");
        let scaled: Vec<f64> = set.iter().map(|v| v / 10f64.powi(s.exp)).collect();
        assert!(scaled.iter().all(|v| (1.0..1000.).contains(v)), "{scaled:?}");
        assert_eq!(s.signed(Some(set[0])), format!("+{:.*}", s.decimals, 320.0));
        assert_eq!(s.unsigned(Some(set[1])), format!("{:.*}", s.decimals, 11.0));
        assert!(s.interval(Some((set[2], set[3]))).starts_with("[+100"));
        // The same set times 10^-3 has the same digits at a different exponent.
        let t = CoefScale::pick(set.map(|v| v * 1e-3));
        assert_eq!(t.exp, -9);
        assert_eq!(t.signed(Some(3.2e-7)), s.signed(Some(3.2e-4)));
        // Ordinary magnitudes are not scaled and are not forced into scientific notation.
        let n = CoefScale::pick([0.42, 0.031]);
        assert_eq!((n.exp, n.tag()), (0, String::new()));
        assert_eq!(n.signed(Some(0.42)), "+0.4200");
        assert_eq!(n.signed(Some(0.031)), "+0.0310");
        assert!(!n.signed(Some(0.031)).contains('e'));
        // Huge values scale by thousands with a visible tag, not an unlabeled shorter number.
        let big = CoefScale::pick([4.2e8, 1.0e6]);
        assert_eq!(big.exp, 6);
        assert_eq!(big.tag(), "×10\u{2076}");
        assert!(big.signed(Some(4.2e8)).starts_with("+420"));
        // Negative values keep a real minus; zero/empty sets stay unscaled.
        assert!(CoefScale::pick([-3.2e-4]).signed(Some(-3.2e-4)).starts_with('\u{2212}'));
        assert_eq!(CoefScale::pick([0.0]).exp, 0);
        assert_eq!(CoefScale::pick(Vec::<f64>::new()).exp, 0);
        assert_eq!(CoefScale::pick([f64::NAN]).exp, 0);
    }

    #[test]
    fn decimals_follow_standard_errors_not_ci_ends_or_tiny_betas() {
        // β = 1.02, SE = 0.05, CI crosses zero with an end at −4e-5 (never printed as text).
        let s = CoefScale::pick_with([1.02, 0.05, -4e-5, 2.0], [0.05]);
        assert_eq!((s.exp, s.decimals), (0, 4));
        assert_eq!(s.signed(Some(1.02)), "+1.0200");
        // A tiny insignificant β of 2e-7 does not inflate the other rows either.
        let t = CoefScale::pick_with([1.02, 2e-7, 0.05], [0.05, 0.04]);
        assert_eq!(t.decimals, 4);
        assert_eq!(t.signed(Some(2e-7)), "+<0.0001");
        // Coarser SEs give fewer decimals (floor of 2).
        assert_eq!(CoefScale::pick_with([42.0], [3.1]).decimals, 2);
        // No SE at all falls back to a neutral 3.
        assert_eq!(CoefScale::pick_with([1.0], Vec::<f64>::new()).decimals, 3);
    }

    #[test]
    fn exact_values_round_trip() {
        assert_eq!(exact(Some(0.000123456789)), "1.23456789e\u{2212}4");
        assert_eq!(exact(Some(-2.5)), "\u{2212}2.5e0");
        assert_eq!(exact(Some(-0.00012)), "\u{2212}1.2e\u{2212}4");
        assert!(!exact(Some(-0.00012)).contains('-'));
        assert_eq!(exact(Some(0.1 + 0.2)).replace('e', " ").split(' ').count(), 2);
    }

    #[test]
    fn levels_avoid_forced_exponents() {
        assert_eq!(level(0.01234), "0.0123");
        assert_eq!(level(1234.6), "1,235");
        assert_eq!(level(12.345), "12.3");
        assert_eq!(level(0.), "0");
        assert_eq!(level(4.2e-9), "4.20e\u{2212}9");
        assert_eq!(level(-3.5e12), "\u{2212}3.50e12");
    }
}

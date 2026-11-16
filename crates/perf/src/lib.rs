//! Offline CPU performance gate. Fixtures are never a production dependency.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    hint::black_box,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub mod fixtures;
pub mod suite;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn output_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root().join("target")).join("perf")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub median_us: f64,
    pub p95_us: f64,
    pub min_us: f64,
    pub n: usize,
}
pub fn summarize(samples: &mut [f64]) -> Summary {
    assert!(!samples.is_empty() && samples.iter().all(|s| s.is_finite() && *s >= 0.0));
    samples.sort_unstable_by(f64::total_cmp);
    let n = samples.len();
    Summary {
        median_us: (samples[(n - 1) / 2] + samples[n / 2]) / 2.0,
        p95_us: samples[(n * 95).div_ceil(100) - 1],
        min_us: samples[0],
        n,
    }
}
fn invoke(f: &mut dyn FnMut() -> usize) {
    assert!(black_box(f()) > 0, "zero work token");
}
pub fn measure(f: &mut dyn FnMut() -> usize) -> Summary {
    let warm = Instant::now();
    let mut calls = 0;
    while calls < 3 || warm.elapsed() < Duration::from_millis(50) {
        invoke(f);
        calls += 1;
    }
    let per_call = (warm.elapsed().as_secs_f64() / calls as f64).max(1e-9);
    let mut k = (0.0002 / per_call).ceil().clamp(1.0, 1_000_000.0) as usize;
    let sample_secs = loop {
        let t = Instant::now();
        for _ in 0..k {
            invoke(f);
        }
        let secs = t.elapsed().as_secs_f64();
        if secs >= 0.0002 || k == 1_000_000 {
            break secs;
        }
        k = (k * 2).min(1_000_000);
    };
    let n = (0.3 / sample_secs).clamp(9.0, 201.0) as usize;
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        for _ in 0..k {
            invoke(f);
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / k as f64);
    }
    summarize(&mut samples)
}
pub fn calibrate() -> f64 {
    let f = || {
        let mut state = 0x123456789abcdefu64;
        let values: Vec<f64> = (0..1 << 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as f64 / u64::MAX as f64
            })
            .collect();
        black_box(values.iter().map(|x| x.sqrt()).sum::<f64>());
        let mut ints: Vec<u64> = (0..200_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            })
            .collect();
        ints.sort_unstable();
        black_box(ints);
    };
    let mut samples = (0..9)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect::<Vec<_>>();
    summarize(&mut samples).median_us
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Budget {
    pub budget_us: f64,
    pub relative: bool,
    #[serde(default)]
    pub class: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub over_target: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Budgets {
    pub schema: u32,
    pub cases: BTreeMap<String, Budget>,
}
impl Budgets {
    pub fn load() -> Result<Self, String> {
        let b: Self = read_json(&root().join("perf/budgets.json"))?;
        if b.schema != 1 || b.cases.values().any(|b| !b.budget_us.is_finite() || b.budget_us <= 0.0) {
            return Err("invalid budget schema or ceiling".into());
        }
        Ok(b)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    Pass,
    Warn(String),
    Fail(String),
    New,
}
pub fn check(s: &Summary, b: &Budget, base: Option<(&Summary, f64)>, calib: f64, slack: f64) -> Verdict {
    if !slack.is_finite()
        || slack <= 0.0
        || !s.median_us.is_finite()
        || !b.budget_us.is_finite()
        || b.budget_us <= 0.0
        || calib <= 0.0
        || !calib.is_finite()
    {
        return Verdict::Fail("invalid measurement, budget, calibration or slack".into());
    }
    if s.median_us > b.budget_us * slack {
        return Verdict::Fail(format!("absolute {:.2} > {:.2} us", s.median_us, b.budget_us * slack));
    }
    let Some((old, old_calib)) = base else { return Verdict::New };
    if old_calib <= 0.0 || !old_calib.is_finite() || old.median_us <= 0.0 || !old.median_us.is_finite() {
        return Verdict::Fail("invalid baseline".into());
    }
    if b.relative {
        let scaled = old.median_us * calib / old_calib;
        let ratio = s.median_us / scaled;
        if ratio > 1.5 && s.median_us - scaled > 20.0 {
            return Verdict::Fail(format!("normalised regression {ratio:.2}x"));
        }
        if ratio > 1.2 {
            return Verdict::Warn(format!("normalised slowdown {ratio:.2}x"));
        }
        if ratio < 0.7 {
            return Verdict::Warn(format!("normalised speedup {ratio:.2}x; review baseline"));
        }
    }
    Verdict::Pass
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Host {
    pub cpu: String,
    pub cores: usize,
    pub os: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: u32,
    pub recorded: String,
    pub profile: String,
    pub host: Host,
    pub calib_us: f64,
    pub cases: BTreeMap<String, Summary>,
}
impl Baseline {
    pub fn new(calib_us: f64) -> Self {
        let cpu = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    l.strip_prefix("model name").and_then(|s| s.split_once(':')).map(|(_, v)| v.trim().to_owned())
                })
            })
            .unwrap_or_else(|| "unknown".into());
        Self {
            schema: 1,
            recorded: chrono::Utc::now().date_naive().to_string(),
            profile: if cfg!(debug_assertions) { "debug" } else { "release" }.into(),
            host: Host {
                cpu,
                cores: std::thread::available_parallelism().map_or(1, usize::from),
                os: std::env::consts::OS.into(),
            },
            calib_us,
            cases: BTreeMap::new(),
        }
    }
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let b: Self = read_json(path)?;
        if b.schema != 1
            || b.calib_us <= 0.0
            || !b.calib_us.is_finite()
            || b.cases.values().any(|s| s.n == 0 || s.median_us <= 0.0 || !s.median_us.is_finite())
        {
            return Err("invalid baseline".into());
        }
        Ok(Some(b))
    }
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}
pub struct Case {
    pub id: &'static str,
    pub run: Box<dyn FnMut() -> usize>,
}
#[derive(Debug)]
pub struct Opts {
    pub filter: Option<String>,
    pub absolute_only: bool,
    pub save_baseline: bool,
    pub slack: f64,
    pub json: PathBuf,
}
impl Default for Opts {
    fn default() -> Self {
        Self {
            filter: None,
            absolute_only: std::env::var_os("TR_PERF_ABSOLUTE").is_some(),
            save_baseline: false,
            slack: 1.0,
            json: output_dir().join("latest.json"),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub measurement: Baseline,
    pub verdicts: BTreeMap<String, Verdict>,
    pub errors: Vec<String>,
    pub slack: f64,
}
impl Report {
    pub fn exit_code(&self) -> i32 {
        if !self.errors.is_empty() {
            2
        } else if self.verdicts.values().any(|v| matches!(v, Verdict::Fail(_))) {
            1
        } else {
            0
        }
    }
}
pub fn validate_ids(cases: &[Case], budgets: &Budgets) -> Result<(), String> {
    let ids: BTreeSet<_> = cases.iter().map(|c| c.id.to_owned()).collect();
    if ids.len() != cases.len() || ids != budgets.cases.keys().cloned().collect() {
        return Err("budget ids and suite ids do not match exactly (or duplicate case)".into());
    }
    Ok(())
}
pub fn run_cases(mut cases: Vec<Case>, budgets: &Budgets, base: Option<&Baseline>, opts: &Opts, calib: f64) -> Report {
    let mut report =
        Report { measurement: Baseline::new(calib), verdicts: BTreeMap::new(), errors: vec![], slack: opts.slack };
    for c in &mut cases {
        let Some(b) = budgets.cases.get(c.id) else {
            report.errors.push(format!("{}: missing budget", c.id));
            continue;
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            invoke(&mut c.run);
            let old = base.and_then(|v| v.cases.get(c.id).map(|s| (s, v.calib_us)));
            let mut effective = b.clone();
            effective.relative &= !opts.absolute_only;
            let mut best = measure(&mut c.run);
            let mut verdict = check(&best, &effective, old, calib, opts.slack);
            for _ in 0..2 {
                if !matches!(verdict, Verdict::Fail(_)) {
                    break;
                }
                let next = measure(&mut c.run);
                if next.median_us < best.median_us {
                    best = next;
                }
                verdict = check(&best, &effective, old, calib, opts.slack);
            }
            println!(
                "{:30} {:10.2} us  p95 {:10.2} / {:10.2}  {:?}",
                c.id,
                best.median_us,
                best.p95_us,
                b.budget_us * opts.slack,
                verdict
            );
            (best, verdict)
        }));
        match result {
            Ok((s, v)) => {
                report.measurement.cases.insert(c.id.into(), s);
                report.verdicts.insert(c.id.into(), v);
            }
            Err(e) => {
                let reason = e
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| e.downcast_ref::<&str>().copied())
                    .unwrap_or("panic");
                report.errors.push(format!("{}: {reason}", c.id));
            }
        }
    }
    report
}
pub fn run_gate(cases: Vec<Case>, opts: &Opts) -> Result<Report, String> {
    let budgets = Budgets::load()?;
    validate_ids(&cases, &budgets)?;
    let base_path = root().join("perf/baseline.json");
    let base = Baseline::load(&base_path)?;
    let cases = cases.into_iter().filter(|c| opts.filter.as_ref().is_none_or(|f| c.id.contains(f))).collect::<Vec<_>>();
    if cases.is_empty() {
        return Err("filter matched no cases".into());
    }
    let calib = calibrate();
    println!("calibration {calib:.2} us; slack {}{}", opts.slack, if opts.slack != 1.0 { " — OVERRIDE" } else { "" });
    let report = run_cases(cases, &budgets, base.as_ref(), opts, calib);
    write_json(&output_dir().join("latest.json"), &report)?;
    if opts.json != output_dir().join("latest.json") {
        write_json(&opts.json, &report)?;
    }
    if opts.save_baseline {
        if opts.filter.is_some() {
            return Err("save-baseline requires the full suite (avoids dropping cases)".into());
        }
        if report.exit_code() != 0 {
            return Err("refusing to save a failing baseline".into());
        }
        // Paint is absolute-only: retain its actual measured values unchanged.
        let mut recorded = Baseline::new(calib);
        recorded.cases = report.measurement.cases.clone();
        if let Some(old) = base {
            for (id, s) in old.cases {
                if id.starts_with("paint.") {
                    recorded.cases.insert(id, s);
                }
            }
        }
        write_json(&base_path, &recorded)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests;

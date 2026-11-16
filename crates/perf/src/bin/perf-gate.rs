use tr_perf::{
    Opts,
    suite::{self, Scale},
};
fn main() {
    let result = std::panic::catch_unwind(run);
    let code = match result {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            eprintln!("harness error: {e}");
            2
        }
        Err(_) => {
            eprintln!("harness panic");
            2
        }
    };
    std::process::exit(code);
}
fn run() -> Result<i32, String> {
    if cfg!(debug_assertions) {
        return Err("perf-gate requires --release".into());
    }
    let mut opts = Opts::default();
    if let Ok(v) = std::env::var("TR_PERF_SLACK") {
        opts.slack = v.parse().map_err(|_| "invalid TR_PERF_SLACK")?;
    }
    if !opts.slack.is_finite() || opts.slack <= 0.0 {
        return Err("slack must be positive and finite".into());
    }
    let mut args = std::env::args().skip(1);
    let mut list = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--filter" => opts.filter = Some(args.next().ok_or("--filter needs a substring")?),
            "--json" => opts.json = args.next().ok_or("--json needs a path")?.into(),
            "--absolute-only" => opts.absolute_only = true,
            "--save-baseline" => opts.save_baseline = true,
            "--list" => list = true,
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    let cases = suite::all(Scale::Full);
    if list {
        for c in cases {
            println!("{}", c.id);
        }
        return Ok(0);
    }
    let r = tr_perf::run_gate(cases, &opts)?;
    for e in &r.errors {
        eprintln!("harness error: {e}");
    }
    let fail = r.verdicts.values().filter(|v| matches!(v, tr_perf::Verdict::Fail(_))).count();
    println!("{} passed, {fail} failed, {} harness errors", r.verdicts.len() - fail, r.errors.len());
    Ok(r.exit_code())
}

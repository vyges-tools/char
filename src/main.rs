//! vyges-char CLI.
//!
//!   vyges-char run   JOB [-o OUT] [--json]   characterize -> Liberty (or JSON)
//!   vyges-char check JOB                      parse + validate the job
//!   vyges-char demo  [-o OUT] [--json]        sample output (no sim)
//!
//! Common flags: -h/--help, -V/--version, -q/--quiet, -v/--verbose.
//! Exit codes: 0 ok · 1 runtime/sim error · 2 usage/validation.

use std::process::exit;

use vyges_char::dataset;
use vyges_char::engine::{self, Characterized};
use vyges_char::job::{AutoCfg, CharJob, SparseCfg};
use vyges_char::library;
use vyges_char::liberty::{self, Arc, Table, Units};
use vyges_char::{sparse, surrogate};

const USAGE: &str = "\
vyges-char — standard-cell timing characterization (SPICE -> Liberty)

usage:
  vyges-char run     JOB      [-o OUT] [--json] [--jobs N]
                              [--sparse RxC [--verify K]] | [--auto [--target PCT]]
                                                  characterize one cell. --sparse: simulate a
                                                  coarse RxC grid, surrogate-fill the rest.
                                                  --auto: self-tuning — keep sampling the
                                                  biggest gap until CV error <= target, fill.
                                                  --jobs: parallelize the per-point sweep.
  vyges-char library MANIFEST  [-o DIR]           characterize many cells (parallel) -> merged .lib
  vyges-char dataset [JOB]    [-o OUT] [--format csv|jsonl] [--clean]
                                                  flatten characterization to a tidy
                                                  training table (no JOB = offline demo)
  vyges-char surrogate [JOB]  [--degree D] [--metric M] [--log] [--json]
                                                  fit a CPU surrogate on a grid subset,
                                                  report held-out error (no JOB = demo)
  vyges-char check   JOB
  vyges-char demo    [-o OUT] [--json]

flags:
  -o FILE          write output to FILE (default: stdout)
  --json           characterization summary as JSON instead of Liberty
  --format FMT     dataset format: csv (default) or jsonl
  --clean          dataset: drop flagged (non-physical, e.g. negative-delay) rows
  --degree D       surrogate polynomial degree per axis (default 2)
  --metric M       surrogate: restrict to one metric (e.g. cell_rise)
  --log            surrogate: fit in log-log space (NLDM grids are log-spaced)
  --sparse RxC     run: simulate only a coarse RxC grid, surrogate-fill the dense .lib
  --verify K       run --sparse: re-simulate K un-fitted points, report the real error
  --jobs N         run: parallelize the per-point ngspice sweep across N threads (N=auto: all cores)
  --auto           run: self-tuning active sampling to a target accuracy, then surrogate-fill
  --target PCT     run --auto: stop when LOO-CV error <= PCT% of peak (default 2.0)
  --max-points N   run --auto: cap simulated points (default: the full grid)
  --seed RxC       run --auto: initial seed grid (default 3x3)
  -q, --quiet      suppress non-essential output
  -v, --verbose    extra detail on stderr
  -h, --help       show this help
  -V, --version    show version
  --bug-report     file a bug (central: vyges/community)
  --feature-request request a feature (central)
  --sponsor        sponsor Vyges (github.com/sponsors/vyges-ip)
  --star           star this tool on GitHub ⭐
";

const BUG_URL: &str =
    "https://github.com/vyges/community/issues/new?template=bug_report_template.yaml";
const FEATURE_URL: &str = "https://github.com/vyges/community/issues/new?labels=enhancement";
const SPONSOR_URL: &str = "https://github.com/sponsors/vyges-ip";
const STAR_URL: &str = "https://github.com/vyges-tools/char";

/// Print a labelled URL; if stdout is a terminal, also try to open it in a browser.
/// In headless / agent contexts (not a TTY) it just prints the URL.
fn link(label: &str, url: &str) {
    use std::io::IsTerminal;
    println!("{label}:\n  {url}");
    if std::io::stdout().is_terminal() {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(url).status();
    }
}

#[derive(Default)]
struct Cli {
    positionals: Vec<String>,
    out: Option<String>,
    format: Option<String>,
    degree: Option<String>,
    metric: Option<String>,
    sparse: Option<String>,
    verify: Option<String>,
    jobs: Option<String>,
    auto: bool,
    target: Option<String>,
    max_points: Option<String>,
    seed: Option<String>,
    log: bool,
    clean: bool,
    json: bool,
    quiet: bool,
    verbose: bool,
    help: bool,
    version: bool,
    bug_report: bool,
    feature_request: bool,
    sponsor: bool,
    star: bool,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                c.out = args.get(i + 1).cloned();
                i += 1;
            }
            "--format" => {
                c.format = args.get(i + 1).cloned();
                i += 1;
            }
            "--degree" => {
                c.degree = args.get(i + 1).cloned();
                i += 1;
            }
            "--metric" => {
                c.metric = args.get(i + 1).cloned();
                i += 1;
            }
            "--sparse" => {
                c.sparse = args.get(i + 1).cloned();
                i += 1;
            }
            "--verify" => {
                c.verify = args.get(i + 1).cloned();
                i += 1;
            }
            "--jobs" => {
                c.jobs = args.get(i + 1).cloned();
                i += 1;
            }
            "--auto" => c.auto = true,
            "--target" => {
                c.target = args.get(i + 1).cloned();
                i += 1;
            }
            "--max-points" => {
                c.max_points = args.get(i + 1).cloned();
                i += 1;
            }
            "--seed" => {
                c.seed = args.get(i + 1).cloned();
                i += 1;
            }
            "--log" => c.log = true,
            "--clean" => c.clean = true,
            "--json" => c.json = true,
            "-q" | "--quiet" => c.quiet = true,
            "-v" | "--verbose" => c.verbose = true,
            "-h" | "--help" => c.help = true,
            "-V" | "--version" => c.version = true,
            "--bug-report" => c.bug_report = true,
            "--feature-request" => c.feature_request = true,
            "--sponsor" => c.sponsor = true,
            "--star" => c.star = true,
            other => c.positionals.push(other.to_string()),
        }
        i += 1;
    }
    c
}

fn write_out(text: &str, cli: &Cli) {
    match &cli.out {
        Some(path) => match std::fs::write(path, text) {
            Ok(_) => {
                if !cli.quiet {
                    println!("wrote {path}");
                }
            }
            Err(e) => {
                eprintln!("error: {path}: {e}");
                exit(1);
            }
        },
        None => print!("{text}"),
    }
}

fn render(library: &str, slews: &[f64], loads: &[f64], arcs: &[Arc], cli: &Cli) -> String {
    if cli.json {
        liberty::render_json(library, slews, loads, arcs)
    } else {
        liberty::render(library, &Units::default(), slews, loads, arcs)
    }
}

/// A sample arc (no simulation) for `demo`.
fn demo_arc() -> (Vec<f64>, Vec<f64>, Arc) {
    let slews = vec![0.01, 0.04, 0.16];
    let loads = vec![0.0005, 0.002, 0.008];
    let t = |base: f64| {
        let mut tb = Table::new(slews.len(), loads.len());
        for i in 0..slews.len() {
            for j in 0..loads.len() {
                tb.values[i][j] = base + 0.05 * i as f64 + 0.1 * j as f64;
            }
        }
        tb
    };
    let arc = Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: t(0.10),
        cell_fall: t(0.09),
        rise_transition: t(0.05),
        fall_transition: t(0.04),
        sigma_rise: Table::new(0, 0),
        sigma_fall: Table::new(0, 0),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(2, 2),
        recv_c2_rise: Table::new(2, 2),
        recv_c1_fall: Table::new(2, 2),
        recv_c2_fall: Table::new(2, 2),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    };
    (slews, loads, arc)
}

/// One surrogate accuracy report line: which (cell, arc, corner, metric) and its
/// held-out error.
struct SurrRow {
    cell: String,
    arc: String,
    corner: String,
    metric: String,
    eval: surrogate::Eval,
}

/// Fit + held-out-evaluate one grid (honoring an optional `--metric` filter) and append
/// a report line. Grids too small to hold out (or to fit even at degree 1) are skipped.
#[allow(clippy::too_many_arguments)]
fn eval_table(
    report: &mut Vec<SurrRow>,
    cell: &str,
    arc: &str,
    corner: &str,
    metric: &str,
    filter: &Option<String>,
    ax1: &[f64],
    ax2: &[f64],
    vals: &[Vec<f64>],
    degree: usize,
    log: bool,
) {
    if let Some(f) = filter {
        if f != metric {
            return;
        }
    }
    let evald = if log {
        surrogate::holdout_eval_log(ax1, ax2, vals, degree)
    } else {
        surrogate::holdout_eval(ax1, ax2, vals, degree)
    };
    if let Some(eval) = evald {
        report.push(SurrRow {
            cell: cell.into(),
            arc: arc.into(),
            corner: corner.into(),
            metric: metric.into(),
            eval,
        });
    }
}

/// Evaluate the surrogate over every timing table of a characterized result.
#[allow(clippy::too_many_arguments)]
fn eval_result(
    report: &mut Vec<SurrRow>,
    corner: &str,
    slews: &[f64],
    loads: &[f64],
    result: &Characterized,
    filter: &Option<String>,
    degree: usize,
    log: bool,
) {
    match result {
        Characterized::Comb(arcs) => {
            for a in arcs {
                let arc = format!("{}->{}", a.in_pin, a.out_pin);
                for (metric, t) in [
                    ("cell_rise", &a.cell_rise),
                    ("cell_fall", &a.cell_fall),
                    ("rise_transition", &a.rise_transition),
                    ("fall_transition", &a.fall_transition),
                ] {
                    eval_table(report, &a.cell, &arc, corner, metric, filter, slews, loads, &t.values, degree, log);
                }
            }
        }
        Characterized::Seq(c) => {
            let arc = format!("{}->{}", c.clock_pin, c.out_pin);
            for (metric, t) in [
                ("ckq_rise", &c.ckq_rise),
                ("ckq_fall", &c.ckq_fall),
                ("ckq_rise_trans", &c.ckq_rise_trans),
                ("ckq_fall_trans", &c.ckq_fall_trans),
            ] {
                eval_table(report, &c.cell, &arc, corner, metric, filter, slews, loads, &t.values, degree, log);
            }
        }
    }
}

/// A synthetic, smooth NLDM-like grid for the offline `surrogate` demo (no sim). The
/// `sqrt(load)` term is deliberately non-polynomial, so a polynomial fit shows a small
/// but honest non-zero held-out error.
fn demo_surrogate_grid() -> (Vec<f64>, Vec<f64>, Vec<Vec<f64>>) {
    let slews: Vec<f64> = vec![0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64];
    let loads: Vec<f64> = vec![0.0005, 0.001, 0.002, 0.004, 0.008, 0.016, 0.032];
    let mut vals = vec![vec![0.0f64; loads.len()]; slews.len()];
    for (i, &s) in slews.iter().enumerate() {
        for (j, &l) in loads.iter().enumerate() {
            vals[i][j] = 0.03 + 0.25 * s + 0.9 * l + 3.5 * s * l + 0.4 * l.sqrt();
        }
    }
    (slews, loads, vals)
}

/// Print the surrogate report as a text table or JSON.
fn print_surrogate(report: &[SurrRow], json: bool) {
    if json {
        let items: Vec<String> = report
            .iter()
            .map(|r| {
                let e = &r.eval;
                format!(
                    "{{\"cell\":\"{}\",\"arc\":\"{}\",\"corner\":\"{}\",\"metric\":\"{}\",\"degree\":{},\"n_train\":{},\"n_test\":{},\"max_abs\":{},\"rms\":{},\"mean_abs\":{},\"scale\":{},\"max_rel_pct\":{},\"rms_rel_pct\":{}}}",
                    r.cell, r.arc, r.corner, r.metric, e.deg_s, e.n_train, e.n_test, e.max_abs, e.rms, e.mean_abs, e.scale, e.max_rel_pct, e.rms_rel_pct
                )
            })
            .collect();
        println!("[{}]", items.join(","));
        return;
    }
    println!(
        "{:<22} {:<10} {:<8} {:<16} {:>3} {:>6} {:>5} {:>11} {:>11} {:>8} {:>8}",
        "cell", "arc", "corner", "metric", "deg", "train", "test", "max_abs", "rms", "max%pk", "rms%pk"
    );
    for r in report {
        let e = &r.eval;
        println!(
            "{:<22} {:<10} {:<8} {:<16} {:>3} {:>6} {:>5} {:>11.5} {:>11.5} {:>7.2}% {:>7.2}%",
            trunc(&r.cell, 22),
            trunc(&r.arc, 10),
            trunc(&r.corner, 8),
            r.metric,
            e.deg_s,
            e.n_train,
            e.n_test,
            e.max_abs,
            e.rms,
            e.max_rel_pct,
            e.rms_rel_pct
        );
    }
}

/// Parse a `--sparse` grid spec: `RxC` (e.g. `4x4`) or a single `N` (meaning `NxN`).
fn parse_grid(s: &str) -> Option<(usize, usize)> {
    let low = s.to_ascii_lowercase();
    let (a, b) = low.split_once('x').unwrap_or((low.as_str(), low.as_str()));
    let r = a.trim().parse::<usize>().ok()?;
    let c = b.trim().parse::<usize>().ok()?;
    (r >= 1 && c >= 1).then_some((r, c))
}

/// Parse an optional integer flag (>= `min`), or exit(2) on a bad value.
fn parse_usize(opt: Option<&str>, default: usize, name: &str, min: usize) -> usize {
    match opt {
        None => default,
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n >= min => n,
            _ => {
                eprintln!("error: {name} must be an integer >= {min} (got {s:?})");
                exit(2);
            }
        },
    }
}

/// Truncate a string to `n` chars for fixed-width display.
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(std::iter::once('…')).collect()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--describe") {
        // Machine-readable description of `run` for tooling that drives it.
        const DESCRIBE: &str = r#"{
  "name": "char",
  "summary": "standard-cell timing characterization (SPICE -> Liberty)",
  "invocation": {
    "args_template": ["run", "{job}"],
    "optional": [
      { "arg": "out", "flag": "-o" },
      { "arg": "jobs", "flag": "--jobs" },
      { "arg": "sparse", "flag": "--sparse" },
      { "arg": "verify", "flag": "--verify" },
      { "arg": "auto", "flag": "--auto" },
      { "arg": "target", "flag": "--target" },
      { "arg": "max_points", "flag": "--max-points" },
      { "arg": "seed", "flag": "--seed" },
      { "arg": "degree", "flag": "--degree" }
    ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "required": ["job"],
    "properties": {
      "job": { "type": "string", "description": "path to the characterization job file (JOB)" },
      "out": { "type": "string", "description": "write output to FILE instead of stdout" },
      "jobs": { "type": "string", "description": "parallelize the per-point ngspice sweep across N threads (N or 'auto')" },
      "sparse": { "type": "string", "description": "simulate only a coarse RxC grid, surrogate-fill the dense .lib" },
      "verify": { "type": "string", "description": "with --sparse: re-simulate K un-fitted points, report the real error" },
      "auto": { "type": "boolean", "description": "self-tuning active sampling to a target accuracy, then surrogate-fill" },
      "target": { "type": "string", "description": "with --auto: stop when LOO-CV error <= PCT% of peak (default 2.0)" },
      "max_points": { "type": "string", "description": "with --auto: cap simulated points (default: the full grid)" },
      "seed": { "type": "string", "description": "with --auto: initial seed grid (default 3x3)" },
      "degree": { "type": "string", "description": "surrogate polynomial degree per axis, used with --sparse or --auto (default 2)" }
    }
  },
  "artifacts": [ { "role": "liberty", "from_arg": "out" } ]
}
"#;
        print!("{DESCRIBE}");
        return;
    }
    let cli = parse_cli(&args);

    if cli.bug_report {
        return link("Report a bug (central — vyges/community)", BUG_URL);
    }
    if cli.feature_request {
        return link("Request a feature (central — vyges/community)", FEATURE_URL);
    }
    if cli.sponsor {
        return link("Sponsor Vyges", SPONSOR_URL);
    }
    if cli.star {
        return link("Star vyges-char on GitHub ⭐", STAR_URL);
    }
    if cli.version {
        println!("vyges-char {} ({})", vyges_char::VERSION, env!("VYGES_GIT_SHA"));
        println!("{}", vyges_char::COPYRIGHT);
        return;
    }
    let cmd = cli.positionals.first().cloned().unwrap_or_default();
    if cli.help || cmd.is_empty() {
        print!("{USAGE}");
        exit(if cmd.is_empty() && !cli.help { 2 } else { 0 });
    }

    match cmd.as_str() {
        "demo" => {
            let (slews, loads, arc) = demo_arc();
            write_out(&render("vyges_char_demo", &slews, &loads, std::slice::from_ref(&arc), &cli), &cli);
        }
        "check" => {
            let Some(path) = cli.positionals.get(1) else {
                eprintln!("usage: vyges-char check JOB");
                exit(2);
            };
            match CharJob::load(path) {
                Ok(j) => println!(
                    "OK  cell={} arc={}->{} {} slews={} loads={}",
                    j.cell, j.in_pin, j.out_pin, j.sense, j.slews.len(), j.loads.len()
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            }
        }
        "run" => {
            let Some(path) = cli.positionals.get(1) else {
                eprintln!("usage: vyges-char run JOB [-o OUT]");
                exit(2);
            };
            let mut job = match CharJob::load(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            };
            if let Some(j) = cli.jobs.as_deref() {
                job.threads = if j.eq_ignore_ascii_case("auto") {
                    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
                } else {
                    match j.parse::<usize>() {
                        Ok(n) if n >= 1 => n,
                        _ => {
                            eprintln!("error: --jobs must be a positive integer or 'auto' (got {j:?})");
                            exit(2);
                        }
                    }
                };
            }
            if cli.auto {
                if cli.sparse.is_some() {
                    eprintln!("error: use --auto OR --sparse, not both");
                    exit(2);
                }
                if job.seq {
                    eprintln!("error: --auto does not support sequential cells (v1)");
                    exit(2);
                }
                if job.power_char || job.ccs || job.recv || job.montecarlo > 0 {
                    eprintln!("error: --auto is NLDM-only in v1 (remove power_char/ccs/recv/montecarlo)");
                    exit(2);
                }
                let degree = parse_usize(cli.degree.as_deref(), 2, "--degree", 1);
                let target = match cli.target.as_deref() {
                    None => 2.0,
                    Some(t) => match t.parse::<f64>() {
                        Ok(v) if v > 0.0 => v,
                        _ => {
                            eprintln!("error: --target must be a positive number (got {t:?})");
                            exit(2);
                        }
                    },
                };
                let dense = job.slews.len() * job.loads.len();
                let max_points = parse_usize(cli.max_points.as_deref(), dense, "--max-points", 1);
                let seed = match cli.seed.as_deref() {
                    None => (3, 3),
                    Some(s) => match parse_grid(s) {
                        Some(g) => g,
                        None => {
                            eprintln!("error: --seed expects RxC (e.g. 3x3) or N (got {s:?})");
                            exit(2);
                        }
                    },
                };
                job.auto = Some(AutoCfg { seed, degree, target_pct: target, max_points });
            }
            if let Some(spec) = cli.sparse.clone() {
                let Some((r, c)) = parse_grid(&spec) else {
                    eprintln!("error: --sparse expects RxC (e.g. 4x4) or N (got {spec:?})");
                    exit(2);
                };
                // Sequential D-flops are supported (engine rejects async + --auto for seq).
                if !job.seq && (job.power_char || job.ccs || job.recv || job.montecarlo > 0) {
                    eprintln!("error: --sparse is NLDM-only in v1 (remove power_char/ccs/recv/montecarlo)");
                    exit(2);
                }
                let degree = match cli.degree.as_deref() {
                    None => 2,
                    Some(d) => match d.parse::<usize>() {
                        Ok(n) if n >= 1 => n,
                        _ => {
                            eprintln!("error: --degree must be a positive integer (got {d:?})");
                            exit(2);
                        }
                    },
                };
                let verify = match cli.verify.as_deref() {
                    None => 0,
                    Some(v) => match v.parse::<usize>() {
                        Ok(n) => n,
                        _ => {
                            eprintln!("error: --verify must be a non-negative integer (got {v:?})");
                            exit(2);
                        }
                    },
                };
                let mm = |xs: &[f64]| {
                    xs.iter()
                        .cloned()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)))
                };
                let (smin, smax) = mm(&job.slews);
                let (lmin, lmax) = mm(&job.loads);
                job.sparse = Some(SparseCfg {
                    sim_slews: sparse::geometric(smin, smax, r),
                    sim_loads: sparse::geometric(lmin, lmax, c),
                    degree,
                    verify,
                });
            }
            if cli.verbose {
                let n = job.corners.len().max(1);
                eprintln!(
                    "characterizing {} ({}x{} grid, {} corner{})",
                    job.cell, job.slews.len(), job.loads.len(), n, if n == 1 { "" } else { "s" }
                );
            }
            match engine::run_corners(&job, cli.json) {
                Ok(libs) => {
                    if job.corners.is_empty() {
                        // single default corner: keep the stdout / -o FILE behaviour.
                        write_out(&libs[0].1, &cli);
                    } else {
                        // per-corner sweep: write <cell>__<corner>.{lib,json} into the
                        // -o directory (default cwd), one file per corner.
                        let dir = cli.out.as_deref().unwrap_or(".");
                        let ext = if cli.json { "json" } else { "lib" };
                        for (name, text) in &libs {
                            let path = format!("{dir}/{}__{name}.{ext}", job.cell);
                            if let Err(e) = std::fs::write(&path, text) {
                                eprintln!("error: writing {path}: {e}");
                                exit(1);
                            }
                            eprintln!("wrote {path}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(1);
                }
            }
        }
        "library" => {
            let Some(path) = cli.positionals.get(1) else {
                eprintln!("usage: vyges-char library MANIFEST [-o OUTDIR]");
                exit(2);
            };
            let ljob = match library::LibraryJob::load(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            };
            if cli.verbose {
                eprintln!("characterizing {} cell(s) on {} thread(s)", ljob.jobs.len(), ljob.threads);
            }
            let t0 = std::time::Instant::now();
            match library::run_library(&ljob) {
                Ok(res) => {
                    let dir = cli.out.as_deref();
                    if let Some(d) = dir {
                        if let Err(e) = std::fs::create_dir_all(d) {
                            eprintln!("error: creating {d}: {e}");
                            exit(1);
                        }
                    }
                    for (corner, text) in &res.libs {
                        match dir {
                            Some(d) => {
                                let name = if corner.is_empty() {
                                    format!("{d}/{}.lib", ljob.library)
                                } else {
                                    format!("{d}/{}__{corner}.lib", ljob.library)
                                };
                                if let Err(e) = std::fs::write(&name, text) {
                                    eprintln!("error: writing {name}: {e}");
                                    exit(1);
                                }
                                eprintln!("wrote {name}");
                            }
                            None => print!("{text}"),
                        }
                    }
                    let ok = res.cells - res.failures.len();
                    eprintln!(
                        "characterized {}/{} cell(s) in {:.1}s",
                        ok, res.cells, t0.elapsed().as_secs_f64()
                    );
                    for (cell, err) in &res.failures {
                        eprintln!("  FAILED {cell}: {err}");
                    }
                    if !res.failures.is_empty() {
                        exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(1);
                }
            }
        }
        "dataset" => {
            let fmt = match cli.format.as_deref() {
                None => dataset::Format::Csv,
                Some(s) => match dataset::Format::parse(s) {
                    Some(f) => f,
                    None => {
                        eprintln!("error: --format must be csv or jsonl (got {s:?})");
                        exit(2);
                    }
                },
            };
            let rows = match cli.positionals.get(1) {
                // No JOB → offline demo dataset from the built-in sample arc (no sim).
                None => {
                    let (slews, loads, arc) = demo_arc();
                    let ctx = dataset::Ctx { corner: "", vdd: 1.8, temp: 25.0, slews: &slews, loads: &loads };
                    dataset::rows_comb(&ctx, std::slice::from_ref(&arc))
                }
                Some(path) => {
                    let job = match CharJob::load(path) {
                        Ok(j) => j,
                        Err(e) => {
                            eprintln!("error: {e}");
                            exit(2);
                        }
                    };
                    let results = match engine::characterize_corners(&job) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: {e}");
                            exit(1);
                        }
                    };
                    let mut rows = Vec::new();
                    for cr in &results {
                        let ctx = dataset::Ctx {
                            corner: &cr.name,
                            vdd: cr.vdd,
                            temp: cr.temp,
                            slews: &job.slews,
                            loads: &job.loads,
                        };
                        match &cr.result {
                            Characterized::Comb(arcs) => rows.extend(dataset::rows_comb(&ctx, arcs)),
                            Characterized::Seq(cell) => rows.extend(dataset::rows_seq(&ctx, cell)),
                        }
                    }
                    rows
                }
            };
            let flagged = rows.iter().filter(|r| !r.flag.is_empty()).count();
            let rows = if cli.clean {
                let (kept, dropped) = dataset::without_flagged(rows);
                if !cli.quiet {
                    eprintln!("dataset: dropped {dropped} flagged (non-physical) row(s)");
                }
                kept
            } else {
                rows
            };
            write_out(&dataset::render(&rows, fmt), &cli);
            if !cli.quiet {
                let note = if !cli.clean && flagged > 0 {
                    format!(" ({flagged} flagged non-physical — see `flag` column, or use --clean)")
                } else {
                    String::new()
                };
                eprintln!("dataset: {} rows{note}", rows.len());
            }
        }
        "surrogate" => {
            let degree = match cli.degree.as_deref() {
                None => 2,
                Some(s) => match s.parse::<usize>() {
                    Ok(d) if d >= 1 => d,
                    _ => {
                        eprintln!("error: --degree must be a positive integer (got {s:?})");
                        exit(2);
                    }
                },
            };
            let filter = cli.metric.clone();
            let mut report: Vec<SurrRow> = Vec::new();
            match cli.positionals.get(1) {
                // No JOB → offline demo on a synthetic smooth grid (no sim).
                None => {
                    let (slews, loads, vals) = demo_surrogate_grid();
                    eval_table(&mut report, "DEMO", "A->Y", "", "cell_rise", &filter, &slews, &loads, &vals, degree, cli.log);
                }
                Some(path) => {
                    let job = match CharJob::load(path) {
                        Ok(j) => j,
                        Err(e) => {
                            eprintln!("error: {e}");
                            exit(2);
                        }
                    };
                    let results = match engine::characterize_corners(&job) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: {e}");
                            exit(1);
                        }
                    };
                    for cr in &results {
                        eval_result(&mut report, &cr.name, &job.slews, &job.loads, &cr.result, &filter, degree, cli.log);
                    }
                }
            }
            if report.is_empty() {
                eprintln!(
                    "surrogate: nothing to evaluate (grid too small to hold out, or --metric matched nothing)"
                );
                exit(1);
            }
            print_surrogate(&report, cli.json);
        }
        other => {
            eprintln!("vyges-char: unknown command {other:?}\n");
            print!("{USAGE}");
            exit(2);
        }
    }
}

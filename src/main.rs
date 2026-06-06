//! vyges-char CLI.
//!
//!   vyges-char run   JOB [-o OUT] [--json]   characterize -> Liberty (or JSON)
//!   vyges-char check JOB                      parse + validate the job
//!   vyges-char demo  [-o OUT] [--json]        sample output (no sim)
//!
//! Common flags: -h/--help, -V/--version, -q/--quiet, -v/--verbose.
//! Exit codes: 0 ok · 1 runtime/sim error · 2 usage/validation.

use std::process::exit;

use vyges_char::engine;
use vyges_char::job::CharJob;
use vyges_char::library;
use vyges_char::liberty::{self, Arc, Table, Units};

const USAGE: &str = "\
vyges-char — standard-cell timing characterization (SPICE -> Liberty)

usage:
  vyges-char run     JOB      [-o OUT] [--json]   characterize one cell
  vyges-char library MANIFEST  [-o DIR]           characterize many cells (parallel) -> merged .lib
  vyges-char check   JOB
  vyges-char demo    [-o OUT] [--json]

flags:
  -o FILE          write output to FILE (default: stdout)
  --json           characterization summary as JSON instead of Liberty
  -q, --quiet      suppress non-essential output
  -v, --verbose    extra detail on stderr
  -h, --help       show this help
  -V, --version    show version
";

#[derive(Default)]
struct Cli {
    positionals: Vec<String>,
    out: Option<String>,
    json: bool,
    quiet: bool,
    verbose: bool,
    help: bool,
    version: bool,
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
            "--json" => c.json = true,
            "-q" | "--quiet" => c.quiet = true,
            "-v" | "--verbose" => c.verbose = true,
            "-h" | "--help" => c.help = true,
            "-V" | "--version" => c.version = true,
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli(&args);

    if cli.version {
        println!("vyges-char {}", vyges_char::VERSION);
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
            let job = match CharJob::load(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            };
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
        other => {
            eprintln!("vyges-char: unknown command {other:?}\n");
            print!("{USAGE}");
            exit(2);
        }
    }
}

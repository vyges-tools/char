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
use vyges_char::liberty::{self, Arc, Table, Units};

const USAGE: &str = "\
vyges-char — standard-cell timing characterization (SPICE -> Liberty)

usage:
  vyges-char run   JOB [-o OUT] [--json]
  vyges-char check JOB
  vyges-char demo  [-o OUT] [--json]

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

fn render(library: &str, slews: &[f64], loads: &[f64], arc: &Arc, cli: &Cli) -> String {
    if cli.json {
        liberty::render_json(library, slews, loads, std::slice::from_ref(arc))
    } else {
        liberty::render(library, &Units::default(), slews, loads, std::slice::from_ref(arc))
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
    };
    (slews, loads, arc)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli(&args);

    if cli.version {
        println!("vyges-char {}", vyges_char::VERSION);
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
            write_out(&render("vyges_char_demo", &slews, &loads, &arc, &cli), &cli);
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
                eprintln!("characterizing {} ({}x{} grid)", job.cell, job.slews.len(), job.loads.len());
            }
            match engine::characterize(&job) {
                Ok(arc) => {
                    let lib = format!("{}_char", job.cell);
                    write_out(&render(&lib, &job.slews, &job.loads, &arc, &cli), &cli);
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

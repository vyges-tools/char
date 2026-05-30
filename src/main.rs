//! vyges-char CLI.
//!
//!   vyges-char run   JOB [-o OUT.lib]   characterize -> Liberty (needs ngspice)
//!   vyges-char check JOB                parse + validate the job, print summary
//!   vyges-char demo  [-o OUT.lib]       emit a sample .lib (no sim) to show output

use std::process::exit;

use vyges_char::engine;
use vyges_char::job::CharJob;
use vyges_char::liberty::{self, Arc, Table, Units};

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn write_out(text: &str, out: Option<String>) {
    match out {
        Some(path) => match std::fs::write(&path, text) {
            Ok(_) => println!("wrote {path}"),
            Err(e) => {
                eprintln!("error: {path}: {e}");
                exit(1);
            }
        },
        None => print!("{text}"),
    }
}

fn demo_lib() -> String {
    let slews = vec![0.01, 0.04, 0.16];
    let loads = vec![0.0005, 0.002, 0.008];
    let t = |base: f64| {
        let mut tb = Table::new(slews.len(), loads.len());
        for (i, _) in slews.iter().enumerate() {
            for (j, _) in loads.iter().enumerate() {
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
    };
    liberty::render("vyges_char_demo", &Units::default(), &slews, &loads, &[arc])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    match cmd {
        "--version" | "-V" => println!("vyges-char {}", vyges_char::VERSION),
        "demo" => write_out(&demo_lib(), arg_after(&args, "-o")),
        "check" => {
            let Some(path) = args.get(1) else {
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
            let Some(path) = args.get(1) else {
                eprintln!("usage: vyges-char run JOB [-o OUT.lib]");
                exit(2);
            };
            let job = match CharJob::load(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            };
            match engine::run_to_lib(&job) {
                Ok(lib) => write_out(&lib, arg_after(&args, "-o")),
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(1);
                }
            }
        }
        _ => {
            eprintln!("vyges-char {}\nusage: vyges-char <run|check|demo|--version>", vyges_char::VERSION);
            exit(2);
        }
    }
}

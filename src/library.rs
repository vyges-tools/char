//! Library-scale characterization: run many cells and emit one merged `.lib`.
//!
//! A `.charlib` manifest names a set of per-cell `.char` jobs (which are reused
//! verbatim — every cell type the single-cell path supports works here) and a
//! thread count. Cells are characterized **in parallel** (each `ngspice` point
//! is a subprocess; the bottleneck is the simulator, so the pool gives a near
//! linear speed-up across cores), then merged per corner into a single library.
//!
//! ```text
//! library:  sky130_fd_sc_hd_subset
//! threads:  8                       # default: available parallelism
//! jobs:     cells/inv_1.char, cells/nand2_1.char, cells/dfxtp_1.char
//! #jobs_dir: cells                  # alternative: every *.char in a directory
//! ```

use crate::engine::{run_corners, CharError};
use crate::job::CharJob;
use crate::liberty;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct LibraryJob {
    pub library: String,
    pub jobs: Vec<String>, // paths to per-cell `.char` jobs (resolved against base_dir)
    pub threads: usize,
    pub base_dir: String,
}

#[derive(Debug)]
pub struct LibraryError(pub String);
impl std::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "library job error: {}", self.0)
    }
}
impl std::error::Error for LibraryError {}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

impl LibraryJob {
    pub fn parse(text: &str, base_dir: &str) -> Result<LibraryJob, LibraryError> {
        let mut library = String::new();
        let mut jobs: Vec<String> = Vec::new();
        let mut jobs_dir: Option<String> = None;
        let mut threads = 0usize;
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| LibraryError(format!("expected 'key: value', got {line:?}")))?;
            let (k, v) = (k.trim().to_lowercase(), v.trim().to_string());
            match k.as_str() {
                "library" => library = v,
                "threads" => {
                    threads = v.parse().map_err(|_| LibraryError(format!("bad threads: {v:?}")))?
                }
                "jobs" => jobs.extend(
                    v.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
                ),
                "jobs_dir" => jobs_dir = Some(v),
                other => return Err(LibraryError(format!("unknown key: {other}"))),
            }
        }
        // a directory of `.char` files (sorted) appends to any explicit jobs.
        if let Some(dir) = &jobs_dir {
            let resolved = resolve(base_dir, dir);
            let mut found: Vec<String> = std::fs::read_dir(&resolved)
                .map_err(|e| LibraryError(format!("{resolved}: {e}")))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("char"))
                .filter_map(|p| p.to_str().map(|s| s.to_string()))
                .collect();
            found.sort();
            jobs.extend(found);
        }
        if library.is_empty() {
            return Err(LibraryError("missing key: library".into()));
        }
        if jobs.is_empty() {
            return Err(LibraryError("no jobs (set `jobs:` or `jobs_dir:`)".into()));
        }
        if threads == 0 {
            threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        }
        Ok(LibraryJob { library, jobs, threads, base_dir: base_dir.to_string() })
    }

    pub fn load(path: &str) -> Result<LibraryJob, LibraryError> {
        let text = std::fs::read_to_string(path).map_err(|e| LibraryError(format!("{path}: {e}")))?;
        let base = Path::new(path).parent().and_then(|p| p.to_str()).unwrap_or(".");
        LibraryJob::parse(&text, base)
    }
}

fn resolve(base: &str, rel: &str) -> String {
    if Path::new(rel).is_absolute() || base.is_empty() {
        rel.to_string()
    } else {
        Path::new(base).join(rel).to_string_lossy().into_owned()
    }
}

/// Result of a library run: one merged `.lib` per corner, the cells that failed
/// (name + error — never aborts the whole run), and the cell count attempted.
pub struct LibraryResult {
    pub libs: Vec<(String, String)>, // (corner_name, merged_lib_text)
    pub failures: Vec<(String, String)>, // (cell, error)
    pub cells: usize,
}

/// Characterize every cell in the manifest (in parallel) and merge per corner.
/// A cell that fails to characterize is recorded in `failures` and dropped from
/// the merge — a single bad cell never sinks the whole library.
pub fn run_library(ljob: &LibraryJob) -> Result<LibraryResult, CharError> {
    let mut jobs: Vec<CharJob> = Vec::with_capacity(ljob.jobs.len());
    for p in &ljob.jobs {
        let path = resolve(&ljob.base_dir, p);
        jobs.push(CharJob::load(&path).map_err(|e| CharError::Io(format!("{path}: {e}")))?);
    }

    // per-cell outcome: Ok = (corner, lib) per corner; Err = the failure message.
    type CellOutcome = Result<Vec<(String, String)>, String>;
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<CellOutcome>>> =
        Mutex::new((0..jobs.len()).map(|_| None).collect());
    let nthreads = ljob.threads.clamp(1, jobs.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..nthreads {
            let (jobs, next, results) = (&jobs, &next, &results);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() {
                    break;
                }
                let r = run_corners(&jobs[i], false).map_err(|e| e.to_string());
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });

    // regroup by corner, in cell (input) order, recording failures.
    let res = results.into_inner().unwrap();
    let mut by_corner: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut failures = Vec::new();
    for (i, r) in res.into_iter().enumerate() {
        match r.expect("every index assigned") {
            Ok(corner_libs) => {
                for (cn, lib) in corner_libs {
                    if !order.contains(&cn) {
                        order.push(cn.clone());
                    }
                    by_corner.entry(cn).or_default().push(lib);
                }
            }
            Err(e) => failures.push((jobs[i].cell.clone(), e)),
        }
    }

    let libs = order
        .into_iter()
        .map(|cn| {
            let name = if cn.is_empty() {
                ljob.library.clone()
            } else {
                format!("{}__{}", ljob.library, cn)
            };
            let merged = liberty::merge_libraries(&name, &by_corner[&cn]);
            (cn, merged)
        })
        .collect();

    Ok(LibraryResult { libs, failures, cells: jobs.len() })
}

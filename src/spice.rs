//! SPICE deck generation + ngspice `.measure` output parsing.
//!
//! Deck generation and parsing are pure std (unit-tested). The actual sim run
//! lives in `engine` and shells out to `ngspice`.

use std::collections::HashMap;

/// Build a transient deck that drives `in_pin` with a ramp of `slew_ns` and
/// loads `out_pin` with `load_pf`, measuring propagation delay + output slew.
/// `subckt_call` is the instance line for the cell-under-test (caller wires
/// the pin order); `includes` are model/netlist `.include` lines.
#[allow(clippy::too_many_arguments)]
pub fn deck(
    title: &str,
    includes: &[String],
    osdi: &[String],
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    vdd: f64,
    slew_ns: f64,
    load_pf: f64,
    rising_input: bool,
    mc: Option<u64>,
) -> String {
    let (v0, v1) = if rising_input { (0.0, vdd) } else { (vdd, 0.0) };
    let half = vdd / 2.0;
    let mut s = String::new();
    s.push_str(&format!("* {title}\n"));
    // OSDI device models (e.g. PSP103 / HICUM via OpenVAF) must be registered
    // before the netlist's `.model` cards are parsed — `pre_osdi` in a leading
    // control block does that; the Monte-Carlo RNG seed is set in the same block
    // (`set rndseed`) so each mismatch sample is independent. Needed for PDKs whose
    // devices are Verilog-A/OSDI (IHP sg13g2, mixed-signal/BCD); empty otherwise.
    if !osdi.is_empty() || mc.is_some() {
        s.push_str(".control\n");
        for o in osdi {
            // ngspice `pre_osdi` takes the rest of the line as the path literally —
            // quotes would become part of the filename, so emit it unquoted.
            s.push_str(&format!("pre_osdi {o}\n"));
        }
        if let Some(seed) = mc {
            s.push_str(&format!("set rndseed={seed}\n"));
        }
        s.push_str(".endc\n");
    }
    // LVF Monte-Carlo: enable device mismatch (sky130/gf180 convention) so the
    // model's agauss/mismatch terms vary per seeded run.
    if mc.is_some() {
        s.push_str(".param mc_mm_switch=1\n");
    }
    for inc in includes {
        s.push_str(&format!(".include \"{inc}\"\n"));
    }
    s.push_str(&format!("VVDD VDD 0 {vdd}\n"));
    s.push_str("VVSS VSS 0 0\n");
    // PWL input ramp: flat, then ramp over slew_ns starting at 1ns.
    s.push_str(&format!(
        "VIN {in_pin} 0 PWL(0 {v0} 1n {v0} {}n {v1})\n",
        1.0 + slew_ns
    ));
    s.push_str(subckt_call);
    if !subckt_call.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!("CL {out_pin} 0 {load_pf}p\n"));
    s.push_str(".tran 1p 20n\n");
    let in_dir = if rising_input { "RISE=1" } else { "FALL=1" };
    let out_dir = if rising_input { "FALL=1" } else { "RISE=1" }; // inverting arc
    s.push_str(&format!(
        ".measure tran prop_delay TRIG v({in_pin}) VAL={half} {in_dir} \
         TARG v({out_pin}) VAL={half} {out_dir}\n"
    ));
    // output transition 20%-80% (or 80%-20%)
    let (lo, hi) = (0.2 * vdd, 0.8 * vdd);
    if rising_input {
        s.push_str(&format!(
            ".measure tran out_slew TRIG v({out_pin}) VAL={hi} FALL=1 \
             TARG v({out_pin}) VAL={lo} FALL=1\n"
        ));
    } else {
        s.push_str(&format!(
            ".measure tran out_slew TRIG v({out_pin}) VAL={lo} RISE=1 \
             TARG v({out_pin}) VAL={hi} RISE=1\n"
        ));
    }
    s.push_str(".end\n");
    s
}

/// Find a cell's port list from its `.subckt` definition in a netlist.
///
/// Returns the pin names in declared order (e.g. sky130's
/// `A VGND VNB VPB VPWR Y`), folding `+` continuation lines. Case-insensitive
/// match on the cell name. `None` if the cell has no `.subckt` here.
pub fn parse_subckt_pins(netlist: &str, cell: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = netlist.lines().collect();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let mut toks = line.split_whitespace();
        let Some(kw) = toks.next() else { continue }; // skip blank lines
        if !kw.eq_ignore_ascii_case(".subckt") {
            continue;
        }
        let Some(name) = toks.next() else { continue };
        if !name.eq_ignore_ascii_case(cell) {
            continue;
        }
        let mut pins: Vec<String> = toks.map(|s| s.to_string()).collect();
        // fold `+` continuation lines
        for cont in &lines[i + 1..] {
            let c = cont.trim();
            if let Some(rest) = c.strip_prefix('+') {
                pins.extend(rest.split_whitespace().map(|s| s.to_string()));
            } else {
                break;
            }
        }
        // a parameterized subckt may carry `name=value` tails — drop them
        pins.retain(|p| !p.contains('='));
        return Some(pins);
    }
    None
}

/// Parse ngspice `.measure` results from stdout/log.
/// Lines look like: `prop_delay           =  1.234560e-10 targ= ...`
pub fn parse_measures(output: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some((lhs, rhs)) = line.split_once('=') {
            let name = lhs.trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                continue;
            }
            // value is the first token after '='
            if let Some(tok) = rhs.split_whitespace().next() {
                if let Ok(v) = tok.parse::<f64>() {
                    out.insert(name.to_string(), v);
                }
            }
        }
    }
    out
}

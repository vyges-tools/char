//! SPICE deck generation + ngspice `.measure` output parsing.
//!
//! Deck generation and parsing are pure std (unit-tested). The actual sim run
//! lives in `engine` and shells out to `ngspice`.

use std::collections::HashMap;

/// Build a transient deck that drives `in_pin` with a ramp of `slew_ns` and
/// loads `out_pin` with `load_pf`, measuring propagation delay + output slew.
/// `subckt_call` is the instance line for the cell-under-test (caller wires
/// the pin order); `includes` are model/netlist `.include` lines.
pub fn deck(
    title: &str,
    includes: &[String],
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    vdd: f64,
    slew_ns: f64,
    load_pf: f64,
    rising_input: bool,
) -> String {
    let (v0, v1) = if rising_input { (0.0, vdd) } else { (vdd, 0.0) };
    let half = vdd / 2.0;
    let mut s = String::new();
    s.push_str(&format!("* {title}\n"));
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
            if let Some(tok) = rhs.trim().split_whitespace().next() {
                if let Ok(v) = tok.parse::<f64>() {
                    out.insert(name.to_string(), v);
                }
            }
        }
    }
    out
}

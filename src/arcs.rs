//! Arc auto-derivation from a Liberty Boolean `function`.
//!
//! A real netlist instantiates cells whose timing arcs (which input drives which
//! output, the unateness, and the side-input state that sensitizes the path) are
//! tedious and error-prone to hand-write — especially XORs (non-unate) and
//! AOI/OAI compounds. Given the output pin's Boolean `function` (which every PDK
//! ships in its `.lib`), this module derives those arcs automatically by
//! **cofactor analysis**: for each input, sweep all assignments of the other
//! inputs (standard cells have ≤6), find one that makes the output respond, and
//! read off the local sense from that point. char then measures the values.
//!
//! Liberty function operators: `'` / `!` NOT, `*` / `&` / juxtaposition AND,
//! `^` XOR, `+` / `|` OR (precedence: NOT > AND > XOR > OR).

use crate::job::ArcSpec;

#[derive(Debug, Clone)]
enum Expr {
    Var(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    LParen,
    RParen,
    Not,
    Prime, // postfix NOT
    And,
    Or,
    Xor,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let mut t = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => t.push(Tok::LParen),
            ')' => t.push(Tok::RParen),
            '!' => t.push(Tok::Not),
            '\'' => t.push(Tok::Prime),
            '*' | '&' => t.push(Tok::And),
            '+' | '|' => t.push(Tok::Or),
            '^' => t.push(Tok::Xor),
            _ if c.is_alphanumeric() || c == '_' || c == '[' || c == ']' => {
                let start = i;
                while i < cs.len()
                    && (cs[i].is_alphanumeric() || cs[i] == '_' || cs[i] == '[' || cs[i] == ']')
                {
                    i += 1;
                }
                t.push(Tok::Ident(cs[start..i].iter().collect()));
                continue;
            }
            _ => return Err(format!("unexpected char {c:?} in function")),
        }
        i += 1;
    }
    Ok(t)
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let v = self.t.get(self.i).cloned();
        self.i += 1;
        v
    }
    // or := xor (('+'|'|') xor)*
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_xor()?;
        while self.peek() == Some(&Tok::Or) {
            self.next();
            e = Expr::Or(Box::new(e), Box::new(self.parse_xor()?));
        }
        Ok(e)
    }
    // xor := and ('^' and)*
    fn parse_xor(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_and()?;
        while self.peek() == Some(&Tok::Xor) {
            self.next();
            e = Expr::Xor(Box::new(e), Box::new(self.parse_and()?));
        }
        Ok(e)
    }
    // and := unary (('*'|'&')? unary)*    (juxtaposition = AND)
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(&Tok::And) => {
                    self.next();
                    e = Expr::And(Box::new(e), Box::new(self.parse_unary()?));
                }
                // implicit AND: another operand starts with no operator between
                Some(&Tok::Ident(_)) | Some(&Tok::LParen) | Some(&Tok::Not) => {
                    e = Expr::And(Box::new(e), Box::new(self.parse_unary()?));
                }
                _ => break,
            }
        }
        Ok(e)
    }
    // unary := '!' unary | primary "'"*
    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        let mut e = self.parse_primary()?;
        while self.peek() == Some(&Tok::Prime) {
            self.next();
            e = Expr::Not(Box::new(e));
        }
        Ok(e)
    }
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Ident(name)) => Ok(Expr::Var(name)),
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                if self.next() != Some(Tok::RParen) {
                    return Err("missing ')'".into());
                }
                Ok(e)
            }
            other => Err(format!("expected operand, got {other:?}")),
        }
    }
}

fn parse(s: &str) -> Result<Expr, String> {
    let mut p = Parser { t: lex(s)?, i: 0 };
    let e = p.parse_or()?;
    if p.i != p.t.len() {
        return Err("trailing tokens in function".into());
    }
    Ok(e)
}

fn eval(e: &Expr, env: &std::collections::HashMap<&str, bool>) -> bool {
    match e {
        Expr::Var(n) => *env.get(n.as_str()).unwrap_or(&false),
        Expr::Not(a) => !eval(a, env),
        Expr::And(a, b) => eval(a, env) && eval(b, env),
        Expr::Or(a, b) => eval(a, env) || eval(b, env),
        Expr::Xor(a, b) => eval(a, env) ^ eval(b, env),
    }
}

/// Ordered, de-duplicated variable names as they first appear.
fn vars(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Var(n) => {
            if !out.iter().any(|x| x == n) {
                out.push(n.clone());
            }
        }
        Expr::Not(a) => vars(a, out),
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Xor(a, b) => {
            vars(a, out);
            vars(b, out);
        }
    }
}

/// Derive the timing arcs for one output pin from its Boolean function.
///
/// For each input that the output depends on, sweep all 2^(n-1) assignments of
/// the other inputs; the first that flips the output is the sensitizing side
/// state, and the direction there gives the arc's sense (positive_unate if the
/// output rises with the input, negative_unate if it falls). Inputs the output
/// never responds to (redundant in this function) produce no arc.
pub fn derive_arcs(out_pin: &str, function: &str) -> Result<Vec<ArcSpec>, String> {
    let e = parse(function)?;
    let mut ins = Vec::new();
    vars(&e, &mut ins);
    if ins.len() > 16 {
        return Err(format!(
            "function has {} inputs (>16); refusing brute force",
            ins.len()
        ));
    }
    let mut arcs = Vec::new();
    for (xi, x) in ins.iter().enumerate() {
        let others: Vec<&String> = ins
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != xi)
            .map(|(_, n)| n)
            .collect();
        let mut found: Option<(bool, Vec<(String, bool)>)> = None;
        for combo in 0..(1u32 << others.len()) {
            let mut env = std::collections::HashMap::new();
            let mut side = Vec::with_capacity(others.len());
            for (b, o) in others.iter().enumerate() {
                let v = (combo >> b) & 1 == 1;
                env.insert(o.as_str(), v);
                side.push(((*o).clone(), v));
            }
            env.insert(x.as_str(), false);
            let f0 = eval(&e, &env);
            env.insert(x.as_str(), true);
            let f1 = eval(&e, &env);
            if f0 != f1 {
                // output rises with x (f1 && !f0) -> positive_unate, else negative.
                found = Some((f1 && !f0, side));
                break;
            }
        }
        if let Some((rises, side)) = found {
            arcs.push(ArcSpec {
                in_pin: x.clone(),
                out_pin: out_pin.to_string(),
                sense: if rises {
                    "positive_unate"
                } else {
                    "negative_unate"
                }
                .to_string(),
                side,
            });
        }
    }
    if arcs.is_empty() {
        return Err(format!(
            "function {function:?} yields no timing arcs for {out_pin}"
        ));
    }
    Ok(arcs)
}

// ---- arc derivation straight from a reference Liberty -------------------

/// Extract a brace-balanced `keyword (name) { ... }` block whose paren-name
/// (quotes stripped) equals `want`; or any such block when `want` is empty.
fn block_after<'a>(lines: &'a [&'a str], start: usize, kw: &str) -> Option<(String, usize, usize)> {
    let kw_sp = format!("{kw} (");
    let kw_np = format!("{kw}(");
    let mut i = start;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with(&kw_sp) || t.starts_with(&kw_np) {
            let name = t
                .split('(')
                .nth(1)
                .and_then(|r| r.split(')').next())
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .to_string();
            let mut depth = 0i32;
            let mut started = false;
            let mut j = i;
            while j < lines.len() {
                for c in lines[j].chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            started = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                j += 1;
                if started && depth <= 0 {
                    break;
                }
            }
            return Some((name, i, j));
        }
        i += 1;
    }
    None
}

/// Whether a pin block declares `direction : output` (robust to inline/own-line).
fn pin_is_output(lines: &[&str]) -> bool {
    let joined = lines.join(" ");
    for seg in joined.split(';') {
        if let Some((k, v)) = seg.split_once(':') {
            // last word, so an inline "pin (A) { direction" still matches
            if k.split_whitespace().last() == Some("direction") {
                return v.contains("output");
            }
        }
    }
    false
}

/// The `function : "..."` value inside a pin block, if any. Robust to inline or
/// own-line attributes, and excludes `power_down_function` (key must be exactly
/// `function`).
fn pin_function(lines: &[&str]) -> Option<String> {
    let joined = lines.join(" ");
    for seg in joined.split(';') {
        if let Some((k, v)) = seg.split_once(':') {
            // exact last word "function" (so power_down_function is excluded)
            if k.split_whitespace().last() == Some("function") {
                let q = v.find('"')?;
                let rest = &v[q + 1..];
                let end = rest.find('"')?;
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

/// Derive all of a cell's combinational arcs straight from a **reference
/// Liberty**: find the cell, then for each output pin with a Boolean `function`,
/// derive its arcs. This is what makes `char library` netlist-driven — no
/// hand-authored `arc:`/`function:` lines, just a PDK `.lib` to read structure
/// from (char still measures the values in SPICE).
pub fn arcs_from_lib(lib_text: &str, cell: &str) -> Result<Vec<ArcSpec>, String> {
    let lines: Vec<&str> = lib_text.lines().collect();
    // locate the cell block
    let mut i = 0;
    let (cs, ce) = loop {
        let (name, s, e) = block_after(&lines, i, "cell")
            .ok_or_else(|| format!("cell {cell:?} not found in reference lib"))?;
        if name == cell {
            break (s, e);
        }
        i = e;
    };
    let cell_lines = &lines[cs..ce];
    // each pin block; keep output pins that carry a combinational function
    let mut arcs = Vec::new();
    let mut p = 0;
    while let Some((pin, ps, pe)) = block_after(cell_lines, p, "pin") {
        let pin_lines = &cell_lines[ps..pe];
        if pin_is_output(pin_lines) {
            if let Some(func) = pin_function(pin_lines) {
                arcs.extend(derive_arcs(&pin, &func)?);
            }
        }
        p = pe;
    }
    if arcs.is_empty() {
        return Err(format!(
            "no combinational output function found for {cell} in reference lib"
        ));
    }
    Ok(arcs)
}

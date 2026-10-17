// SPDX-License-Identifier: Apache-2.0
//! Every message this tool prints about a run, through one path: a `vyges-events/1.0` event with
//! its code (text on a terminal, JSONL when piped) with the opt-in `events` feature — else (the
//! default, std-only build) the same line on stderr.

/// How much a message matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// One message, with its code (`CHAR-...`).
pub fn emit(level: Level, code: &str, msg: impl Into<String>) {
    let msg = msg.into();
    #[cfg(feature = "events")]
    {
        use vyges_events::{Event, Severity};
        let sev = match level {
            Level::Info => Severity::Info,
            Level::Warn => Severity::Warn,
            Level::Error => Severity::Error,
        };
        vyges_events::emit(&Event::new("vyges-char", sev, msg).with_code(code));
    }
    #[cfg(not(feature = "events"))]
    {
        let _ = code;
        match level {
            Level::Info => eprintln!("{msg}"),
            Level::Warn => eprintln!("warning: {msg}"),
            Level::Error => eprintln!("error: {msg}"),
        }
    }
}

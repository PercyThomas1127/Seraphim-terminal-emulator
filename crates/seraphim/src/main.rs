//! Seraphim Phase I headless driver.
//!
//! Until the GPU renderer lands, this binary exercises the Terminal Core
//! end-to-end:
//!
//! ```text
//! seraphim run [--cols N] [--rows N] [--timeout SECS] [--events] -- PROGRAM [ARGS...]
//!     Run PROGRAM in a PTY-backed session, then print the final screen.
//!     --events also prints every event published on the bus.
//!
//! seraphim replay [--cols N] [--rows N] FILE
//!     Feed a recorded byte stream (e.g. from `script -q -c CMD out.log`)
//!     through the parser and print the resulting screen. Useful for
//!     reproducing rendering bugs and building compatibility fixtures.
//! ```

use std::process::ExitCode;
use std::time::Duration;

use seraphim_events::EventBus;
use seraphim_session::{Command, Session, SessionConfig};
use seraphim_terminal::Terminal;

struct Opts {
    cols: u16,
    rows: u16,
    timeout: u64,
    events: bool,
    rest: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { cols: 80, rows: 24, timeout: 30, events: false, rest: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |name: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{name} needs a numeric value"))
        };
        match a.as_str() {
            "--cols" => o.cols = num("--cols")?.clamp(1, 1000) as u16,
            "--rows" => o.rows = num("--rows")?.clamp(1, 1000) as u16,
            "--timeout" => o.timeout = num("--timeout")?,
            "--events" => o.events = true,
            "--" => {
                o.rest.extend(it.by_ref().cloned());
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.rest.push(a.clone()),
        }
    }
    Ok(o)
}

fn print_screen(term: &Terminal) {
    let width = term.cols();
    println!("┌{}┐", "─".repeat(width));
    for row in term.screen().rows_iter() {
        // Every cell occupies one column; a wide glyph's spacer cell emits
        // nothing, so the line is exactly `width` columns.
        let mut line = String::new();
        for cell in &row.cells {
            cell.push_text(&mut line);
        }
        println!("│{line}│");
    }
    println!("└{}┘", "─".repeat(width));
    let c = term.cursor();
    println!(
        "cursor {},{}  title {:?}  cwd {:?}  alt-screen {}",
        c.row,
        c.col,
        term.title(),
        term.cwd(),
        term.is_alternate_screen()
    );
}

fn run(o: Opts) -> Result<ExitCode, String> {
    let (program, args) =
        o.rest.split_first().ok_or("run: missing PROGRAM (use `-- PROGRAM ARGS`)")?;
    let bus = EventBus::new();
    let events = bus.subscribe("*", 100_000);
    let config = SessionConfig {
        command: Command::new(program).args(args),
        cols: o.cols,
        rows: o.rows,
        ..Default::default()
    };
    let session = Session::spawn(1, config, &bus).map_err(|e| format!("spawn {program}: {e}"))?;
    let exit = session.wait_exit(Duration::from_secs(o.timeout));
    print_screen(&session.terminal());
    if o.events {
        for e in events.drain().iter().filter(|e| e.name != "terminal.output") {
            println!("event {:<24} session={:?} {}", e.name, e.session, e.data);
        }
        if events.dropped() > 0 {
            println!("({} events dropped)", events.dropped());
        }
    }
    match exit {
        Some(info) => {
            println!("exited code={:?} signal={:?}", info.code, info.signal);
            Ok(ExitCode::from(info.code.unwrap_or(1).clamp(0, 255) as u8))
        }
        None => Err(format!("timed out after {}s", o.timeout)),
    }
}

fn replay(o: Opts) -> Result<ExitCode, String> {
    let path = o.rest.first().ok_or("replay: missing FILE")?;
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut term = Terminal::new(o.cols as usize, o.rows as usize);
    term.advance(&bytes);
    print_screen(&term);
    for e in term.take_events() {
        println!("event {e:?}");
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: seraphim run [--cols N] [--rows N] [--timeout S] [--events] -- PROGRAM [ARGS...]\n       seraphim replay [--cols N] [--rows N] FILE";
    let result = match args.first().map(String::as_str) {
        Some("run") => parse(&args[1..]).and_then(run),
        Some("replay") => parse(&args[1..]).and_then(replay),
        Some("--version") => {
            println!("seraphim {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("seraphim: {e}");
        ExitCode::from(2)
    })
}

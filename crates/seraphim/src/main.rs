//! Seraphim terminal.
//!
//! ```text
//! seraphim [--config PATH] [--profile NAME] [--preset NAME] [--font FAMILY]
//!          [--font-size PT] [--cols N] [--rows N] [--extension PATH]...
//!          [-e PROGRAM [ARGS...]]
//!     Open a window running PROGRAM (default: your shell). Settings come from
//!     ~/.config/seraphim/config.toml, then these options. --preset picks a
//!     background preset (built-in "heavens", presets/NAME.toml, or "none").
//!     --extension also loads an extension folder or .lua file (with the
//!     grants of its [extensions.NAME] entry). --layout opens a [[layout]].
//!
//! seraphim bench [--json] [--frames N]
//!     Measure parser throughput, frame preparation and GPU frame times on
//!     this machine (no window needed).
//!
//! seraphim --print-default-config
//!     Print the documented default configuration file.
//!
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

mod actions;
mod bench;
mod bindings;
mod interaction;
mod menu;
mod panel;
mod panels;
mod profile;
mod restore;
mod scripting;
mod settings;
mod tabbar;
mod watch;
mod window;
mod workspace;

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
        for col in 0..row.cells.len() {
            row.push_cell_text(col, &mut line);
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

/// Window options from the config file, then command-line overrides.
fn parse_window(args: &[String]) -> Result<window::WindowOptions, String> {
    // --config and --profile must be known before anything else applies.
    let mut config_path = seraphim_config::Config::default_path();
    let mut profile: Option<String> = None;
    let mut preset: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config_path = Some(it.next().ok_or("--config needs a PATH")?.into()),
            "--profile" => profile = Some(it.next().ok_or("--profile needs a NAME")?.clone()),
            "--preset" => preset = Some(it.next().ok_or("--preset needs a NAME")?.clone()),
            "-e" | "--" => break,
            _ => {}
        }
    }
    let mut o = match profile::load(config_path.as_deref(), profile.as_deref(), preset.as_deref()) {
        Ok((mut o, warnings)) => {
            for w in &warnings {
                eprintln!("seraphim: {w}");
            }
            o.notice = warnings.into_iter().next();
            o
        }
        // A broken config must not leave you without a terminal: report
        // it and carry on with the defaults.
        Err(e) => {
            eprintln!("seraphim: ignoring config: {e}");
            let mut o =
                profile::options_from_config(&seraphim_config::Config::default(), None, "default")
                    .0;
            o.notice = Some(format!("Configuration ignored: {e}"));
            // Keep watching the broken file: fixing it applies the fix.
            o.watch = config_path.iter().cloned().collect();
            o
        }
    };
    o.config_path = config_path;
    o.cli_preset = preset;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value =
            |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--config" => {
                value("--config")?;
            }
            "--profile" => {
                value("--profile")?;
            }
            "--preset" => {
                value("--preset")?;
            }
            "--font" => {
                o.font_family = Some(value("--font")?);
                o.cli_font_family = o.font_family.clone();
            }
            "--font-size" => {
                o.font_size = value("--font-size")?
                    .parse::<f32>()
                    .map_err(|_| "--font-size needs a number")?
                    .clamp(4.0, 200.0);
                o.cli_font_size = Some(o.font_size);
            }
            "--cols" => o.cols = value("--cols")?.parse().map_err(|_| "--cols needs a number")?,
            "--rows" => o.rows = value("--rows")?.parse().map_err(|_| "--rows needs a number")?,
            "--allow-clipboard-read" => o.allow_clipboard_read = true,
            "--layout" => o.cli_layout = Some(value("--layout")?),
            "--extension" => {
                let p = std::path::PathBuf::from(value("--extension")?);
                o.cli_extensions.push(p.canonicalize().unwrap_or(p));
            }
            "-e" | "--" => {
                let rest: Vec<&String> = it.by_ref().collect();
                let (prog, args) = rest.split_first().ok_or("-e needs a PROGRAM")?;
                o.command = Some(Command::new(prog.as_str()).args(args.iter().map(|s| s.as_str())));
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    o.cols = o.cols.clamp(10, 1000);
    o.rows = o.rows.clamp(3, 1000);
    Ok(o)
}

/// Compile Seraphim's terminfo entry into ~/.terminfo with `tic`.
fn install_terminfo() -> Result<(), String> {
    const SOURCE: &str = include_str!("../../../extra/terminfo/xterm-seraphim.terminfo");
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let out = std::path::PathBuf::from(home).join(".terminfo");
    let src = std::env::temp_dir().join(format!("xterm-seraphim-{}.terminfo", std::process::id()));
    std::fs::write(&src, SOURCE).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("tic")
        .arg("-x")
        .arg("-o")
        .arg(&out)
        .arg(&src)
        .status()
        .map_err(|e| format!("tic (from ncurses) is needed: {e}"));
    let _ = std::fs::remove_file(&src);
    match status? {
        s if s.success() => {
            println!(
                "Installed xterm-seraphim into {}; new sessions use TERM=xterm-seraphim.",
                out.display()
            );
            Ok(())
        }
        s => Err(format!("tic failed ({s})")),
    }
}

fn main() -> ExitCode {
    let _ = window::STARTED.set(std::time::Instant::now());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: seraphim [--config PATH] [--profile NAME] [--preset NAME] [--font FAMILY] [--font-size PT] [--cols N] [--rows N] [--allow-clipboard-read] [--extension PATH]... [--layout NAME] [-e PROGRAM [ARGS...]]\n       seraphim --print-default-config\n       seraphim --install-terminfo\n       seraphim bench [--json] [--frames N]\n       seraphim run [--cols N] [--rows N] [--timeout S] [--events] -- PROGRAM [ARGS...]\n       seraphim replay [--cols N] [--rows N] FILE";
    let result = match args.first().map(String::as_str) {
        Some("run") => parse(&args[1..]).and_then(run),
        Some("replay") => parse(&args[1..]).and_then(replay),
        Some("--version") => {
            println!("seraphim {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("bench") => {
            let json = args.iter().any(|a| a == "--json");
            let frames = args
                .iter()
                .position(|a| a == "--frames")
                .and_then(|i| args.get(i + 1)?.parse().ok())
                .unwrap_or(120);
            bench::run(json, frames).map(|_| ExitCode::SUCCESS)
        }
        Some("--install-terminfo") => install_terminfo().map(|_| ExitCode::SUCCESS),
        Some("--print-default-config") => {
            print!("{}", seraphim_config::DEFAULT_TOML);
            Ok(ExitCode::SUCCESS)
        }
        Some("-h") | Some("--help") => {
            println!("{usage}");
            Ok(ExitCode::SUCCESS)
        }
        _ => parse_window(&args)
            .and_then(window::run)
            .map(|code| ExitCode::from(code.clamp(0, 255) as u8)),
    };
    result.unwrap_or_else(|e| {
        eprintln!("seraphim: {e}");
        ExitCode::from(2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_maps_onto_window_options() {
        let c = seraphim_config::Config::parse(
            "[font]\nsize = 13.5\n[window]\npadding = 0\n[shell]\nprogram = \"/bin/zsh\"\nargs = [\"-l\"]\n[colors]\nnormal = [\"#000001\", \"#000002\", \"#000003\", \"#000004\", \"#000005\", \"#000006\", \"#000007\", \"#000008\"]\n",
            std::path::Path::new("t.toml"),
        )
        .unwrap();
        let o = profile::options_from_config(&c, None, "default").0;
        assert_eq!(o.font_size, 13.5);
        assert_eq!(o.padding, 0.0);
        assert_eq!(o.theme.ansi[0], [0, 0, 1]);
        assert_eq!(o.theme.ansi[7], [0, 0, 8]);
        assert_eq!(o.theme.ansi[8], seraphim_config::Colors::default().bright[0]);
        let cmd = o.command.unwrap();
        assert_eq!(cmd.program, "/bin/zsh");
        assert_eq!(cmd.args, vec![std::ffi::OsString::from("-l")]);
    }

    #[test]
    fn default_config_equals_default_theme() {
        let o =
            profile::options_from_config(&seraphim_config::Config::default(), None, "default").0;
        assert_eq!(
            o.theme,
            seraphim_render::Theme::default(),
            "config defaults and renderer defaults agree"
        );
    }

    #[test]
    fn cli_overrides_config() {
        let dir = std::env::temp_dir().join(format!("seraphim-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[font]\nsize = 20.0\n[window]\ncolumns = 50\n").unwrap();
        let args: Vec<String> = ["--config", path.to_str().unwrap(), "--font-size", "9"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse_window(&args).unwrap();
        assert_eq!(o.font_size, 9.0, "CLI wins");
        assert_eq!(o.cols, 50, "config applies where CLI is silent");
    }
}

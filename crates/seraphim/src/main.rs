//! Seraphim terminal.
//!
//! ```text
//! seraphim [--config PATH] [--font FAMILY] [--font-size PT] [--cols N] [--rows N] [-e PROGRAM [ARGS...]]
//!     Open a window running PROGRAM (default: your shell). Settings come from
//!     ~/.config/seraphim/config.toml, then these options.
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
mod window;

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
    // --config must be known before anything else is applied.
    let mut config_path = seraphim_config::Config::default_path();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--config" {
            config_path = Some(it.next().ok_or("--config needs a PATH")?.into());
        } else if a == "-e" || a == "--" {
            break;
        }
    }
    let config = match &config_path {
        Some(p) => match seraphim_config::Config::load(p) {
            Ok(c) => c.unwrap_or_default(),
            // A broken config must not leave you without a terminal: report
            // it and carry on with the defaults.
            Err(e) => {
                eprintln!("seraphim: ignoring config: {e}");
                seraphim_config::Config::default()
            }
        },
        None => seraphim_config::Config::default(),
    };
    let mut o = options_from_config(&config);

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value =
            |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--config" => {
                value("--config")?;
            }
            "--font" => o.font_family = Some(value("--font")?),
            "--font-size" => {
                o.font_size = value("--font-size")?
                    .parse::<f32>()
                    .map_err(|_| "--font-size needs a number")?
                    .clamp(4.0, 200.0)
            }
            "--cols" => o.cols = value("--cols")?.parse().map_err(|_| "--cols needs a number")?,
            "--rows" => o.rows = value("--rows")?.parse().map_err(|_| "--rows needs a number")?,
            "--allow-clipboard-read" => o.allow_clipboard_read = true,
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

fn options_from_config(c: &seraphim_config::Config) -> window::WindowOptions {
    let mut theme = seraphim_render::Theme::default();
    let k = &c.colors;
    theme.foreground = k.foreground;
    theme.background = k.background;
    theme.cursor = k.cursor;
    theme.cursor_text = k.cursor_text;
    theme.selection_bg = k.selection_background;
    theme.selection_fg = k.selection_foreground;
    theme.bold_is_bright = k.bold_is_bright;
    theme.ansi[..8].copy_from_slice(&k.normal);
    theme.ansi[8..].copy_from_slice(&k.bright);
    window::WindowOptions {
        font_family: c.font.family.clone(),
        font_size: c.font.size,
        cols: c.window.columns,
        rows: c.window.rows,
        padding: c.window.padding,
        command: c
            .shell
            .program
            .as_ref()
            .map(|p| Command::new(p.as_str()).args(c.shell.args.iter().map(|s| s.as_str()))),
        allow_clipboard_read: c.clipboard.allow_read,
        theme,
        scrollback: c.terminal.scrollback,
        grapheme_clustering: c.terminal.grapheme_clustering,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: seraphim [--config PATH] [--font FAMILY] [--font-size PT] [--cols N] [--rows N] [--allow-clipboard-read] [-e PROGRAM [ARGS...]]\n       seraphim --print-default-config\n       seraphim run [--cols N] [--rows N] [--timeout S] [--events] -- PROGRAM [ARGS...]\n       seraphim replay [--cols N] [--rows N] FILE";
    let result = match args.first().map(String::as_str) {
        Some("run") => parse(&args[1..]).and_then(run),
        Some("replay") => parse(&args[1..]).and_then(replay),
        Some("--version") => {
            println!("seraphim {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
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
        let o = options_from_config(&c);
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
        let o = options_from_config(&seraphim_config::Config::default());
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

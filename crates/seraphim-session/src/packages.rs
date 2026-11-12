//! Package manager activity, recognized from what runs in the terminal.
//!
//! No package manager tells a terminal what it is doing, so Seraphim
//! recognizes their command lines (from shell integration, or the
//! foreground process) and follows their progress in their own output.
//! Each manager is described by how its commands read and what its
//! progress lines look like. Pure: strings in, facts out.

use std::time::Instant;

/// What a command does to the system's packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Install,
    Remove,
    /// Upgrade the whole system (or the named packages).
    Upgrade,
    /// Refresh the package lists.
    Sync,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Install => "install",
            Op::Remove => "remove",
            Op::Upgrade => "upgrade",
            Op::Sync => "sync",
        }
    }
}

/// A recognized package manager command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgCommand {
    /// The manager's usual name: `portage`, `pacman`, `apt`, `dnf`, ….
    pub manager: &'static str,
    pub op: Op,
    /// Packages named on the command line (sets like `@world` too).
    pub packages: Vec<String>,
}

/// Split a command line into words, honoring quotes and backslashes.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The simple commands of a line (`a && b; c | d` → a, b, c, d).
fn commands(line: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    for w in words(line) {
        if matches!(w.as_str(), "&&" | "||" | ";" | "|" | "&") {
            out.push(Vec::new());
            continue;
        }
        // `a;b` or `a&&` stuck to a word.
        let mut w = w.as_str();
        let mut end = false;
        for sep in ["&&", "||", ";"] {
            if let Some(s) = w.strip_suffix(sep) {
                w = s;
                end = true;
            }
        }
        if !w.is_empty() {
            out.last_mut().unwrap().push(w.to_owned());
        }
        if end {
            out.push(Vec::new());
        }
    }
    out.retain(|c| !c.is_empty());
    out
}

/// Skip what runs a command on another's behalf (`sudo -E`, `doas`,
/// `env A=b`, `nice -n 5`, …) and variable assignments; return the rest.
fn unwrap(mut argv: &[String]) -> &[String] {
    loop {
        let Some(first) = argv.first() else { return argv };
        let name = first.rsplit('/').next().unwrap_or(first);
        // Options of each wrapper that take a value.
        let valued: &[&str] = match name {
            "sudo" => &["-u", "-g", "-p", "-C", "-D", "-R", "-T", "-U", "-h", "-r", "-t"],
            "doas" => &["-u", "-C"],
            "run0" => &["-u", "-g", "-D", "--user", "--group", "--chdir", "--nice"],
            "pkexec" => &["--user", "-u"],
            "env" => &["-u", "-C", "-S"],
            "nice" => &["-n"],
            "ionice" => &["-c", "-n", "-p"],
            "stdbuf" => &["-i", "-o", "-e"],
            "time" | "command" | "nohup" | "exec" | "builtin" => &[],
            _ if first.contains('=') && !first.starts_with('-') => {
                // VAR=value before the command.
                argv = &argv[1..];
                continue;
            }
            _ => return argv,
        };
        argv = &argv[1..];
        while let Some(a) = argv.first() {
            if a == "--" {
                argv = &argv[1..];
                break;
            }
            if !a.starts_with('-') && !(name == "env" && a.contains('=')) {
                break;
            }
            argv = &argv[1..];
            if valued.contains(&a.as_str()) {
                argv = argv.get(1..).unwrap_or(&[]);
            }
        }
    }
}

fn positional(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| !a.starts_with('-')).cloned().collect()
}

/// The first package manager command in a shell command line.
pub fn parse(line: &str) -> Option<PkgCommand> {
    commands(line).iter().find_map(|argv| parse_argv(unwrap(argv)))
}

/// A package manager command from its argument vector.
pub fn parse_argv(argv: &[String]) -> Option<PkgCommand> {
    let argv = unwrap(argv);
    let prog = argv.first()?;
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    // Interpreters run the manager as a script (`python3 /usr/bin/emerge`,
    // `sh -c "apt install x"`, `/bin/sh ./pacman-wrapper`).
    let interpreter = prog.starts_with("python")
        || matches!(prog, "sh" | "bash" | "dash" | "zsh" | "perl" | "ruby");
    if interpreter {
        let mut rest = &argv[1..];
        while let Some(a) = rest.first().filter(|a| a.starts_with('-')) {
            if a == "-c" {
                return rest.get(1).and_then(|line| parse(line));
            }
            rest = &rest[1..];
        }
        return if rest.is_empty() { None } else { parse_argv(rest) };
    }
    let args = &argv[1..];
    let cmd = |manager, op, packages| Some(PkgCommand { manager, op, packages });
    match prog {
        "emerge" => emerge(args),
        "emaint" if args.iter().any(|a| a == "sync") => cmd("portage", Op::Sync, vec![]),
        "eix-sync" => cmd("portage", Op::Sync, vec![]),
        "pacman" | "paru" | "yay" | "pikaur" => {
            let m = match prog {
                "pacman" => "pacman",
                "paru" => "paru",
                "yay" => "yay",
                _ => "pikaur",
            };
            pacman(m, args)
        }
        "apt" | "apt-get" | "aptitude" | "nala" => {
            let m = if prog == "apt-get" { "apt" } else { prog_static(prog) };
            subcommand(m, args, &["-o", "-c", "-t", "--target-release"], |s| match s {
                "install" | "reinstall" => Some(Op::Install),
                "remove" | "purge" | "autoremove" | "autopurge" => Some(Op::Remove),
                "upgrade" | "full-upgrade" | "dist-upgrade" | "safe-upgrade" => Some(Op::Upgrade),
                "update" => Some(Op::Sync),
                _ => None,
            })
        }
        "dnf" | "dnf5" | "yum" | "microdnf" => {
            let m = if prog == "yum" { "yum" } else { "dnf" };
            let mut c = subcommand(
                m,
                args,
                &[
                    "--setopt",
                    "-c",
                    "--config",
                    "--releasever",
                    "-x",
                    "--exclude",
                    "--repo",
                    "--enablerepo",
                    "--disablerepo",
                ],
                |s| match s {
                    "install" | "reinstall" | "in" | "groupinstall" | "localinstall" => {
                        Some(Op::Install)
                    }
                    "remove" | "erase" | "rm" | "autoremove" | "groupremove" => Some(Op::Remove),
                    "upgrade" | "update" | "up" | "distro-sync" | "dsync" | "upgrade-minimal" => {
                        Some(Op::Upgrade)
                    }
                    "makecache" | "check-update" => Some(Op::Sync),
                    _ => None,
                },
            );
            // `dnf group install X`.
            if c.is_none() {
                let p = positional(args);
                if p.first().is_some_and(|g| g == "group" || g == "groups") {
                    let op = match p.get(1).map(String::as_str) {
                        Some("install") => Op::Install,
                        Some("remove") => Op::Remove,
                        Some("upgrade") => Op::Upgrade,
                        _ => return None,
                    };
                    c = cmd(m, op, p[2..].to_vec());
                }
            }
            c
        }
        "zypper" => subcommand("zypper", args, &["-r", "--repo", "--root", "-R"], |s| match s {
            "install" | "in" | "source-install" | "si" => Some(Op::Install),
            "remove" | "rm" => Some(Op::Remove),
            "update" | "up" | "dist-upgrade" | "dup" | "patch" => Some(Op::Upgrade),
            "refresh" | "ref" => Some(Op::Sync),
            _ => None,
        }),
        "xbps-install" => {
            let flags: String = args
                .iter()
                .filter(|a| a.starts_with('-') && !a.starts_with("--"))
                .flat_map(|a| a.chars().skip(1))
                .collect();
            let long = |l: &str| args.iter().any(|a| a == l);
            let pkgs = positional(args);
            let op = if flags.contains('u') || long("--update") {
                Op::Upgrade
            } else if !pkgs.is_empty() {
                Op::Install
            } else if flags.contains('S') || long("--sync") {
                Op::Sync
            } else {
                return None;
            };
            cmd("xbps", op, pkgs)
        }
        "xbps-remove" => {
            let pkgs = positional(args);
            let orphans = args.iter().any(|a| a == "-o" || a == "--remove-orphans");
            (!pkgs.is_empty() || orphans).then_some(PkgCommand {
                manager: "xbps",
                op: Op::Remove,
                packages: pkgs,
            })
        }
        "apk" => subcommand("apk", args, &["-X", "--repository", "-p", "--root"], |s| match s {
            "add" => Some(Op::Install),
            "del" => Some(Op::Remove),
            "upgrade" => Some(Op::Upgrade),
            "update" => Some(Op::Sync),
            _ => None,
        }),
        "eopkg" => subcommand("eopkg", args, &[], |s| match s {
            "install" | "it" => Some(Op::Install),
            "remove" | "rm" => Some(Op::Remove),
            "upgrade" | "up" => Some(Op::Upgrade),
            "update-repo" | "ur" => Some(Op::Sync),
            _ => None,
        }),
        "nix-env" => {
            let has = |s: &[&str]| args.iter().any(|a| s.contains(&a.as_str()));
            let op = if has(&["-i", "--install", "-iA"]) {
                Op::Install
            } else if has(&["-e", "--uninstall"]) {
                Op::Remove
            } else if has(&["-u", "--upgrade"]) {
                Op::Upgrade
            } else {
                return None;
            };
            cmd("nix", op, positional(args))
        }
        "nix-channel" if args.iter().any(|a| a == "--update") => cmd("nix", Op::Sync, vec![]),
        "nix" => {
            let p = positional(args);
            if p.first().map(String::as_str) != Some("profile") {
                return None;
            }
            let op = match p.get(1).map(String::as_str) {
                Some("install" | "add") => Op::Install,
                Some("remove") => Op::Remove,
                Some("upgrade") => Op::Upgrade,
                _ => return None,
            };
            cmd("nix", op, p[2..].to_vec())
        }
        "flatpak" => subcommand("flatpak", args, &[], |s| match s {
            "install" => Some(Op::Install),
            "uninstall" | "remove" => Some(Op::Remove),
            "update" | "upgrade" => Some(Op::Upgrade),
            _ => None,
        }),
        "snap" => subcommand("snap", args, &[], |s| match s {
            "install" => Some(Op::Install),
            "remove" => Some(Op::Remove),
            "refresh" => Some(Op::Upgrade),
            _ => None,
        }),
        "brew" => subcommand("brew", args, &[], |s| match s {
            "install" | "reinstall" => Some(Op::Install),
            "uninstall" | "remove" | "rm" => Some(Op::Remove),
            "upgrade" => Some(Op::Upgrade),
            "update" => Some(Op::Sync),
            _ => None,
        }),
        _ => None,
    }
}

fn prog_static(p: &str) -> &'static str {
    match p {
        "apt" => "apt",
        "aptitude" => "aptitude",
        "nala" => "nala",
        _ => "apt",
    }
}

/// `manager [options] subcommand [packages]`. `valued` are the options
/// that take a separate value.
fn subcommand(
    manager: &'static str,
    args: &[String],
    valued: &[&str],
    op: impl Fn(&str) -> Option<Op>,
) -> Option<PkgCommand> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') {
            i += if valued.contains(&a.as_str()) { 2 } else { 1 };
            continue;
        }
        let op = op(a)?;
        let mut packages = Vec::new();
        let mut j = i + 1;
        while j < args.len() {
            let b = &args[j];
            if b.starts_with('-') {
                j += if valued.contains(&b.as_str()) { 2 } else { 1 };
                continue;
            }
            packages.push(b.clone());
            j += 1;
        }
        return Some(PkgCommand { manager, op, packages });
    }
    None
}

fn emerge(args: &[String]) -> Option<PkgCommand> {
    let mut short = String::new();
    let mut long = Vec::new();
    let mut pkgs = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if let Some(l) = a.strip_prefix("--") {
            long.push(l.split('=').next().unwrap_or(l).to_owned());
        } else if let Some(s) = a.strip_prefix('-') {
            short.push_str(s);
            // -j N, -l N, -k: some take a value.
            if s.ends_with('j') || s.ends_with('l') {
                skip = true;
            }
        } else if a.parse::<f64>().is_err() {
            pkgs.push(a.clone());
        }
    }
    let l = |s: &str| long.iter().any(|x| x == s);
    let s = |c: char| short.contains(c);
    // Looking, not doing.
    if s('p')
        || s('s')
        || s('S')
        || s('h')
        || l("pretend")
        || l("search")
        || l("searchdesc")
        || l("info")
        || l("help")
        || l("version")
        || l("fetchonly")
        || s('f')
    {
        return None;
    }
    let cmd = |op, packages| Some(PkgCommand { manager: "portage", op, packages });
    if l("sync") {
        return cmd(Op::Sync, vec![]);
    }
    if s('C') || s('c') || s('P') || l("unmerge") || l("depclean") || l("prune") || l("rage-clean")
    {
        return cmd(Op::Remove, pkgs);
    }
    let sets = pkgs.iter().any(|p| p == "@world" || p == "@system" || p == "world");
    if s('u') || l("update") || sets {
        return cmd(Op::Upgrade, pkgs);
    }
    if pkgs.is_empty() {
        return None;
    }
    cmd(Op::Install, pkgs)
}

fn pacman(manager: &'static str, args: &[String]) -> Option<PkgCommand> {
    let mut flags = String::new();
    let mut long = Vec::new();
    for a in args {
        if let Some(l) = a.strip_prefix("--") {
            long.push(l.to_owned());
        } else if let Some(s) = a.strip_prefix('-') {
            flags.push_str(s);
        }
    }
    let pkgs = positional(args);
    let cmd = |op, packages| Some(PkgCommand { manager, op, packages });
    let l = |s: &str| long.iter().any(|x| x == s);
    let f = |c: char| flags.contains(c);
    // `paru`, `yay` alone upgrade the system.
    if args.is_empty() && manager != "pacman" {
        return cmd(Op::Upgrade, vec![]);
    }
    if f('S') || l("sync") {
        // -Ss search, -Si info, -Sl list, -Sg groups, -Sc clean: no change.
        if f('s') || f('i') || f('l') || f('g') || f('c') || f('p') {
            return None;
        }
        if f('u') || l("sysupgrade") {
            return cmd(Op::Upgrade, pkgs);
        }
        if pkgs.is_empty() {
            return f('y').then(|| PkgCommand { manager, op: Op::Sync, packages: vec![] });
        }
        return cmd(Op::Install, pkgs);
    }
    if f('R') || l("remove") {
        return (!f('p')).then_some(PkgCommand { manager, op: Op::Remove, packages: pkgs });
    }
    if f('U') || l("upgrade") {
        return cmd(Op::Install, pkgs);
    }
    // `yay foo`: search and install.
    if manager != "pacman" && flags.is_empty() && long.is_empty() && !pkgs.is_empty() {
        return cmd(Op::Install, pkgs);
    }
    None
}

/// What a line of output says about progress.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// The package being worked on.
    pub package: Option<String>,
    /// Packages done so far, and of how many.
    pub done: Option<usize>,
    pub total: Option<usize>,
    /// An explicit percentage (apt's progress bar, flatpak).
    pub percent: Option<u8>,
}

/// `(3/10)`, `[3/10]`, `3/10` at the start of `s`, with what follows.
fn fraction(s: &str) -> Option<(usize, usize, &str)> {
    let s = s.trim_start();
    let (open, close) = match s.chars().next()? {
        '(' => (1, ')'),
        '[' => (1, ']'),
        _ => (0, ' '),
    };
    let body = &s[open..];
    let end = if open == 1 { body.find(close)? } else { body.find(' ').unwrap_or(body.len()) };
    let (a, b) = body[..end].split_once('/')?;
    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b > 0 && a <= b).then_some((a, b, body[end..].trim_start_matches([')', ']']).trim_start()))
}

/// `(1 of 5)`.
fn of(s: &str) -> Option<(usize, usize, &str)> {
    let s = s.trim_start().strip_prefix('(')?;
    let end = s.find(')')?;
    let (a, b) = s[..end].split_once(" of ")?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, s[end + 1..].trim_start()))
}

fn first_word(s: &str) -> Option<String> {
    let w = s.split_whitespace().next()?;
    let w = w.trim_end_matches([':', ',', '.', '…']);
    (!w.is_empty()).then(|| w.to_owned())
}

/// A percentage anywhere in the line (`45%`).
fn percent_in(s: &str) -> Option<u8> {
    let i = s.find('%')?;
    let digits: String = s[..i]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse::<u32>().ok().filter(|p| *p <= 100).map(|p| p as u8)
}

/// Read one line of a manager's output.
pub fn step(manager: &str, line: &str) -> Option<Step> {
    let line = line.trim_end();
    let t = line.trim_start();
    let verbs = |rest: &str, list: &[&str]| -> Option<String> {
        let rest = rest.trim_start();
        let (verb, after) = rest.split_once(char::is_whitespace)?;
        let verb = verb.trim_end_matches(':');
        list.iter().any(|v| v.eq_ignore_ascii_case(verb)).then(|| first_word(after)).flatten()
    };
    const CHANGE: &[&str] = &[
        "installing",
        "upgrading",
        "reinstalling",
        "removing",
        "downgrading",
        "purging",
        "updating",
        "uninstalling",
        "erasing",
    ];
    match manager {
        "pacman" | "paru" | "yay" | "pikaur" | "zypper" | "apk" => {
            // pacman `(3/10) installing foo`; zypper `(3/10) Installing: foo-1.0`;
            // apk `(3/10) Installing foo (1.0-r0)`.
            let (done, total, rest) = fraction(t)?;
            let package = verbs(rest, CHANGE)?;
            Some(Step {
                package: Some(package),
                done: Some(done),
                total: Some(total),
                percent: None,
            })
        }
        "portage" => {
            // `>>> Emerging (1 of 5) dev-libs/foo-1.0::gentoo`,
            // `>>> Completed (1 of 5) …`, `>>> Unmerging (1 of 2) …`.
            let rest = t.strip_prefix(">>> ")?;
            let (verb, rest) = rest.split_once(' ')?;
            let (n, total, pkg) = of(rest)?;
            let package = first_word(pkg).map(|p| p.split("::").next().unwrap_or(&p).to_owned());
            match verb {
                "Emerging" | "Unmerging" => {
                    Some(Step { package, done: Some(n - 1), total: Some(total), percent: None })
                }
                "Completed" => {
                    Some(Step { package, done: Some(n), total: Some(total), percent: None })
                }
                _ => None,
            }
        }
        "apt" | "aptitude" | "nala" => {
            // Dpkg::Progress-Fancy: `Progress: [ 45%]`.
            if let Some(rest) = t.strip_prefix("Progress: [") {
                return Some(Step { percent: percent_in(rest), ..Default::default() });
            }
            for verb in ["Unpacking", "Setting up", "Removing", "Purging configuration files for"] {
                if let Some(rest) = t.strip_prefix(verb).and_then(|r| r.strip_prefix(' ')) {
                    let package =
                        first_word(rest).map(|p| p.split(':').next().unwrap_or(&p).to_owned());
                    return Some(Step { package, ..Default::default() });
                }
            }
            None
        }
        "dnf" | "yum" => {
            // dnf4 `  Installing   : foo-1.0-1.fc39.x86_64     3/10`;
            // dnf5 `[3/10] Installing foo-0:1.0-1.fc40.x86_64  100% | …`.
            if let Some((done, total, rest)) = fraction(t).filter(|_| t.starts_with('[')) {
                let package = verbs(rest, CHANGE)?;
                return Some(Step {
                    package: Some(package),
                    done: Some(done),
                    total: Some(total),
                    percent: None,
                });
            }
            let (verb, rest) = t.split_once(':')?;
            if !CHANGE.iter().any(|v| v.eq_ignore_ascii_case(verb.trim())) {
                return None;
            }
            let mut w = rest.split_whitespace();
            let package = w.next()?.to_owned();
            let (done, total) = w.last()?.split_once('/')?;
            Some(Step {
                package: Some(package),
                done: done.parse().ok(),
                total: total.parse().ok(),
                percent: None,
            })
        }
        "xbps" => {
            // `foo-1.0_1: installing ...`, `foo-1.0_1: installed successfully.`
            let (pkg, rest) = t.split_once(": ")?;
            if pkg.contains(' ') {
                return None;
            }
            let rest = rest.trim();
            let doing = ["unpacking", "installing", "configuring", "updating", "removing"];
            if rest.ends_with("successfully.") || doing.iter().any(|d| rest.starts_with(d)) {
                return Some(Step { package: Some(pkg.to_owned()), ..Default::default() });
            }
            None
        }
        "flatpak" => {
            // `Installing 3/10… ████▌  45%  1.2 MB/s`.
            let (verb, rest) = t.split_once(' ')?;
            if !CHANGE.iter().any(|v| v.eq_ignore_ascii_case(verb)) {
                return None;
            }
            let frac = rest.trim_end_matches(|c: char| c != '…' && c != '.' && !c.is_ascii_digit());
            let f = frac.split(['…', ' ']).next()?;
            let (a, b) = f.trim_end_matches('.').split_once('/')?;
            Some(Step {
                package: None,
                done: a.parse::<usize>().ok().map(|a| a.saturating_sub(1)),
                total: b.parse().ok(),
                percent: percent_in(rest),
            })
        }
        "brew" => {
            // `==> Installing foo`, `==> Upgrading foo`, `==> Pouring foo--1.0…`.
            let rest = t.strip_prefix("==> ")?;
            let package = verbs(rest, &["installing", "upgrading", "uninstalling", "pouring"])?;
            Some(Step {
                package: Some(package.split("--").next().unwrap_or(&package).to_owned()),
                ..Default::default()
            })
        }
        _ => None,
    }
}

/// Follows one running package manager command: its progress from its
/// output, line by line.
#[derive(Debug)]
pub struct Tracker {
    pub command: PkgCommand,
    pub started: Instant,
    pub package: Option<String>,
    pub done: usize,
    pub total: Option<usize>,
    pub percent: Option<u8>,
    /// Counting lines that name a package (apt, xbps, brew) when the
    /// manager gives no totals.
    counted: std::collections::BTreeSet<String>,
    partial: String,
}

impl Tracker {
    pub fn new(command: PkgCommand) -> Tracker {
        Tracker {
            command,
            started: Instant::now(),
            package: None,
            done: 0,
            total: None,
            percent: None,
            counted: Default::default(),
            partial: String::new(),
        }
    }

    /// Progress so far, 0–100, when known.
    pub fn progress(&self) -> Option<u8> {
        self.percent
            .or_else(|| self.total.filter(|t| *t > 0).map(|t| ((self.done.min(t) * 100) / t) as u8))
    }

    /// Feed printed text. Returns true if the progress changed.
    pub fn feed(&mut self, text: &str) -> bool {
        self.partial.push_str(text);
        let mut changed = false;
        // Progress bars redraw a line with \r; treat that as a line end.
        while let Some(i) = self.partial.find(['\n', '\r']) {
            let line: String = self.partial.drain(..=i).collect();
            changed |= self.line(line.trim_end_matches(['\n', '\r']));
        }
        if self.partial.len() > 4096 {
            self.partial.clear();
        }
        changed
    }

    fn line(&mut self, line: &str) -> bool {
        let Some(s) = step(self.command.manager, line) else { return false };
        let before = (self.package.clone(), self.done, self.total, self.percent);
        if let Some(p) = &s.package {
            if s.done.is_none() && self.counted.insert(p.clone()) && self.total.is_none() {
                // apt: "Setting up" each one; xbps: each package.
                self.done = self.counted.len().saturating_sub(1);
            }
            self.package = Some(p.clone());
        }
        if let Some(d) = s.done {
            self.done = d;
        }
        if s.total.is_some() {
            self.total = s.total;
        }
        if s.percent.is_some() {
            self.percent = s.percent;
        }
        before != (self.package.clone(), self.done, self.total, self.percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Option<(&'static str, Op, Vec<String>)> {
        parse(line).map(|c| (c.manager, c.op, c.packages))
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn commands_of_each_manager() {
        use Op::*;
        type Case<'a> = (&'a str, Option<(&'a str, Op, &'a [&'a str])>);
        let cases: &[Case] = &[
            (
                "sudo emerge -av www-client/firefox",
                Some(("portage", Install, &["www-client/firefox"])),
            ),
            (
                "emerge --ask --verbose --update --deep --newuse @world",
                Some(("portage", Upgrade, &["@world"])),
            ),
            ("emerge -uDNav @world", Some(("portage", Upgrade, &["@world"]))),
            ("emerge --sync", Some(("portage", Sync, &[]))),
            ("emaint sync -a", Some(("portage", Sync, &[]))),
            ("emerge -C dev-libs/foo", Some(("portage", Remove, &["dev-libs/foo"]))),
            ("emerge --depclean", Some(("portage", Remove, &[]))),
            ("emerge -pv firefox", None),
            ("emerge -s firefox", None),
            ("emerge -j 4 vim", Some(("portage", Install, &["vim"]))),
            ("sudo pacman -S firefox vlc", Some(("pacman", Install, &["firefox", "vlc"]))),
            ("pacman -Syu", Some(("pacman", Upgrade, &[]))),
            ("pacman -Sy", Some(("pacman", Sync, &[]))),
            ("pacman -Rns firefox", Some(("pacman", Remove, &["firefox"]))),
            ("pacman -U ./pkg.tar.zst", Some(("pacman", Install, &["./pkg.tar.zst"]))),
            ("pacman -Ss firefox", None),
            ("pacman -Qi firefox", None),
            ("paru", Some(("paru", Upgrade, &[]))),
            ("yay -S google-chrome", Some(("yay", Install, &["google-chrome"]))),
            ("sudo apt install -y firefox", Some(("apt", Install, &["firefox"]))),
            (
                "sudo apt-get -o Dpkg::Options::=--force-confnew install vim",
                Some(("apt", Install, &["vim"])),
            ),
            ("sudo apt update && sudo apt upgrade -y", Some(("apt", Sync, &[]))),
            ("apt full-upgrade", Some(("apt", Upgrade, &[]))),
            ("apt purge foo", Some(("apt", Remove, &["foo"]))),
            ("apt search foo", None),
            ("nala install htop", Some(("nala", Install, &["htop"]))),
            ("sudo dnf install -y firefox", Some(("dnf", Install, &["firefox"]))),
            ("dnf upgrade --refresh", Some(("dnf", Upgrade, &[]))),
            (
                "dnf group install 'Development Tools'",
                Some(("dnf", Install, &["Development Tools"])),
            ),
            ("yum remove httpd", Some(("yum", Remove, &["httpd"]))),
            ("dnf list installed", None),
            ("zypper -n in vim", Some(("zypper", Install, &["vim"]))),
            ("sudo zypper dup", Some(("zypper", Upgrade, &[]))),
            ("zypper ref", Some(("zypper", Sync, &[]))),
            ("sudo xbps-install -Su", Some(("xbps", Upgrade, &[]))),
            ("xbps-install -S firefox", Some(("xbps", Install, &["firefox"]))),
            ("xbps-install -S", Some(("xbps", Sync, &[]))),
            ("xbps-remove -R firefox", Some(("xbps", Remove, &["firefox"]))),
            ("doas apk add curl", Some(("apk", Install, &["curl"]))),
            ("apk update", Some(("apk", Sync, &[]))),
            ("eopkg it vlc", Some(("eopkg", Install, &["vlc"]))),
            ("nix-env -iA nixpkgs.hello", Some(("nix", Install, &["nixpkgs.hello"]))),
            ("nix profile install nixpkgs#hello", Some(("nix", Install, &["nixpkgs#hello"]))),
            (
                "flatpak install flathub org.gimp.GIMP",
                Some(("flatpak", Install, &["flathub", "org.gimp.GIMP"])),
            ),
            ("flatpak update", Some(("flatpak", Upgrade, &[]))),
            ("sudo snap install code --classic", Some(("snap", Install, &["code"]))),
            ("brew upgrade", Some(("brew", Upgrade, &[]))),
            ("run0 pacman -S git", Some(("pacman", Install, &["git"]))),
            ("pkexec --user root dnf install git", Some(("dnf", Install, &["git"]))),
            (
                "sudo -E -u root env LANG=C nice -n 5 apt install git",
                Some(("apt", Install, &["git"])),
            ),
            (
                "DEBIAN_FRONTEND=noninteractive apt-get install -y git",
                Some(("apt", Install, &["git"])),
            ),
            ("cd /tmp; /usr/bin/pacman -S git", Some(("pacman", Install, &["git"]))),
            ("echo pacman -S git", None),
            ("ls -l", None),
            ("vim install.sh", None),
            (
                "python3.12 -b /usr/lib/python-exec/python3.12/emerge -av vim",
                Some(("portage", Install, &["vim"])),
            ),
            ("sh -c 'apt-get install -y curl'", Some(("apt", Install, &["curl"]))),
            ("/bin/sh ./apt install htop", Some(("apt", Install, &["htop"]))),
        ];
        for (line, want) in cases {
            let want = want.map(|(m, op, pk)| (m, op, v(pk)));
            assert_eq!(p(line), want, "{line}");
        }
    }

    #[test]
    fn progress_lines() {
        let s = |m, l| step(m, l);
        let pk = |p: &str, d, t| {
            Some(Step { package: Some(p.into()), done: Some(d), total: Some(t), percent: None })
        };
        assert_eq!(
            s("pacman", "(3/10) installing firefox                  [####] 100%"),
            pk("firefox", 3, 10)
        );
        assert_eq!(s("pacman", "(1/4) checking keys in keyring"), None);
        assert_eq!(s("pacman", "(2/2) removing vlc"), pk("vlc", 2, 2));
        assert_eq!(
            s("portage", ">>> Emerging (2 of 5) dev-libs/foo-1.0::gentoo"),
            pk("dev-libs/foo-1.0", 1, 5)
        );
        assert_eq!(
            s("portage", ">>> Completed (2 of 5) dev-libs/foo-1.0::gentoo"),
            pk("dev-libs/foo-1.0", 2, 5)
        );
        assert_eq!(
            s("portage", ">>> Unmerging (1 of 1) dev-libs/foo-1.0..."),
            pk("dev-libs/foo-1.0", 0, 1)
        );
        assert_eq!(
            s("zypper", "(3/10) Installing: vim-9.0-1.1.x86_64 ..........[done]"),
            pk("vim-9.0-1.1.x86_64", 3, 10)
        );
        assert_eq!(s("apk", "(1/3) Installing curl (8.5.0-r0)"), pk("curl", 1, 3));
        assert_eq!(
            s("dnf", "  Installing       : firefox-120.0-1.fc39.x86_64       3/10 "),
            pk("firefox-120.0-1.fc39.x86_64", 3, 10)
        );
        assert_eq!(s("dnf", "  Verifying        : firefox-120.0-1.fc39.x86_64       3/10 "), None);
        assert_eq!(
            s("dnf", "[3/10] Installing firefox-0:120.0-1.fc40.x86_64   100% |  10.0 MiB/s"),
            pk("firefox-0:120.0-1.fc40.x86_64", 3, 10)
        );
        assert_eq!(
            s("apt", "Setting up firefox (120.0+build1-0ubuntu1) ..."),
            Some(Step { package: Some("firefox".into()), ..Default::default() })
        );
        assert_eq!(
            s("apt", "Unpacking libfoo1:amd64 (1.2-3) over (1.2-2) ..."),
            Some(Step { package: Some("libfoo1".into()), ..Default::default() })
        );
        assert_eq!(
            s("apt", "Progress: [ 45%] [#####.....]"),
            Some(Step { percent: Some(45), ..Default::default() })
        );
        assert_eq!(
            s("xbps", "firefox-120.0_1: installed successfully."),
            Some(Step { package: Some("firefox-120.0_1".into()), ..Default::default() })
        );
        assert_eq!(s("xbps", "Size to download: 50MB"), None);
        assert_eq!(
            s("flatpak", "Installing 2/3… ████████▌   45%  1.2 MB/s  00:10"),
            Some(Step { package: None, done: Some(1), total: Some(3), percent: Some(45) })
        );
        assert_eq!(
            s("brew", "==> Pouring wget--1.21.4.arm64_sonoma.bottle.tar.gz"),
            Some(Step { package: Some("wget".into()), ..Default::default() })
        );
    }

    #[test]
    fn tracker_follows_output() {
        let mut t = Tracker::new(parse("pacman -S a b").unwrap());
        assert!(t.feed("resolving dependencies...\n(1/2) installing a\n(2/2) ins"));
        assert_eq!(
            (t.package.as_deref(), t.done, t.total, t.progress()),
            (Some("a"), 1, Some(2), Some(50))
        );
        assert!(t.feed("talling b\n"));
        assert_eq!(t.progress(), Some(100));
        assert!(!t.feed(":: Running post-transaction hooks...\n"));
        // apt: no totals, so packages are counted.
        let mut t = Tracker::new(parse("apt install a").unwrap());
        t.feed("Unpacking a (1) ...\nSetting up a (1) ...\nUnpacking b (1) ...\n");
        assert_eq!((t.package.as_deref(), t.done, t.progress()), (Some("b"), 1, None));
        t.feed("Progress: [ 60%] [###]\r");
        assert_eq!(t.progress(), Some(60));
    }
}

//! Desktop integration: the window icon, and installing Seraphim into the
//! application menu (KDE, GNOME and any freedesktop.org desktop) with
//! `seraphim --install-desktop`.
//!
//! The icon is pixel art, so every size is a whole-number (or, for 48 px,
//! nearest-pixel) enlargement of the 32 px original and stays crisp; see
//! `extra/icons`. The menu entry is `extra/linux/seraphim.desktop`, with
//! `Exec` pointing at the binary that installs it.

use std::path::{Path, PathBuf};

/// The desktop entry's id: windows say they are `seraphim` (X11 `WM_CLASS`,
/// Wayland app id) so desktops match them to the menu entry and its icon.
pub const APP_ID: &str = "seraphim";

const DESKTOP_ENTRY: &str = include_str!("../../../extra/linux/seraphim.desktop");

/// (size, PNG) for the hicolor icon theme.
const ICONS: &[(u32, &[u8])] = &[
    (32, include_bytes!("../../../extra/icons/hicolor/32x32/apps/seraphim.png")),
    (48, include_bytes!("../../../extra/icons/hicolor/48x48/apps/seraphim.png")),
    (64, include_bytes!("../../../extra/icons/hicolor/64x64/apps/seraphim.png")),
    (128, include_bytes!("../../../extra/icons/hicolor/128x128/apps/seraphim.png")),
    (256, include_bytes!("../../../extra/icons/hicolor/256x256/apps/seraphim.png")),
    (512, include_bytes!("../../../extra/icons/hicolor/512x512/apps/seraphim.png")),
];

/// RGBA pixels of a PNG (the icons are 8-bit RGBA).
fn decode(png_bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba)
        .then(|| (buf[..info.buffer_size()].to_vec(), info.width, info.height))
}

/// The icon for Seraphim's windows (where the desktop shows the window's
/// own icon rather than the menu entry's). 64 px: desktops scale it down
/// for title bars and taskbars.
pub fn window_icon() -> Option<winit::window::Icon> {
    let (_, png) = ICONS.iter().find(|(s, _)| *s == 64)?;
    let (rgba, w, h) = decode(png)?;
    winit::window::Icon::from_rgba(rgba, w, h).ok()
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
fn data_home() -> Result<PathBuf, String> {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share"))
}

/// A path as a desktop-entry `Exec` argument: quoted when it has to be,
/// with `"`, `` ` ``, `$` and `\` escaped, and `%` doubled.
fn exec_arg(path: &Path) -> String {
    let s = path.to_string_lossy().replace('%', "%%");
    let plain = s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@".contains(c));
    if plain {
        return s;
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The menu entry for the binary at `exe`.
fn entry_for(exe: &Path) -> String {
    let exec = exec_arg(exe);
    let mut out = String::new();
    for line in DESKTOP_ENTRY.lines() {
        match line.strip_prefix("Exec=") {
            Some(rest) => {
                // `Exec=seraphim [args]` → the installed binary.
                let args = rest.strip_prefix("seraphim").unwrap_or(rest);
                out.push_str(&format!("Exec={exec}{args}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
        if line == "Type=Application" {
            // Hide the entry if the binary goes away.
            out.push_str(&format!("TryExec={exec}\n"));
        }
    }
    out
}

/// Where each file goes under `data`.
fn targets(data: &Path) -> (PathBuf, Vec<(PathBuf, &'static [u8])>) {
    let entry = data.join("applications").join(format!("{APP_ID}.desktop"));
    let icons = ICONS
        .iter()
        .map(|(s, png)| (data.join(format!("icons/hicolor/{s}x{s}/apps/{APP_ID}.png")), *png))
        .collect();
    (entry, icons)
}

/// Tell the desktop the menu changed (each tool is optional).
fn refresh(data: &Path) {
    let quiet = |cmd: &str, args: &[&std::ffi::OsStr]| {
        let _ = std::process::Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    quiet("update-desktop-database", &[data.join("applications").as_os_str()]);
    // KDE rebuilds its menu cache when it notices; this makes it immediate.
    for k in ["kbuildsycoca6", "kbuildsycoca5"] {
        if std::process::Command::new(k)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            break;
        }
    }
}

/// `seraphim --install-desktop`: the menu entry and icons, for this user.
pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("where is seraphim? {e}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let data = data_home()?;
    let (entry, icons) = targets(&data);
    for (path, png) in &icons {
        write(path, png)?;
    }
    write(&entry, entry_for(&exe).as_bytes())?;
    refresh(&data);
    println!("Installed the Seraphim menu entry: {}", entry.display());
    println!("  runs {}", exe.display());
    println!("  icons in {}", data.join("icons/hicolor").display());
    println!("Remove it with `seraphim --uninstall-desktop`.");
    Ok(())
}

/// `seraphim --uninstall-desktop`.
pub fn uninstall() -> Result<(), String> {
    let data = data_home()?;
    let (entry, icons) = targets(&data);
    let mut removed = 0;
    for path in std::iter::once(&entry).chain(icons.iter().map(|(p, _)| p)) {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    refresh(&data);
    println!("Removed {removed} files (the menu entry and its icons).");
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode_at_their_sizes() {
        for (size, png) in ICONS {
            let (rgba, w, h) = decode(png).expect("decodes");
            assert_eq!((w, h), (*size, *size));
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            assert_eq!(rgba[3], 0, "{size}: transparent corner");
        }
        assert!(window_icon().is_some());
        // Enlargements keep the pixel art: 64 px is the 32 px one, doubled.
        let (a, ..) = decode(ICONS[0].1).unwrap();
        let (b, ..) = decode(ICONS[2].1).unwrap();
        for y in 0..64usize {
            for x in 0..64usize {
                let (i, j) = (((y / 2) * 32 + x / 2) * 4, (y * 64 + x) * 4);
                assert_eq!(a[i..i + 4], b[j..j + 4], "({x},{y})");
            }
        }
    }

    #[test]
    fn entry_points_at_the_binary() {
        let e = entry_for(Path::new("/home/me/.cargo/bin/seraphim"));
        assert!(e.contains("\nExec=/home/me/.cargo/bin/seraphim\n"), "{e}");
        assert!(e.contains("\nTryExec=/home/me/.cargo/bin/seraphim\n"));
        assert!(e.contains("Icon=seraphim\n") && e.contains("StartupWMClass=seraphim\n"));
        assert_eq!(
            e.matches("Exec=/home/me/.cargo/bin/seraphim\n").count(),
            3,
            "main, TryExec, action"
        );
        let odd = entry_for(Path::new("/opt/My Apps/100%/sera$phim"));
        assert!(odd.contains("Exec=\"/opt/My Apps/100%%/sera\\$phim\"\n"), "{odd}");
    }

    #[test]
    fn files_follow_the_xdg_layout() {
        let d = std::env::temp_dir().join(format!("seraphim-desktop-{}", std::process::id()));
        let (entry, icons) = targets(&d);
        assert_eq!(entry, d.join("applications/seraphim.desktop"));
        assert_eq!(icons[1].0, d.join("icons/hicolor/48x48/apps/seraphim.png"));
        assert_eq!(icons.len(), 6);
    }
}

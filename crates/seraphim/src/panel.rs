//! Panels: retained widget trees painted into a cell grid (ADR 0006).
//!
//! Built-in panels and extensions describe *what* to show as a list of
//! [`Widget`]s; this module lays them out and paints them as ANSI text for
//! a virtual terminal, which the window draws with the normal text
//! renderer. Extensions never draw, so fonts, themes, colors and reduced
//! motion stay Seraphim's.

use serde::Deserialize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::menu::Palette;

/// Text emphasis, mapped to theme colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    #[default]
    Normal,
    Dim,
    Bold,
    Accent,
    Good,
    Warn,
    Alert,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Item {
    pub text: String,
    /// Right-aligned, dimmed.
    pub detail: String,
    pub style: Style,
}

/// One block of a panel, top to bottom.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Widget {
    Heading {
        text: String,
    },
    Text {
        text: String,
        #[serde(default)]
        style: Style,
    },
    /// Label and value pairs, aligned.
    #[serde(alias = "kv")]
    Keyvalue {
        items: Vec<(String, String)>,
    },
    /// A bar for a value from 0 to 1.
    Gauge {
        #[serde(default)]
        label: String,
        value: f32,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        style: Style,
    },
    /// Recent values as block characters.
    Sparkline {
        #[serde(default)]
        label: String,
        values: Vec<f32>,
        #[serde(default)]
        max: Option<f32>,
    },
    /// Selectable rows (Up/Down, Enter, clicks).
    List {
        items: Vec<Item>,
    },
    Separator,
}

/// What a click or key in a panel landed on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Painted {
    pub bytes: Vec<u8>,
    /// Rows (of the grid) holding list items: (row, item index).
    pub rows: Vec<(usize, usize)>,
    /// Number of list items in the panel.
    pub items: usize,
}

fn rgb(c: [u8; 3]) -> String {
    format!("{};{};{}", c[0], c[1], c[2])
}

fn color(p: &Palette, s: Style, good: [u8; 3], warn: [u8; 3], alert: [u8; 3]) -> [u8; 3] {
    match s {
        Style::Normal | Style::Bold => p.menu_fg,
        Style::Dim => p.dim,
        Style::Accent => p.accent,
        Style::Good => good,
        Style::Warn => warn,
        Style::Alert => alert,
    }
}

fn cut(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if out.width() + cw + 1 > w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Theme colors a panel uses beyond the menu palette.
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub good: [u8; 3],
    pub warn: [u8; 3],
    pub alert: [u8; 3],
}

impl Colors {
    pub fn from_theme(t: &seraphim_render::Theme) -> Colors {
        Colors { good: t.ansi[10], warn: t.ansi[11], alert: t.ansi[9] }
    }
}

/// Paint `title` and `widgets` into `cols` x `rows`. `selected` is the
/// highlighted list item (across all lists), `focused` whether the panel
/// has the keyboard. The view scrolls to keep the selection visible.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    title: &str,
    widgets: &[Widget],
    cols: usize,
    rows: usize,
    p: &Palette,
    c: Colors,
    selected: Option<usize>,
    focused: bool,
) -> Painted {
    let bg = p.menu_bg;
    let reset = format!("\x1b[0;38;2;{};48;2;{}m", rgb(p.menu_fg), rgb(bg));
    // Lines first (text, and which item each holds), then scroll and emit.
    let mut lines: Vec<(String, Option<usize>)> = Vec::new();
    let inner = cols.saturating_sub(2).max(1);
    let mut item = 0usize;
    let fg = |s: Style| color(p, s, c.good, c.warn, c.alert);
    for w in widgets {
        match w {
            Widget::Heading { text } => {
                lines.push((format!("\x1b[1;38;2;{}m{}", rgb(p.accent), cut(text, inner)), None));
            }
            Widget::Text { text, style } => {
                // Wrap on words.
                let mut line = String::new();
                for word in text.split_whitespace() {
                    if !line.is_empty() && line.width() + 1 + word.width() > inner {
                        lines.push((format!("\x1b[38;2;{}m{}", rgb(fg(*style)), line), None));
                        line.clear();
                    }
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&cut(word, inner));
                }
                let bold = if *style == Style::Bold { "\x1b[1m" } else { "" };
                lines.push((format!("{bold}\x1b[38;2;{}m{}", rgb(fg(*style)), line), None));
            }
            Widget::Keyvalue { items } => {
                let kw = items.iter().map(|(k, _)| k.width()).max().unwrap_or(0).min(inner / 2);
                for (k, v) in items {
                    let k = cut(k, kw);
                    let pad = " ".repeat(kw - k.width() + 2);
                    let v = cut(v, inner.saturating_sub(kw + 2));
                    lines.push((
                        format!(
                            "\x1b[38;2;{}m{k}{pad}\x1b[38;2;{}m{v}",
                            rgb(p.dim),
                            rgb(p.menu_fg)
                        ),
                        None,
                    ));
                }
            }
            Widget::Gauge { label, value, text, style } => {
                let v = value.clamp(0.0, 1.0);
                let text = text.clone().unwrap_or_else(|| format!("{:.0}%", v * 100.0));
                let lw = label.width().min(inner / 3);
                let bar_w = inner.saturating_sub(lw + text.width() + 2).max(4);
                let filled = (bar_w as f32 * v).round() as usize;
                let hue = match style {
                    Style::Normal if v > 0.9 => c.alert,
                    Style::Normal if v > 0.7 => c.warn,
                    Style::Normal => p.accent,
                    s => fg(*s),
                };
                let lbl = cut(label, lw);
                let gap = " ".repeat(lw - lbl.width() + usize::from(lw > 0));
                lines.push((
                    format!(
                        "\x1b[38;2;{}m{lbl}{gap}\x1b[38;2;{}m{}\x1b[38;2;{}m{} \x1b[38;2;{}m{text}",
                        rgb(p.dim),
                        rgb(hue),
                        "█".repeat(filled),
                        rgb(p.border),
                        "░".repeat(bar_w - filled),
                        rgb(p.menu_fg)
                    ),
                    None,
                ));
            }
            Widget::Sparkline { label, values, max } => {
                const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                let lw = label.width().min(inner / 3);
                let room = inner.saturating_sub(lw + usize::from(lw > 0));
                let vals = &values[values.len().saturating_sub(room)..];
                let top = max.unwrap_or_else(|| vals.iter().cloned().fold(0.0, f32::max)).max(1e-6);
                let spark: String = vals
                    .iter()
                    .map(|v| BARS[((v / top).clamp(0.0, 1.0) * 7.0).round() as usize])
                    .collect();
                let lbl = cut(label, lw);
                let gap = " ".repeat(lw - lbl.width() + usize::from(lw > 0));
                lines.push((
                    format!(
                        "\x1b[38;2;{}m{lbl}{gap}\x1b[38;2;{}m{spark}",
                        rgb(p.dim),
                        rgb(p.accent)
                    ),
                    None,
                ));
            }
            Widget::List { items } => {
                for it in items {
                    let sel = selected == Some(item);
                    let detail = cut(&it.detail, inner / 2);
                    let room =
                        inner.saturating_sub(detail.width() + usize::from(!detail.is_empty()) + 2);
                    let text = cut(&it.text, room);
                    let pad = " ".repeat(inner.saturating_sub(2 + text.width() + detail.width()));
                    let (f, b) = if sel && focused {
                        (p.hi_fg, p.hi_bg)
                    } else if sel {
                        (p.menu_fg, p.border)
                    } else {
                        (fg(it.style), bg)
                    };
                    let mark = if sel { "▸ " } else { "  " };
                    lines.push((
                        format!(
                            "\x1b[38;2;{};48;2;{}m{mark}{text}{pad}\x1b[38;2;{}m{detail}",
                            rgb(f),
                            rgb(b),
                            rgb(if sel && focused { p.hi_fg } else { p.dim })
                        ),
                        Some(item),
                    ));
                    item += 1;
                }
            }
            Widget::Separator => {
                lines.push((format!("\x1b[38;2;{}m{}", rgb(p.border), "─".repeat(inner)), None));
            }
        }
    }
    // Row 0 is the title; the rest scrolls to keep the selection in view.
    let body = rows.saturating_sub(1).max(1);
    let sel_line = selected.and_then(|s| lines.iter().position(|(_, i)| *i == Some(s)));
    let first = match sel_line {
        Some(l) if l >= body => l + 1 - body,
        _ => 0,
    };
    let mut out = String::from("\x1b[H\x1b[?7l\x1b[?25l");
    out.push_str(&reset);
    out.push_str("\x1b[2J");
    let title_bg = if focused { p.hi_bg } else { p.bar_bg };
    let title_fg = if focused { p.hi_fg } else { p.bar_fg };
    let t = cut(title, cols.saturating_sub(2));
    out.push_str(&format!(
        "\x1b[1;1H\x1b[1;38;2;{};48;2;{}m {t}{}",
        rgb(title_fg),
        rgb(title_bg),
        " ".repeat(cols.saturating_sub(t.width() + 1))
    ));
    let mut painted = Painted { items: item, ..Default::default() };
    for (k, (text, it)) in lines.iter().skip(first).take(body).enumerate() {
        let row = k + 1;
        out.push_str(&format!("\x1b[{};1H{reset}\x1b[2K {text}", row + 1));
        if let Some(i) = it {
            painted.rows.push((row, *i));
        }
    }
    out.push_str("\x1b[0m");
    painted.bytes = out.into_bytes();
    painted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &[u8], cols: usize, rows: usize) -> String {
        let mut t = seraphim_terminal::Terminal::new(cols, rows);
        t.advance(bytes);
        t.screen_text()
    }

    fn style() -> (Palette, Colors) {
        let t = seraphim_render::Theme::default();
        (Palette::from_theme(&t), Colors::from_theme(&t))
    }

    #[test]
    fn widgets_paint() {
        let (p, c) = style();
        let w = vec![
            Widget::Heading { text: "System".into() },
            Widget::Gauge { label: "CPU".into(), value: 0.5, text: None, style: Style::Normal },
            Widget::Sparkline { label: "".into(), values: vec![0.0, 1.0, 2.0, 4.0], max: None },
            Widget::Keyvalue {
                items: vec![("Load".into(), "0.42".into()), ("Uptime".into(), "3 days".into())],
            },
            Widget::Separator,
            Widget::Text {
                text: "a fairly long sentence that has to wrap onto lines".into(),
                style: Style::Dim,
            },
        ];
        let out = paint("Monitor", &w, 30, 12, &p, c, None, false);
        let s = screen(&out.bytes, 30, 12);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0].trim(), "Monitor");
        assert_eq!(lines[1].trim(), "System");
        assert!(
            lines[2].contains("CPU") && lines[2].contains('█') && lines[2].ends_with("50%"),
            "{s}"
        );
        assert!(lines[3].trim_start().starts_with("▁▃▅█") || lines[3].contains('█'), "{s}");
        assert!(lines[4].contains("Load    0.42"), "{s}");
        assert!(lines[6].contains('─'));
        assert!(lines.len() >= 9, "text wrapped: {s}");
        assert_eq!(out.items, 0);
    }

    #[test]
    fn lists_select_and_scroll() {
        let (p, c) = style();
        let items: Vec<Item> = (0..20)
            .map(|i| Item { text: format!("item {i}"), detail: "x".into(), ..Default::default() })
            .collect();
        let w = vec![Widget::List { items }];
        let out = paint("L", &w, 20, 6, &p, c, Some(12), true);
        let s = screen(&out.bytes, 20, 6);
        assert!(s.contains("▸ item 12"), "{s}");
        assert_eq!(out.items, 20);
        assert_eq!(out.rows.last(), Some(&(5, 12)), "scrolled so the selection is the last row");
        let out = paint("L", &w, 20, 6, &p, c, None, true);
        assert_eq!(out.rows[0], (1, 0));
    }

    #[test]
    fn widgets_from_lua_shaped_json() {
        let w: Vec<Widget> = serde_json::from_value(serde_json::json!([
            { "type": "heading", "text": "Hi" },
            { "type": "gauge", "label": "Disk", "value": 0.95 },
            { "type": "kv", "items": [["a", "b"]] },
            { "type": "list", "items": [{ "text": "one", "style": "alert" }] },
            { "type": "separator" }
        ]))
        .unwrap();
        assert_eq!(w.len(), 5);
        assert!(matches!(&w[3], Widget::List { items } if items[0].style == Style::Alert));
        assert!(serde_json::from_value::<Widget>(serde_json::json!({ "type": "video" })).is_err());
    }
}

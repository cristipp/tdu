//! Colour handling that works in every terminal: truecolor emulators, the
//! 256-colour palette, and the 16-colour Linux console.

use ratatui::style::Color;

/// How many colours the terminal can show. The widget computes RGB colours
/// internally and down-converts them with [`ColorMode::convert`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// 24-bit RGB (most modern emulators: iTerm2, WezTerm, kitty, Alacritty,
    /// GNOME Terminal, Konsole, macOS 26+ Terminal.app).
    #[default]
    TrueColor,
    /// xterm 256-colour palette (older Terminal.app, tmux without `Tc`, ...).
    Indexed256,
    /// The 16 basic ANSI colours (Linux virtual console, very old terminals).
    Basic16,
}

impl ColorMode {
    /// Best guess from the environment (`COLORTERM`, `TERM`).
    pub fn detect() -> Self {
        let get = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
        Self::from_env(&get("COLORTERM"), &get("TERM"))
    }

    /// [`detect`](Self::detect) with explicit values, for testing.
    pub fn from_env(colorterm: &str, term: &str) -> Self {
        if colorterm == "truecolor" || colorterm == "24bit" {
            return ColorMode::TrueColor;
        }
        if term.contains("direct") || term.contains("truecolor") {
            ColorMode::TrueColor
        } else if term.contains("256color") {
            ColorMode::Indexed256
        } else if term == "linux" || term.starts_with("vt") || term == "dumb" || term.is_empty() {
            ColorMode::Basic16
        } else {
            // Plain `xterm`, `screen`, `tmux`, ... nearly always support 256.
            ColorMode::Indexed256
        }
    }

    /// Down-convert an RGB colour for this mode. Other colours pass through.
    pub fn convert(self, c: Color) -> Color {
        match (self, c) {
            (ColorMode::TrueColor, _) => c,
            (ColorMode::Indexed256, Color::Rgb(r, g, b)) => Color::Indexed(rgb_to_256(r, g, b)),
            (ColorMode::Basic16, Color::Rgb(r, g, b)) => rgb_to_16(r, g, b),
            _ => c,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "truecolor" | "24bit" => Some(ColorMode::TrueColor),
            "256" => Some(ColorMode::Indexed256),
            "16" => Some(ColorMode::Basic16),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Hsl {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}

impl Hsl {
    pub fn to_color(self) -> Color {
        let l = self.l.clamp(0.0, 1.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * self.s;
        let hp = self.h.rem_euclid(360.0) / 60.0;
        let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
        let (r, g, b) = match hp as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Color::Rgb(to(r), to(g), to(b))
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return Hsl { h: 0.0, s: 0.0, l };
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            60.0 * ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        Hsl { h, s, l }
    }
}

/// Nearest entry in the xterm 256-colour palette (6×6×6 cube or grey ramp).
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let idx = |v: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (**l as i32 - v as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (idx(r), idx(g), idx(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi_ = ((avg.saturating_sub(8)) / 10).min(23) as u8;
    let grey = 8 + 10 * gi_;
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if dist((grey, grey, grey)) < dist(cube) {
        232 + gi_
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

/// Map by hue family and lightness. Nearest-RGB matching would turn the dark,
/// muted tile colours almost all black, so hue is what we keep.
fn rgb_to_16(r: u8, g: u8, b: u8) -> Color {
    let Hsl { h, s, l } = Hsl::from_rgb(r, g, b);
    if s < 0.15 {
        return match l {
            l if l < 0.2 => Color::Black,
            l if l < 0.5 => Color::DarkGray,
            l if l < 0.85 => Color::Gray,
            _ => Color::White,
        };
    }
    let light = l >= 0.45;
    match (((h + 30.0) / 60.0) as usize % 6, light) {
        (0, false) => Color::Red,
        (0, true) => Color::LightRed,
        (1, false) => Color::Yellow,
        (1, true) => Color::LightYellow,
        (2, false) => Color::Green,
        (2, true) => Color::LightGreen,
        (3, false) => Color::Cyan,
        (3, true) => Color::LightCyan,
        (4, false) => Color::Blue,
        (4, true) => Color::LightBlue,
        (_, false) => Color::Magenta,
        (_, true) => Color::LightMagenta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_terminals() {
        use ColorMode::*;
        assert_eq!(
            ColorMode::from_env("truecolor", "xterm-256color"),
            TrueColor
        );
        assert_eq!(ColorMode::from_env("", "xterm-256color"), Indexed256);
        assert_eq!(ColorMode::from_env("", "screen"), Indexed256);
        assert_eq!(ColorMode::from_env("", "linux"), Basic16);
        assert_eq!(ColorMode::from_env("", "xterm-direct"), TrueColor);
    }

    #[test]
    fn converts_rgb() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(128, 128, 128), 244);
        assert_eq!(rgb_to_16(160, 30, 30), Color::Red);
        assert_eq!(rgb_to_16(30, 40, 150), Color::Blue);
        assert_eq!(rgb_to_16(250, 250, 250), Color::White);
        assert_eq!(ColorMode::Basic16.convert(Color::Black), Color::Black);
    }
}

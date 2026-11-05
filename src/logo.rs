//! herdr-feed wordmark: big ASCII splash and a one-line badge, both with the brand gradient.

pub const BIG: [&str; 6] = [
    "███████╗███████╗███████╗██████╗ ",
    "██╔════╝██╔════╝██╔════╝██╔══██╗",
    "█████╗  █████╗  █████╗  ██║  ██║",
    "██╔══╝  ██╔══╝  ██╔══╝  ██║  ██║",
    "██║     ███████╗███████╗██████╔╝",
    "╚═╝     ╚══════╝╚══════╝╚═════╝ ",
];

/// Brand gradient: ember → magenta → violet.
const STOPS: [(f64, f64, f64); 3] = [
    (255.0, 112.0, 67.0),
    (255.0, 64.0, 140.0),
    (140.0, 92.0, 255.0),
];

pub fn grad(t: f64) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f64;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f64;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    (
        (a.0 + (b.0 - a.0) * f) as u8,
        (a.1 + (b.1 - a.1) * f) as u8,
        (a.2 + (b.2 - a.2) * f) as u8,
    )
}

/// Paint a string left-to-right across the gradient; shadow glyphs (╗╔═…) are dimmed.
pub fn paint(s: &str, shade_box: bool) -> String {
    let n = s.chars().count().max(2) as f64 - 1.0;
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        let (r, g, b) = grad(i as f64 / n);
        let k = if shade_box && ch != '█' && ch != ' ' {
            0.45
        } else {
            1.0
        };
        out += &format!(
            "\x1b[38;2;{};{};{}m{ch}",
            (r as f64 * k) as u8,
            (g as f64 * k) as u8,
            (b as f64 * k) as u8
        );
    }
    out + "\x1b[0m"
}

/// Compact top-left badge used on every card.
pub fn badge() -> String {
    format!("\x1b[1m{}\x1b[0m", paint("▚ herdr-feed", false))
}

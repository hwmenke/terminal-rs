//! Look and feel: a native macOS-style chrome (SF Pro, rounded cards, system accent,
//! light/dark following the system) around Bloomberg-dense data (SF Mono numbers, amber
//! tickers, green/red moves).
//!
//! Colours come from the current [`Palette`], selected each frame from the system
//! appearance and accent colour; accessors like [`amber`] read it.

use std::cell::Cell;

use crate::prefs::{MonoFont, UiFont};
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke, TextStyle, Theme, Visuals};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Window background behind cards (shows vibrancy on macOS when translucent).
    pub window: Color32,
    /// Sidebar fill; translucent so the window's vibrancy shows through on macOS.
    pub sidebar: Color32,
    /// Card (pane) background — data lives here.
    pub card: Color32,
    pub card_header: Color32,
    pub border: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub amber: Color32,
    pub amber_dim: Color32,
    pub up: Color32,
    pub down: Color32,
    pub candle_up: Color32,
    pub candle_down: Color32,
    pub cyan: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub accent: Color32,
    pub series: [Color32; 6],
    /// Colour roles: one meaning per channel (see `docs/COLOUR_ROLES.md`).
    pub roles: Roles,
}

/// What a colour means. Each role belongs to exactly one [`Channel`]; a hue that carries
/// one channel never carries another (regime is never green/red, sign never orange/blue).
// The full vocabulary is kept even where a role has no call site yet; tests and
// `role()` enumerate it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourRole {
    SignPos,
    SignNeg,
    SignZero,
    RegimeUp,
    RegimeDown,
    RegimeFlat,
    ProvVerified,
    ProvCached,
    ProvSynthetic,
    ProvUnverified,
    SigPass,
    SigFail,
    LowN,
    Caution,
    Alert,
    Info,
    Highlight,
    Accent,
    Muted,
}

/// The meaning a colour channel carries.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Signed numbers and candle bodies (theme `up`/`down`).
    Sign,
    /// Model state: TSTACK lanes, candle tint, Stop Cloud, Trend Cone, TWALL.
    Regime,
    /// Where data came from; verified data has no hue.
    Provenance,
    /// Pass/fail/low-n: strength and outline, no hue.
    Significance,
    /// Chrome: caution, alert, formula ink, highlight, accent, muted.
    Chrome,
}

#[allow(dead_code)]
impl ColourRole {
    pub const ALL: [ColourRole; 19] = [
        Self::SignPos,
        Self::SignNeg,
        Self::SignZero,
        Self::RegimeUp,
        Self::RegimeDown,
        Self::RegimeFlat,
        Self::ProvVerified,
        Self::ProvCached,
        Self::ProvSynthetic,
        Self::ProvUnverified,
        Self::SigPass,
        Self::SigFail,
        Self::LowN,
        Self::Caution,
        Self::Alert,
        Self::Info,
        Self::Highlight,
        Self::Accent,
        Self::Muted,
    ];

    pub fn channel(self) -> Channel {
        match self {
            Self::SignPos | Self::SignNeg | Self::SignZero => Channel::Sign,
            Self::RegimeUp | Self::RegimeDown | Self::RegimeFlat => Channel::Regime,
            Self::ProvVerified | Self::ProvCached | Self::ProvSynthetic | Self::ProvUnverified => Channel::Provenance,
            Self::SigPass | Self::SigFail | Self::LowN => Channel::Significance,
            Self::Caution | Self::Alert | Self::Info | Self::Highlight | Self::Accent | Self::Muted => Channel::Chrome,
        }
    }
}

/// Role colours that are not plain theme fields, resolved once per theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roles {
    pub regime_up: Color32,
    pub regime_down: Color32,
    pub regime_flat: Color32,
    /// Provenance rail fill for verified (Real) bars; cached is this at 55%.
    pub rail_verified: Color32,
    pub synthetic: Color32,
    pub alert: Color32,
    /// Diverging ramp, most negative to most positive.
    pub heat_div: [Color32; 5],
    /// Sequential (unsigned magnitude) slate ramp, least to most.
    pub heat_seq: [Color32; 5],
    /// Window-link colours: Red, Orange, Green, Blue, Purple.
    pub link: [Color32; 5],
    /// Opaque back for painter-drawn hover cards.
    pub scrim: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Roles {
    pub fn for_theme(dark: bool, c: &crate::themes::ThemeColors) -> Self {
        let (up, down, flat) =
            if dark { (hex(0x7B93FF), hex(0xFF8232), hex(0x7C7F8A)) } else { (hex(0x3D5BD9), hex(0x914100), hex(0x8A8D96)) };
        Roles {
            regime_up: c.regime_up.map_or(up, |h| h.0),
            regime_down: c.regime_down.map_or(down, |h| h.0),
            regime_flat: flat,
            rail_verified: if dark { hex(0x6B7480) } else { hex(0x9AA1AB) },
            synthetic: if dark { hex(0xD670D6) } else { hex(0xA22BA6) },
            alert: if dark { hex(0xE14A53) } else { hex(0xC4303A) },
            heat_div: if dark {
                [hex(0xA8324A), hex(0x6B3C48), hex(0x3F444D), hex(0x28606C), hex(0x127A8C)]
            } else {
                [hex(0xC2455A), hex(0xE3A6AE), hex(0xE4E6EA), hex(0x9CCFD6), hex(0x1F8FA3)]
            },
            heat_seq: if dark {
                [hex(0x2A2D34), hex(0x3F4550), hex(0x5B6472), hex(0x8792A2), hex(0xC3CCD8)]
            } else {
                [hex(0xE9ECF0), hex(0xCDD3DB), hex(0xA3ACB8), hex(0x6F7A88), hex(0x3C4450)]
            },
            link: if dark {
                [hex(0xFF453A), hex(0xFF9F0A), hex(0x30D158), hex(0x0A84FF), hex(0xBF5AF2)]
            } else {
                [hex(0xD70015), hex(0xC93400), hex(0x248A3D), hex(0x0040DD), hex(0x8944AB)]
            },
            scrim: if dark { hex(0x101010) } else { hex(0xFAFAFA) },
        }
    }
}

impl Palette {
    /// The built-in Terminal Dark palette.
    pub fn dark(accent: Color32) -> Self {
        crate::themes::terminal_dark().palette(accent)
    }
}

/// macOS default (blue) accent, used when the system does not report one.
pub const DEFAULT_ACCENT: Color32 = Color32::from_rgb(10, 132, 255);

thread_local! {
    static CURRENT: Cell<Palette> = Cell::new(Palette::dark(DEFAULT_ACCENT));
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static DENSITY: Cell<f32> = const { Cell::new(1.0) };
    static UI_MONO: Cell<bool> = const { Cell::new(false) };
    static FONTS: Cell<Option<MonoFont>> = const { Cell::new(None) };
}

/// Spacing multiplier from the density preference (compact < 1 < comfortable).
pub fn density() -> f32 {
    DENSITY.with(Cell::get)
}

/// A layout size in points scaled by density.
pub fn dp(points: f32) -> f32 {
    (points * density()).round()
}

pub fn palette() -> Palette {
    CURRENT.with(Cell::get)
}

pub fn set_palette(p: Palette) {
    CURRENT.with(|c| c.set(p));
}

pub fn window() -> Color32 {
    palette().window
}
pub fn sidebar() -> Color32 {
    palette().sidebar
}
pub fn card() -> Color32 {
    palette().card
}
pub fn header() -> Color32 {
    palette().card_header
}
pub fn border() -> Color32 {
    palette().border
}
pub fn grid() -> Color32 {
    palette().grid
}
pub fn text() -> Color32 {
    palette().text
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn amber() -> Color32 {
    palette().amber
}
pub fn amber_dim() -> Color32 {
    palette().amber_dim
}
pub fn up() -> Color32 {
    palette().up
}
pub fn candle_up() -> Color32 {
    palette().candle_up
}

pub fn candle_down() -> Color32 {
    palette().candle_down
}

pub fn down() -> Color32 {
    palette().down
}
pub fn cyan() -> Color32 {
    palette().cyan
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn control() -> Color32 {
    palette().control
}
pub fn control_hover() -> Color32 {
    palette().control_hover
}
pub fn series(i: usize) -> Color32 {
    palette().series[i % 6]
}
/// Selection/highlight wash derived from the accent colour.
pub fn accent_wash() -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), if palette().dark { 70 } else { 45 })
}

/// The colour for a role in the current theme.
pub fn role(r: ColourRole) -> Color32 {
    role_in(&palette(), r)
}

pub fn role_in(p: &Palette, r: ColourRole) -> Color32 {
    use ColourRole::*;
    match r {
        SignPos => p.up,
        SignNeg => p.down,
        SignZero | ProvVerified | SigFail | Muted => p.muted,
        RegimeUp => p.roles.regime_up,
        RegimeDown => p.roles.regime_down,
        RegimeFlat => p.roles.regime_flat,
        ProvCached => p.roles.rail_verified.gamma_multiply(0.55),
        ProvSynthetic => p.roles.synthetic,
        ProvUnverified | Alert => p.roles.alert,
        SigPass => p.text,
        LowN | Caution | Highlight => p.amber,
        Info => p.cyan,
        Accent => p.accent,
    }
}

/// Ink for a signed number: positive, negative, or muted for zero/NaN.
pub fn sign(x: f64) -> Color32 {
    if x > 0.0 {
        role(ColourRole::SignPos)
    } else if x < 0.0 {
        role(ColourRole::SignNeg)
    } else {
        role(ColourRole::SignZero)
    }
}

/// Model-state colour (never green/red): 1 up, −1 down, anything else flat.
pub fn regime(state: i8) -> Color32 {
    match state {
        1.. => role(ColourRole::RegimeUp),
        ..=-1 => role(ColourRole::RegimeDown),
        0 => role(ColourRole::RegimeFlat),
    }
}

/// [`regime`] for filled lanes, where flat is the grid colour.
pub fn regime_lane(state: i8) -> Color32 {
    if state == 0 { grid() } else { regime(state) }
}

/// Text ink for provenance: verified data has no hue; sample and unverified do.
pub fn provenance(p: tr_core::Provenance) -> Color32 {
    role(match p {
        tr_core::Provenance::Real => ColourRole::ProvVerified,
        tr_core::Provenance::Cached => ColourRole::ProvVerified,
        tr_core::Provenance::Sample => ColourRole::ProvSynthetic,
        tr_core::Provenance::Missing => ColourRole::ProvUnverified,
    })
}

/// Provenance rail fill (exceptions are additionally hatched by the caller).
pub fn rail(p: tr_core::Provenance) -> Color32 {
    match p {
        tr_core::Provenance::Real => palette().roles.rail_verified,
        tr_core::Provenance::Cached => role(ColourRole::ProvCached),
        tr_core::Provenance::Sample => role(ColourRole::ProvSynthetic),
        tr_core::Provenance::Missing => role(ColourRole::ProvUnverified),
    }
}

/// Caution (amber): text or outline only, never an area fill for state.
pub fn caution() -> Color32 {
    role(ColourRole::Caution)
}
/// Errors and unavailable data (chrome with words).
pub fn alert() -> Color32 {
    role(ColourRole::Alert)
}
/// Formula ink.
pub fn info() -> Color32 {
    role(ColourRole::Info)
}

pub fn link(l: crate::layouts::Link) -> Color32 {
    use crate::layouts::Link;
    let i = match l {
        Link::Red => 0,
        Link::Orange => 1,
        Link::Green => 2,
        Link::Blue => 3,
        Link::Purple => 4,
    };
    palette().roles.link[i]
}

/// Diverging heat step 0..5 (0 most negative, 2 neutral, 4 most positive).
pub fn heat_div(step: usize) -> Color32 {
    palette().roles.heat_div[step.min(4)]
}

/// Sequential (unsigned) heat step 0..5, least to most.
pub fn heat_seq(step: usize) -> Color32 {
    palette().roles.heat_seq[step.min(4)]
}

/// Readable ink on a filled cell: black or white, whichever contrasts more.
pub fn on_fill(fill: Color32) -> Color32 {
    let black = Color32::BLACK;
    let white = Color32::WHITE;
    if contrast(black, fill) >= contrast(white, fill) { black } else { white }
}

/// Opaque back for painter-drawn hover cards (follows light/dark).
pub fn scrim() -> Color32 {
    palette().roles.scrim
}

/// WCAG relative luminance of an opaque sRGB colour.
pub fn luminance(c: Color32) -> f32 {
    let lin = |u: u8| {
        let v = u as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio (1–21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

pub fn dem() -> Color32 {
    if palette().dark { Color32::from_rgb(64, 156, 255) } else { Color32::from_rgb(0, 102, 214) }
}
pub fn rep() -> Color32 {
    if palette().dark { Color32::from_rgb(255, 82, 82) } else { Color32::from_rgb(200, 30, 30) }
}
pub fn ind() -> Color32 {
    if palette().dark { Color32::from_rgb(191, 90, 242) } else { Color32::from_rgb(140, 50, 200) }
}

pub fn party_color(p: tr_congress::Party) -> Color32 {
    match p {
        tr_congress::Party::Democrat => dem(),
        tr_congress::Party::Republican => rep(),
        tr_congress::Party::Independent => ind(),
        tr_congress::Party::Unknown => muted(),
    }
}

pub fn rgba(c: Color32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
}

/// Numbers, tickers and tables (SF Mono on macOS).
pub fn mono(size: f32) -> FontId {
    FontId::new(size * SCALE.with(Cell::get), FontFamily::Monospace)
}

/// Chrome: titles, buttons, labels (SF Pro on macOS).
pub fn ui_font(size: f32) -> FontId {
    let family = if UI_MONO.with(Cell::get) { FontFamily::Monospace } else { FontFamily::Proportional };
    FontId::new(size * SCALE.with(Cell::get), family)
}

/// Corner radii for cards and controls.
pub const CARD_RADIUS: u8 = 10;
pub const CONTROL_RADIUS: u8 = 6;

/// Use the system UI fonts when present (read from the OS at runtime, not bundled).
fn install_fonts(ctx: &egui::Context, mono: MonoFont) {
    if FONTS.with(Cell::get) == Some(mono) {
        return;
    }
    FONTS.with(|f| f.set(Some(mono)));
    let mut fonts = FontDefinitions::default();
    let ui_paths: &[&str] = &[
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFNSText.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ];
    let candidates: &[(&str, FontFamily, &[&str])] =
        &[("system-ui", FontFamily::Proportional, ui_paths), ("system-mono", FontFamily::Monospace, mono.paths())];
    for (name, family, paths) in candidates {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert((*name).into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            fonts.families.entry(family.clone()).or_default().insert(0, (*name).into());
        }
    }
    ctx.set_fonts(fonts);
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.card;
    v.extreme_bg_color = p.card;
    v.faint_bg_color = p.card_header;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_shadow =
        Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow =
        Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if p.dark { 100 } else { 35 }) };
    v.selection.bg_fill = accent_wash_for(p);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_fill = p.control;
    v.widgets.inactive.weak_bg_fill = p.control;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_fill = p.control_hover;
    v.widgets.hovered.weak_bg_fill = p.control_hover;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.bg_fill = accent_wash_for(p);
    v.widgets.active.weak_bg_fill = accent_wash_for(p);
    v.window_corner_radius = CARD_RADIUS.into();
    v.menu_corner_radius = 8.into();
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CONTROL_RADIUS.into();
    }
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v
}

fn accent_wash_for(p: &Palette) -> Color32 {
    Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), if p.dark { 70 } else { 45 })
}

/// One-time setup: fonts and text styles. Visuals are refreshed by [`refresh`].
/// Apply a theme's fonts, text scale and density, and its colours. `system_accent` is
/// used when the theme does not set an accent. Call when the theme changes.
pub fn configure(ctx: &egui::Context, t: &crate::themes::Theme, system_accent: Color32) {
    install_fonts(ctx, t.fonts.mono);
    SCALE.with(|c| c.set(t.fonts.scale.clamp(0.7, 1.6)));
    DENSITY.with(|c| c.set(t.density.factor()));
    UI_MONO.with(|c| c.set(t.fonts.ui == UiFont::Monospace));
    let d = density();
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, ui_font(10.5)),
            (TextStyle::Body, ui_font(13.0)),
            (TextStyle::Button, ui_font(13.0)),
            (TextStyle::Heading, ui_font(16.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0 * d, 3.0 * d);
        s.spacing.button_padding = egui::vec2(8.0 * d, 3.0 * d);
        s.spacing.interact_size.y = 20.0 * d;
        s.spacing.menu_margin = egui::Margin::same((6.0 * d) as i8);
        s.animation_time = 0.18;
    });
    // egui's own widgets follow the theme's light/dark base.
    ctx.set_theme(if t.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    // Force visuals to be rebuilt with the new colours.
    set_palette(Palette { accent: Color32::TRANSPARENT, ..palette() });
    refresh(ctx, t, system_accent);
}

/// Keep the palette in step with the theme and system accent; cheap, call every frame.
pub fn refresh(ctx: &egui::Context, t: &crate::themes::Theme, system_accent: Color32) {
    let p = t.palette(system_accent);
    if p != palette() {
        let v = visuals(&p);
        ctx.set_visuals_of(Theme::Dark, v.clone());
        ctx.set_visuals_of(Theme::Light, v);
    }
    set_palette(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_are_readable() {
        for p in [Palette::dark(DEFAULT_ACCENT), crate::themes::terminal_light().palette(DEFAULT_ACCENT)] {
            let lum = |c: Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            // Body text contrasts strongly with the card it sits on.
            assert!((lum(p.text) - lum(p.card)).abs() > 150.0, "{}", p.dark);
            assert!((lum(p.muted) - lum(p.card)).abs() > 60.0);
        }
        set_palette(crate::themes::terminal_light().palette(Color32::RED));
        assert_eq!(accent(), Color32::RED);
        assert!(!palette().dark);
    }

    fn linear(u: u8) -> f32 {
        let v = u as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }

    /// OKLab (Ottosson 2020) from linear-light RGB.
    fn oklab_lin([r, g, b]: [f32; 3]) -> [f32; 3] {
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        [
            0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        ]
    }

    fn lin(c: Color32) -> [f32; 3] {
        [linear(c.r()), linear(c.g()), linear(c.b())]
    }

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn ok_dist(a: Color32, b: Color32) -> f32 {
        dist(oklab_lin(lin(a)), oklab_lin(lin(b)))
    }

    /// Machado, Oliveira & Fernandes (2009), severity 1.0, applied to linear RGB.
    const PROTAN: [[f32; 3]; 3] =
        [[0.152_286, 1.052_583, -0.204_868], [0.114_503, 0.786_281, 0.099_216], [-0.003_882, -0.048_116, 1.051_998]];
    const DEUTAN: [[f32; 3]; 3] =
        [[0.367_322, 0.860_646, -0.227_968], [0.280_085, 0.672_501, 0.047_413], [-0.011_820, 0.042_940, 0.968_881]];

    fn cvd(m: &[[f32; 3]; 3], c: Color32) -> [f32; 3] {
        let x = lin(c);
        let row = |r: [f32; 3]| (r[0] * x[0] + r[1] * x[1] + r[2] * x[2]).clamp(0.0, 1.0);
        oklab_lin([row(m[0]), row(m[1]), row(m[2])])
    }

    fn palettes() -> Vec<(String, Palette)> {
        crate::themes::builtins().into_iter().map(|t| (t.name.clone(), t.palette(DEFAULT_ACCENT))).collect()
    }

    #[test]
    fn roles_are_distinct_within_data_channels() {
        use ColourRole::*;
        let set = [RegimeUp, RegimeDown, SignPos, SignNeg, ProvSynthetic, Alert, Caution];
        let mut bad = Vec::new();
        for (name, p) in palettes() {
            for (i, &a) in set.iter().enumerate() {
                for &b in &set[i + 1..] {
                    if matches!((a, b), (SignNeg, Alert) | (Alert, SignNeg)) {
                        continue; // both red; alert is chrome that always carries words
                    }
                    if DISTINCT_EXEMPT.contains(&(name.as_str(), a, b)) {
                        continue;
                    }
                    let d = ok_dist(role_in(&p, a), role_in(&p, b));
                    if d < 0.10 {
                        bad.push(format!("{name}: {a:?} vs {b:?} OKLab distance {d:.3}"));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Theme-owned colours that are canonical to a published scheme and sit just under
    /// 3:1; they are always paired with a printed sign or word.
    const CONTRAST_EXEMPT: &[(&str, ColourRole)] =
        &[("Solarized Light", ColourRole::SignPos), ("Solarized Light", ColourRole::Info)];

    /// Pairs of theme-owned colours from a published scheme (Solarized green vs
    /// yellow). Caution is text-only chrome with words, so the pair never meets as fills.
    const DISTINCT_EXEMPT: &[(&str, ColourRole, ColourRole)] =
        &[("Solarized Dark", ColourRole::SignPos, ColourRole::Caution)];

    #[test]
    fn roles_are_readable_on_card() {
        use ColourRole::*;
        let mut bad = Vec::new();
        for (name, p) in palettes() {
            for r in [RegimeUp, RegimeDown, SignPos, SignNeg, ProvSynthetic, Alert, Caution, Info, SigPass] {
                let c = contrast(role_in(&p, r), p.card);
                if c < 3.0 && !CONTRAST_EXEMPT.contains(&(name.as_str(), r)) {
                    bad.push(format!("{name}: {r:?} contrast {c:.2} on card"));
                }
            }
            for fill in p.roles.heat_div.iter().chain(&p.roles.heat_seq) {
                let c = contrast(on_fill(*fill), *fill);
                if c < 4.5 {
                    bad.push(format!("{name}: ink on {fill:?} contrast {c:.2}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn cvd_matrices_preserve_white() {
        for m in [PROTAN, DEUTAN] {
            for row in m {
                assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{row:?}");
            }
        }
    }

    #[test]
    fn regime_and_heat_poles_survive_cvd() {
        for (name, p) in palettes() {
            let pairs = [
                ("regime", p.roles.regime_up, p.roles.regime_down),
                ("diverging ends", p.roles.heat_div[0], p.roles.heat_div[4]),
            ];
            for (what, a, b) in pairs {
                for (kind, m) in [("protan", &PROTAN), ("deutan", &DEUTAN)] {
                    let d = dist(cvd(m, a), cvd(m, b));
                    assert!(d >= 0.08, "{name}: {what} under {kind} distance {d:.3}");
                }
            }
        }
    }

    #[test]
    fn regime_never_equals_sign() {
        use ColourRole::*;
        let mut bad = Vec::new();
        for (name, p) in palettes() {
            for r in [RegimeUp, RegimeDown, RegimeFlat] {
                for s in [SignPos, SignNeg] {
                    let d = ok_dist(role_in(&p, r), role_in(&p, s));
                    if d < 0.10 {
                        bad.push(format!("{name}: {r:?} ~ {s:?} ({d:.3})"));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        set_palette(Palette::dark(DEFAULT_ACCENT));
        assert_eq!(regime(1), role(ColourRole::RegimeUp));
        assert_eq!(regime(-3), role(ColourRole::RegimeDown));
        assert_eq!(regime(0), role(ColourRole::RegimeFlat));
        assert_eq!(regime_lane(0), grid());
        assert_eq!(sign(f64::NAN), muted());
        assert_eq!(sign(0.0), muted());
        assert_eq!(sign(-0.1), down());
        assert_eq!(sign(2.0), up());
        assert_eq!(provenance(tr_core::Provenance::Real), muted());
        assert_eq!(provenance(tr_core::Provenance::Cached), muted());
        assert_eq!(provenance(tr_core::Provenance::Missing), alert());
        assert_ne!(provenance(tr_core::Provenance::Sample), alert());
        assert!(ColourRole::ALL.iter().any(|r| r.channel() == Channel::Significance));
    }

    #[test]
    fn heat_ramps_are_monotone() {
        for (name, p) in palettes() {
            let l = |c: Color32| oklab_lin(lin(c))[0];
            let seq: Vec<f32> = p.roles.heat_seq.iter().map(|&c| l(c)).collect();
            let card = l(p.card);
            // Sequential steps move strictly away from the card.
            for w in seq.windows(2) {
                assert!((w[1] - card).abs() > (w[0] - card).abs(), "{name}: sequential not monotone {seq:?}");
            }
            // Diverging: the further from neutral, the further from the card.
            let div: Vec<f32> = p.roles.heat_div.iter().map(|&c| (l(c) - card).abs()).collect();
            assert!(div[0] > div[1] && div[1] > div[2], "{name}: diverging negative arm {div:?}");
            assert!(div[4] > div[3] && div[3] > div[2], "{name}: diverging positive arm {div:?}");
        }
    }

    #[test]
    fn theme_without_regime_fields_loads_and_round_trips() {
        let t = crate::themes::Theme::parse("name = \"Old\"\n[colors]\nup = \"#00aa66\"\n", false).unwrap();
        assert_eq!(t.colors.regime_up, None);
        let p = t.palette(DEFAULT_ACCENT);
        assert_eq!(p.roles.regime_up, Roles::for_theme(true, &crate::themes::terminal_dark().colors).regime_up);
        assert!(!t.to_toml().contains("regime_"), "absent regime keys stay absent");
        let custom = crate::themes::Theme::parse(
            "name = \"New\"\n[colors]\nregime_up = \"#112233\"\nregime_down = \"#445566\"\n",
            false,
        )
        .unwrap();
        assert_eq!(custom.palette(DEFAULT_ACCENT).roles.regime_down, Color32::from_rgb(0x44, 0x55, 0x66));
        let back = crate::themes::Theme::parse(&custom.to_toml(), false).unwrap();
        assert_eq!(back, custom);
    }
}

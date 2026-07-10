//! Built-in themes and their application to the UI.
//!
//! Every theme is a complete palette over the semantic roles in
//! `ui/theme.slint`. Users pick a theme but can't override individual
//! colors: the `wcag` test below verifies each palette as a unit
//! (readable-text roles must clear WCAG AA contrast against every surface
//! they appear on), and a single swapped color could silently break that.
//!
//! Colors are `0xAARRGGBB`; only `scrim` and `accent_veil` use the alpha
//! channel, everything else is fully opaque.

/// One palette over the semantic roles; mirrors `ThemePalette` in
/// ui/theme.slint.
#[derive(Debug, Clone, Copy)]
pub struct Pal {
    // Surfaces.
    pub win_bg: u32,
    pub panel_bg: u32,
    pub sidebar_bg: u32,
    pub surface: u32,
    pub inset_bg: u32,
    pub code_bg: u32,
    pub control_bg: u32,
    pub raised_bg: u32,
    pub popup_bg: u32,
    pub pill_bg: u32,
    pub hover_bg: u32,
    pub hover_strong: u32,
    // Chrome.
    pub border: u32,
    pub border_strong: u32,
    pub card_border: u32,
    pub rule: u32,
    pub scroll_thumb: u32,
    pub scroll_thumb_hover: u32,
    pub scroll_thumb_active: u32,
    pub scrim: u32,
    // Text.
    pub text_hi: u32,
    pub text: u32,
    pub text_mid: u32,
    pub text_dim: u32,
    pub text_soft: u32,
    pub text_faint: u32,
    pub text_disabled: u32,
    pub text_ghost: u32,
    pub gutter: u32,
    pub code_fg: u32,
    // Accent.
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_tint: u32,
    pub text_accent_soft: u32,
    pub on_accent: u32,
    pub primary_bg: u32,
    pub primary_hover: u32,
    pub primary_border: u32,
    pub selection: u32,
    pub accent_bg: u32,
    pub accent_dim: u32,
    pub accent_veil: u32,
    pub user_bg: u32,
    pub user_border: u32,
    // Status.
    pub ok: u32,
    pub ok_dim: u32,
    pub agent_bg: u32,
    pub agent_border: u32,
    pub diff_add_bg: u32,
    pub err: u32,
    pub err_soft: u32,
    pub err_bg: u32,
    pub err_hover: u32,
    pub diff_del_bg: u32,
    pub warn: u32,
    pub warn_bg: u32,
    pub warn_border: u32,
}

pub struct ThemeDef {
    /// Stable id persisted in appearance settings.
    pub id: &'static str,
    /// Name shown in the Appearance picker.
    pub name: &'static str,
    /// Drives the std-widgets color scheme and the syntax highlight theme.
    pub dark: bool,
    pub pal: Pal,
}

/// The default (original) look.
const DARK: Pal = Pal {
    win_bg: 0xff141414,
    panel_bg: 0xff181818,
    sidebar_bg: 0xff1a1a1a,
    surface: 0xff1e1e1e,
    inset_bg: 0xff17191c,
    code_bg: 0xff1f2226,
    control_bg: 0xff222222,
    raised_bg: 0xff232323,
    popup_bg: 0xff262626,
    pill_bg: 0xff2a2a2a,
    hover_bg: 0xff2e2e2e,
    hover_strong: 0xff333333,
    border: 0xff3a3a3a,
    border_strong: 0xff454545,
    card_border: 0xff3a3f46,
    rule: 0xff33363b,
    scroll_thumb: 0xff3a3e44,
    scroll_thumb_hover: 0xff565b63,
    scroll_thumb_active: 0xff6a6f78,
    scrim: 0xaa000000,
    text_hi: 0xffe8e8e8,
    text: 0xffd8d8d8,
    text_mid: 0xffc8c8c8,
    text_dim: 0xff9d9d9d,
    text_soft: 0xff949494,
    text_faint: 0xff8a8a8a,
    text_disabled: 0xff6a6a6a,
    text_ghost: 0xff5a5a5a,
    gutter: 0xff828282,
    code_fg: 0xffd8d8c8,
    accent: 0xff6fa8dc,
    accent_hover: 0xffa8ccf0,
    accent_tint: 0xff9cbdde,
    text_accent_soft: 0xffdbe6f5,
    on_accent: 0xffeef3fb,
    primary_bg: 0xff35598f,
    primary_hover: 0xff3f6db3,
    primary_border: 0xff4f7dc3,
    selection: 0xff3d5a80,
    accent_bg: 0xff2b3d55,
    accent_dim: 0xff24344a,
    accent_veil: 0x332b3d55,
    user_bg: 0xff232c3d,
    user_border: 0xff3a4a68,
    ok: 0xff7fd18a,
    ok_dim: 0xff8fae8f,
    agent_bg: 0xff1f2a22,
    agent_border: 0xff33503c,
    diff_add_bg: 0xff1e3a24,
    err: 0xffe39ea6,
    err_soft: 0xfff2c4ca,
    err_bg: 0xff4a2328,
    err_hover: 0xff5d2e35,
    diff_del_bg: 0xff42272b,
    warn: 0xffe5c07b,
    warn_bg: 0xff262117,
    warn_border: 0xff5a4d2e,
};

const LIGHT: Pal = Pal {
    win_bg: 0xfff5f5f5,
    panel_bg: 0xffefefef,
    sidebar_bg: 0xffececec,
    surface: 0xffffffff,
    inset_bg: 0xfff0f2f5,
    code_bg: 0xfff2f4f7,
    control_bg: 0xffe6e6e6,
    raised_bg: 0xffe9e9e9,
    popup_bg: 0xfffcfcfc,
    pill_bg: 0xffe2e2e2,
    hover_bg: 0xffdcdcdc,
    hover_strong: 0xffd4d4d4,
    border: 0xffc4c4c4,
    border_strong: 0xffaaaaaa,
    card_border: 0xffbfc4cc,
    rule: 0xffd0d4da,
    scroll_thumb: 0xffbdc2c9,
    scroll_thumb_hover: 0xff9aa0a8,
    scroll_thumb_active: 0xff7f858e,
    scrim: 0x66000000,
    text_hi: 0xff17181a,
    text: 0xff222325,
    text_mid: 0xff333538,
    text_dim: 0xff53565b,
    text_soft: 0xff5c5f64,
    text_faint: 0xff6b6e73,
    text_disabled: 0xff9a9da2,
    text_ghost: 0xffb0b3b8,
    gutter: 0xff6b6e72,
    code_fg: 0xff2c2e33,
    accent: 0xff1d5e9e,
    accent_hover: 0xff174a7d,
    accent_tint: 0xff2a5f93,
    text_accent_soft: 0xff1e3a5c,
    on_accent: 0xffffffff,
    primary_bg: 0xff2b62a5,
    primary_hover: 0xff245489,
    primary_border: 0xff1d4f89,
    selection: 0xffb3cde8,
    accent_bg: 0xffd6e4f5,
    accent_dim: 0xffe2ecf8,
    accent_veil: 0x33306eae,
    user_bg: 0xffdde7f6,
    user_border: 0xffa9c1e0,
    ok: 0xff1c6b2e,
    ok_dim: 0xff33643e,
    agent_bg: 0xffe4f2e6,
    agent_border: 0xff9cc4a4,
    diff_add_bg: 0xffd9efdc,
    err: 0xffa8323f,
    err_soft: 0xff8c2934,
    err_bg: 0xfffbdfe2,
    err_hover: 0xfff5cdd2,
    diff_del_bg: 0xfff7dcdf,
    warn: 0xff7a5500,
    warn_bg: 0xfffdf3d9,
    warn_border: 0xffd9b45c,
};

/// High-contrast dark: near-black surfaces, near-white text, brighter
/// hues; readable tiers clear AAA (7:1) on the main surfaces.
const HC_DARK: Pal = Pal {
    win_bg: 0xff000000,
    panel_bg: 0xff050505,
    sidebar_bg: 0xff080808,
    surface: 0xff101010,
    inset_bg: 0xff0a0c0e,
    code_bg: 0xff0e1013,
    control_bg: 0xff161616,
    raised_bg: 0xff181818,
    popup_bg: 0xff1a1a1a,
    pill_bg: 0xff202020,
    hover_bg: 0xff2a2a2a,
    hover_strong: 0xff323232,
    border: 0xff6e6e6e,
    border_strong: 0xff8a8a8a,
    card_border: 0xff777d86,
    rule: 0xff5c6066,
    scroll_thumb: 0xff5a5f66,
    scroll_thumb_hover: 0xff7d838c,
    scroll_thumb_active: 0xff9aa1ab,
    scrim: 0xcc000000,
    text_hi: 0xffffffff,
    text: 0xfff2f2f2,
    text_mid: 0xffe4e4e4,
    text_dim: 0xffc0c0c0,
    text_soft: 0xffb8b8b8,
    text_faint: 0xffaaaaaa,
    text_disabled: 0xff8a8a8a,
    text_ghost: 0xff707070,
    gutter: 0xff9a9a9a,
    code_fg: 0xfff2f2e4,
    accent: 0xff8ec1f0,
    accent_hover: 0xffc4e0fa,
    accent_tint: 0xffaacdf0,
    text_accent_soft: 0xffe4eefa,
    on_accent: 0xffffffff,
    primary_bg: 0xff2c5a96,
    primary_hover: 0xff3a6db1,
    primary_border: 0xff6fa3dc,
    selection: 0xff2f5a8e,
    accent_bg: 0xff23364e,
    accent_dim: 0xff1c2c40,
    accent_veil: 0x4d2b3d55,
    user_bg: 0xff1b2536,
    user_border: 0xff5f7ba6,
    ok: 0xff8ee89a,
    ok_dim: 0xffa5c9a5,
    agent_bg: 0xff0f1911,
    agent_border: 0xff4a7a56,
    diff_add_bg: 0xff15301b,
    err: 0xfff2aab2,
    err_soft: 0xfffad2d8,
    err_bg: 0xff3c161b,
    err_hover: 0xff54222a,
    diff_del_bg: 0xff351b1f,
    warn: 0xffefd08c,
    warn_bg: 0xff1c1810,
    warn_border: 0xff7a683e,
};

/// Colorblind-safe dark (deuteranopia/protanopia): success is sky blue and
/// errors are orange (Okabe-Ito hues) so status never hangs on a red/green
/// axis; the general accent stays the neutral blue of the dark theme.
const CB_DARK: Pal = Pal {
    ok: 0xff7cc7ee,
    ok_dim: 0xff8fb0c4,
    agent_bg: 0xff1a2530,
    agent_border: 0xff31506b,
    diff_add_bg: 0xff173349,
    err: 0xfff0aa72,
    err_soft: 0xfff7cba6,
    err_bg: 0xff45301a,
    err_hover: 0xff5c3f21,
    diff_del_bg: 0xff3d2b17,
    warn: 0xffe8d47e,
    warn_bg: 0xff242112,
    warn_border: 0xff6b5d33,
    ..DARK
};

/// Colorblind-safe light: same blue/orange status axis over the light
/// surfaces.
const CB_LIGHT: Pal = Pal {
    ok: 0xff0b5e8a,
    ok_dim: 0xff32586b,
    agent_bg: 0xffdeeef8,
    agent_border: 0xff8fb8d4,
    diff_add_bg: 0xffd7e9f6,
    err: 0xff8f4a00,
    err_soft: 0xff753d00,
    err_bg: 0xfffae5d2,
    err_hover: 0xfff3d6ba,
    diff_del_bg: 0xfff6e2cd,
    warn: 0xff6b5500,
    warn_bg: 0xfffdf3d9,
    warn_border: 0xffd9b45c,
    ..LIGHT
};

pub const THEMES: &[ThemeDef] = &[
    ThemeDef {
        id: "dark",
        name: "Dark",
        dark: true,
        pal: DARK,
    },
    ThemeDef {
        id: "light",
        name: "Light",
        dark: false,
        pal: LIGHT,
    },
    ThemeDef {
        id: "high-contrast-dark",
        name: "High Contrast Dark",
        dark: true,
        pal: HC_DARK,
    },
    ThemeDef {
        id: "colorblind-dark",
        name: "Colorblind Dark",
        dark: true,
        pal: CB_DARK,
    },
    ThemeDef {
        id: "colorblind-light",
        name: "Colorblind Light",
        dark: false,
        pal: CB_LIGHT,
    },
];

/// The theme for a persisted id; unknown ids fall back to Dark.
pub fn by_id(id: &str) -> &'static ThemeDef {
    THEMES.iter().find(|t| t.id == id).unwrap_or(&THEMES[0])
}

/// Base font sizes offered in the Appearance section (px; 13 is the
/// design's native size).
pub const FONT_SIZES: &[u32] = &[11, 12, 13, 14, 15, 16, 18];

/// Push a whole appearance (palette, fonts, motion) into the UI. Must run
/// on the UI thread. The chat/file re-render for baked syntax-highlight
/// colors is the caller's job.
pub fn apply(ui: &crate::AppWindow, appearance: &crate::winstate::Appearance) {
    use slint::ComponentHandle as _;

    let def = by_id(&appearance.theme);
    let p = &def.pal;
    let c = |v: u32| slint::Color::from_argb_encoded(v);

    let theme = ui.global::<crate::Theme>();
    theme.set_c(crate::ThemePalette {
        win_bg: c(p.win_bg),
        panel_bg: c(p.panel_bg),
        sidebar_bg: c(p.sidebar_bg),
        surface: c(p.surface),
        inset_bg: c(p.inset_bg),
        code_bg: c(p.code_bg),
        control_bg: c(p.control_bg),
        raised_bg: c(p.raised_bg),
        popup_bg: c(p.popup_bg),
        pill_bg: c(p.pill_bg),
        hover_bg: c(p.hover_bg),
        hover_strong: c(p.hover_strong),
        border: c(p.border),
        border_strong: c(p.border_strong),
        card_border: c(p.card_border),
        rule: c(p.rule),
        scroll_thumb: c(p.scroll_thumb),
        scroll_thumb_hover: c(p.scroll_thumb_hover),
        scroll_thumb_active: c(p.scroll_thumb_active),
        scrim: c(p.scrim),
        text_hi: c(p.text_hi),
        text: c(p.text),
        text_mid: c(p.text_mid),
        text_dim: c(p.text_dim),
        text_soft: c(p.text_soft),
        text_faint: c(p.text_faint),
        text_disabled: c(p.text_disabled),
        text_ghost: c(p.text_ghost),
        gutter: c(p.gutter),
        code_fg: c(p.code_fg),
        accent: c(p.accent),
        accent_hover: c(p.accent_hover),
        accent_tint: c(p.accent_tint),
        text_accent_soft: c(p.text_accent_soft),
        on_accent: c(p.on_accent),
        primary_bg: c(p.primary_bg),
        primary_hover: c(p.primary_hover),
        primary_border: c(p.primary_border),
        selection: c(p.selection),
        accent_bg: c(p.accent_bg),
        accent_dim: c(p.accent_dim),
        accent_veil: c(p.accent_veil),
        user_bg: c(p.user_bg),
        user_border: c(p.user_border),
        ok: c(p.ok),
        ok_dim: c(p.ok_dim),
        agent_bg: c(p.agent_bg),
        agent_border: c(p.agent_border),
        diff_add_bg: c(p.diff_add_bg),
        err: c(p.err),
        err_soft: c(p.err_soft),
        err_bg: c(p.err_bg),
        err_hover: c(p.err_hover),
        diff_del_bg: c(p.diff_del_bg),
        warn: c(p.warn),
        warn_bg: c(p.warn_bg),
        warn_border: c(p.warn_border),
    });
    theme.set_scale(appearance.font_size as f32 / 13.0);
    theme.set_font_family(appearance.font_family.as_str().into());
    theme.set_reduce_motion(appearance.reduce_motion);

    // Built-in widgets (buttons, combo boxes, scrollbars) follow via the
    // std-widgets Palette.
    ui.invoke_set_widget_scheme(def.dark);

    // Freshly rendered code blocks / inline code pick these up; the caller
    // re-renders existing rows.
    crate::render::set_syntax_dark(def.dark);
    crate::render::set_inline_code_tint(p.warn);
}

/// UI font families offered in the Appearance picker: the system default
/// plus whatever fontconfig reports (deduplicated, sorted); falls back to a
/// small curated list where `fc-list` isn't available (e.g. Windows).
pub fn font_families() -> Vec<String> {
    let mut names: Vec<String> = std::process::Command::new("fc-list")
        .args([":", "family"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                // Lines are "Family[,Localized Alias...]"; keep the first.
                .filter_map(|l| l.split(',').next())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !s.starts_with('.'))
                .collect()
        })
        .unwrap_or_else(|| {
            ["Arial", "Helvetica", "Segoe UI", "Verdana", "Tahoma"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an opaque 0xAARRGGBB color.
    fn luminance(argb: u32) -> f64 {
        let chan = |v: u32| {
            let v = (v & 0xff) as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * chan(argb >> 16) + 0.7152 * chan(argb >> 8) + 0.0722 * chan(argb)
    }

    /// WCAG contrast ratio between two opaque colors (≥ 1.0).
    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Readable text/surface pairings actually used by the UI. Tuples are
    /// (foreground getter, background getter, minimum ratio, label).
    /// 4.5 = WCAG AA for normal text; 3.0 = AA for large text and
    /// graphical objects (icons, badges).
    #[test]
    fn all_themes_meet_wcag_contrast() {
        type Get = fn(&Pal) -> u32;
        // Body-text tiers appear on every general surface.
        let text_tiers: &[(Get, f64, &str)] = &[
            (|p| p.text_hi, 4.5, "text-hi"),
            (|p| p.text, 4.5, "text"),
            (|p| p.text_mid, 4.5, "text-mid"),
            (|p| p.text_dim, 4.5, "text-dim"),
        ];
        let surfaces: &[(Get, &str)] = &[
            (|p| p.win_bg, "win-bg"),
            (|p| p.panel_bg, "panel-bg"),
            (|p| p.sidebar_bg, "sidebar-bg"),
            (|p| p.surface, "surface"),
            (|p| p.inset_bg, "inset-bg"),
            (|p| p.code_bg, "code-bg"),
            (|p| p.control_bg, "control-bg"),
            (|p| p.raised_bg, "raised-bg"),
            (|p| p.popup_bg, "popup-bg"),
            (|p| p.pill_bg, "pill-bg"),
            (|p| p.hover_bg, "hover-bg"),
            (|p| p.hover_strong, "hover-strong"),
        ];
        // Specific pairings from the UI beyond the general text/surface
        // grid: status text on its tinted backgrounds, button labels,
        // links, header text on tinted card headers.
        let extra: &[(Get, Get, f64, &str)] = &[
            // Metadata tier: used on surfaces up to popup-bg (not the
            // hover/pill tints).
            (|p| p.text_soft, |p| p.win_bg, 4.5, "text-soft/win-bg"),
            (|p| p.text_soft, |p| p.surface, 4.5, "text-soft/surface"),
            (|p| p.text_soft, |p| p.code_bg, 4.5, "text-soft/code-bg"),
            (|p| p.text_soft, |p| p.popup_bg, 4.5, "text-soft/popup-bg"),
            // Code text on the code slab.
            (|p| p.code_fg, |p| p.code_bg, 4.5, "code-fg/code-bg"),
            (|p| p.code_fg, |p| p.surface, 4.5, "code-fg/surface"),
            // Links / accent text.
            (|p| p.accent, |p| p.win_bg, 4.5, "accent/win-bg"),
            (|p| p.accent, |p| p.surface, 4.5, "accent/surface"),
            (|p| p.accent, |p| p.code_bg, 4.5, "accent/code-bg"),
            // Primary buttons.
            (
                |p| p.on_accent,
                |p| p.primary_bg,
                4.5,
                "on-accent/primary-bg",
            ),
            (
                |p| p.on_accent,
                |p| p.primary_hover,
                4.5,
                "on-accent/primary-hover",
            ),
            // Selected rows / user card headers.
            (|p| p.text, |p| p.accent_bg, 4.5, "text/accent-bg"),
            (|p| p.text, |p| p.accent_dim, 4.5, "text/accent-dim"),
            (|p| p.text_hi, |p| p.accent_bg, 4.5, "text-hi/accent-bg"),
            (|p| p.text, |p| p.user_bg, 4.5, "text/user-bg"),
            (|p| p.text_mid, |p| p.user_bg, 4.5, "text-mid/user-bg"),
            (
                |p| p.text_accent_soft,
                |p| p.user_bg,
                4.5,
                "text-accent-soft/user-bg",
            ),
            (
                |p| p.text_accent_soft,
                |p| p.win_bg,
                4.5,
                "text-accent-soft/win-bg",
            ),
            (|p| p.text, |p| p.agent_bg, 4.5, "text/agent-bg"),
            // Status text (badges, error banners, question wizard).
            (|p| p.ok, |p| p.surface, 4.5, "ok/surface"),
            (|p| p.ok, |p| p.win_bg, 4.5, "ok/win-bg"),
            (|p| p.ok_dim, |p| p.win_bg, 4.5, "ok-dim/win-bg"),
            (|p| p.err, |p| p.surface, 4.5, "err/surface"),
            (|p| p.err, |p| p.win_bg, 4.5, "err/win-bg"),
            (|p| p.err_soft, |p| p.err_bg, 4.5, "err-soft/err-bg"),
            (|p| p.err_soft, |p| p.err_hover, 4.5, "err-soft/err-hover"),
            (|p| p.warn, |p| p.surface, 4.5, "warn/surface"),
            (|p| p.warn, |p| p.warn_bg, 4.5, "warn/warn-bg"),
            (|p| p.text_hi, |p| p.warn_bg, 4.5, "text-hi/warn-bg"),
            (|p| p.text, |p| p.warn_bg, 4.5, "text/warn-bg"),
            // Diff text on diff row tints.
            (|p| p.text, |p| p.diff_add_bg, 4.5, "text/diff-add-bg"),
            (|p| p.text, |p| p.diff_del_bg, 4.5, "text/diff-del-bg"),
            (|p| p.ok, |p| p.diff_add_bg, 4.5, "ok/diff-add-bg"),
            (|p| p.err, |p| p.diff_del_bg, 4.5, "err/diff-del-bg"),
            // Line-number gutters are graphical/large-text tier.
            (|p| p.gutter, |p| p.surface, 3.0, "gutter/surface"),
            (|p| p.gutter, |p| p.diff_add_bg, 3.0, "gutter/diff-add-bg"),
            (|p| p.gutter, |p| p.diff_del_bg, 3.0, "gutter/diff-del-bg"),
            // Accent glyphs on hovers (icon buttons).
            (|p| p.accent, |p| p.hover_strong, 3.0, "accent/hover-strong"),
            (|p| p.text_hi, |p| p.border, 3.0, "text-hi glyph/border-bg"),
        ];

        let mut failures = Vec::new();
        for theme in THEMES {
            let p = &theme.pal;
            for (fg, min, fg_name) in text_tiers {
                for (bg, bg_name) in surfaces {
                    let ratio = contrast(fg(p), bg(p));
                    if ratio < *min {
                        failures.push(format!(
                            "{}: {fg_name} on {bg_name}: {ratio:.2} < {min}",
                            theme.id
                        ));
                    }
                }
            }
            for (fg, bg, min, label) in extra {
                let ratio = contrast(fg(p), bg(p));
                if ratio < *min {
                    failures.push(format!("{}: {label}: {ratio:.2} < {min}", theme.id));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "WCAG contrast failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn theme_ids_are_unique_and_resolvable() {
        for (i, t) in THEMES.iter().enumerate() {
            assert!(THEMES.iter().skip(i + 1).all(|o| o.id != t.id), "{}", t.id);
            assert_eq!(by_id(t.id).id, t.id);
        }
        assert_eq!(by_id("nonsense").id, "dark");
    }
}

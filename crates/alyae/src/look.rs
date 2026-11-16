//! Colours from the desktop: Infynitae's theme when it runs (`LOOK_*` from
//! `infynitae-edge-apps --print-theme`), Windows' light/dark mode and accent colour on Windows,
//! another Linux desktop's through the desktop portal, else egui's dark look. The layout follows AEAM.

use eframe::egui::{self, Color32, FontId, RichText, Stroke};
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub struct Colors {
    pub card: Color32,
    pub edge: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
}

static COLORS: OnceLock<Colors> = OnceLock::new();

pub fn hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_matches('"').strip_prefix('#')?;
    let v = u32::from_str_radix(s.get(..6)?, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn infynitae() -> Option<Colors> {
    if cfg!(windows) {
        return None;
    }
    let out = std::process::Command::new("infynitae-edge-apps")
        .arg("--print-theme")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("LOOK_{k}=")))
            .and_then(hex)
    };
    let (card, text_c, accent) = (get("CARD")?, get("TEXT")?, get("ACCENT")?);
    Some(Colors {
        card,
        edge: get("EDGE").unwrap_or(card.lerp_to_gamma(text_c, 0.2)),
        text: text_c,
        muted: get("MUTED").unwrap_or(card.lerp_to_gamma(text_c, 0.6)),
        accent,
    })
}

/// A DWORD under HKEY_CURRENT_USER.
#[cfg(windows)]
fn reg_dword(key: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (key, value) = (w(key), w(value));
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: NUL-terminated wide strings and a 4-byte buffer for a DWORD
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (r == 0).then_some(data)
}

/// Windows' app mode (light or dark) and accent colour.
#[cfg(windows)]
fn windows() -> Option<Colors> {
    let light = reg_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .is_some_and(|v| v != 0);
    let v = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    // AccentColor is 0xAABBGGRR
    let accent = reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor")
        .map(|c| Color32::from_rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8))
        .unwrap_or(v.selection.bg_fill);
    Some(Colors {
        card: v.panel_fill,
        edge: v.widgets.noninteractive.bg_stroke.color,
        text: v.text_color(),
        muted: v.weak_text_color(),
        accent,
    })
}

#[cfg(not(windows))]
fn windows() -> Option<Colors> {
    None
}

/// One `org.freedesktop.appearance` value from the desktop portal, as busctl prints it
/// (`v u 1`, `v (ddd) 0.8 0.44 0.66`): the words after the type.
#[cfg(not(windows))]
fn portal_value(key: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("busctl")
        .args([
            "--user",
            "--timeout=1",
            "call",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "ReadOne",
            "ss",
            "org.freedesktop.appearance",
            key,
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.split_whitespace().skip(2).map(str::to_owned).collect())
}

/// Any other Linux desktop (GNOME, KDE, ...): its light or dark preference and accent colour
/// through the desktop portal, as Windows' come from the registry.
#[cfg(not(windows))]
fn portal() -> Option<Colors> {
    let scheme = portal_value("color-scheme")?;
    let accent = portal_value("accent-color");
    match portal_from(&scheme, accent.as_deref()) {
        (Some(_), colors) => Some(colors),
        (None, _) => None,
    }
}

/// The colours from the portal's words: `color-scheme` 1 is dark, 2 light, 0 no preference
/// (`None`, egui's dark look is used then); `accent-color` three 0-1 floats, out of range = none.
#[cfg(not(windows))]
fn portal_from(scheme: &[String], accent: Option<&[String]>) -> (Option<bool>, Colors) {
    let light = match scheme.first().map(String::as_str) {
        Some("2") => Some(true),
        Some("1") => Some(false),
        _ => None,
    };
    let v = if light == Some(true) {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    let accent = accent
        .map(|w| {
            w.iter()
                .map(|x| x.parse::<f64>().ok().filter(|f| (0.0..=1.0).contains(f)))
                .collect::<Option<Vec<_>>>()
        })
        .and_then(|c| c.filter(|c| c.len() == 3))
        .map(|c| {
            Color32::from_rgb(
                (c[0] * 255.0).round() as u8,
                (c[1] * 255.0).round() as u8,
                (c[2] * 255.0).round() as u8,
            )
        })
        .unwrap_or(v.selection.bg_fill);
    (
        light,
        Colors {
            card: v.panel_fill,
            edge: v.widgets.noninteractive.bg_stroke.color,
            text: v.text_color(),
            muted: v.weak_text_color(),
            accent,
        },
    )
}

#[cfg(windows)]
fn portal() -> Option<Colors> {
    None
}

/// The desktop's colours, or egui's own dark look's where the desktop gives none.
pub fn colors() -> Colors {
    *COLORS.get_or_init(|| {
        infynitae()
            .or_else(windows)
            .or_else(portal)
            .unwrap_or_else(|| {
                let v = egui::Visuals::dark();
                Colors {
                    card: v.panel_fill,
                    edge: v.widgets.noninteractive.bg_stroke.color,
                    text: v.text_color(),
                    muted: v.weak_text_color(),
                    accent: v.selection.bg_fill,
                }
            })
    })
}

pub fn apply(ctx: &egui::Context) {
    let c = colors();
    let dark = c.card.r() as u32 + c.card.g() as u32 + c.card.b() as u32 <= 384;
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let lift = |k: f32| c.card.lerp_to_gamma(c.text, k);
    v.panel_fill = c.card;
    v.window_fill = c.card;
    v.window_stroke = Stroke::new(1.0, c.edge);
    v.extreme_bg_color = lift(0.04);
    v.faint_bg_color = lift(0.025);
    v.override_text_color = Some(c.text);
    v.weak_text_color = Some(c.muted);
    v.hyperlink_color = c.accent;
    v.selection.bg_fill = c.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, c.text);
    v.widgets.noninteractive.bg_stroke.color = c.edge;
    for (w, k) in [
        (&mut v.widgets.inactive, 0.08),
        (&mut v.widgets.hovered, 0.14),
    ] {
        w.bg_fill = lift(k);
        w.weak_bg_fill = lift(k);
    }
    v.widgets.active.bg_fill = c.accent.gamma_multiply(0.45);
    v.widgets.active.weak_bg_fill = c.accent.gamma_multiply(0.45);
    ctx.set_visuals(v);
    // labels are not text to select: rows and cards take the click, Copy buttons copy
    ctx.global_style_mut(|style| style.interaction.selectable_labels = false);
}

/// The big button (AEAM's Join Server), in the accent.
pub fn primary(label: &str) -> egui::Button<'static> {
    let c = colors();
    egui::Button::new(RichText::new(label.to_string()).strong().color(c.card)).fill(c.accent)
}

/// A small label, as AEAM's "GROUP:".
pub fn tag(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .color(colors().muted)
        .font(FontId::proportional(11.0))
        .strong()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn portal_scheme_and_accent_are_read() {
        let (light, c) = portal_from(&words("2"), Some(&words("0.8 0.439216 0.662745")));
        assert_eq!(light, Some(true));
        assert_eq!(c.accent, Color32::from_rgb(204, 112, 169));
        assert_eq!(c.card, egui::Visuals::light().panel_fill);
        let (light, c) = portal_from(&words("1"), Some(&words("2 0 0")));
        assert_eq!(light, Some(false));
        assert_eq!(c.accent, egui::Visuals::dark().selection.bg_fill);
        assert_eq!(portal_from(&words("0"), None).0, None);
    }
}

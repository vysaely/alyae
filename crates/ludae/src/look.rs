//! Colours from the desktop: Infynitae's theme when it runs (`LOOK_*` from
//! `infynitae-edge-apps --print-theme`), else egui's dark look. The layout follows AEAM.

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

fn hex(s: &str) -> Option<Color32> {
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

/// The desktop's colours, or egui's own dark look's where the desktop gives none.
pub fn colors() -> Colors {
    *COLORS.get_or_init(|| {
        infynitae().unwrap_or_else(|| {
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

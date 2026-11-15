//! Colours from the desktop: Infynitae's theme when it runs (`LOOK_*` from
//! `infynitae-edge-apps --print-theme`), else egui's dark or light look.

use eframe::egui::{self, Color32};

fn hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_matches('"').strip_prefix('#')?;
    let v = u32::from_str_radix(s.get(..6)?, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn infynitae() -> Option<std::collections::HashMap<String, Color32>> {
    if cfg!(windows) {
        return None;
    }
    let out = std::process::Command::new("infynitae-edge-apps").arg("--print-theme").output().ok()?;
    let map: std::collections::HashMap<String, Color32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter_map(|(k, v)| Some((k.strip_prefix("LOOK_")?.to_string(), hex(v)?)))
        .collect();
    (!map.is_empty()).then_some(map)
}

pub fn apply(ctx: &egui::Context) {
    let Some(t) = infynitae() else { return };
    let (Some(&card), Some(&text), Some(&accent)) = (t.get("CARD"), t.get("TEXT"), t.get("ACCENT")) else { return };
    let edge = t.get("EDGE").copied().unwrap_or(text.gamma_multiply(0.2));
    let muted = t.get("MUTED").copied().unwrap_or(text.gamma_multiply(0.6));
    let dark = card.r() as u32 + card.g() as u32 + card.b() as u32 <= 384;
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let lift = |c: Color32, k: f32| c.lerp_to_gamma(text, k);
    v.panel_fill = card;
    v.window_fill = card;
    v.extreme_bg_color = lift(card, 0.04);
    v.faint_bg_color = lift(card, 0.03);
    v.override_text_color = Some(text);
    v.weak_text_color = Some(muted);
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent.gamma_multiply(0.35);
    v.selection.stroke.color = accent;
    v.window_stroke.color = edge;
    v.widgets.noninteractive.bg_stroke.color = edge;
    v.widgets.inactive.weak_bg_fill = lift(card, 0.08);
    v.widgets.inactive.bg_fill = lift(card, 0.08);
    v.widgets.hovered.weak_bg_fill = lift(card, 0.14);
    v.widgets.hovered.bg_fill = lift(card, 0.14);
    v.widgets.active.weak_bg_fill = accent.gamma_multiply(0.45);
    v.widgets.active.bg_fill = accent.gamma_multiply(0.45);
    ctx.set_visuals(v);
}

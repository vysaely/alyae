//! The window: pick a game, then its accounts (a list on the left, the chosen one on the right).

use crate::module::{self, Event, Module, Running};
use crate::store::{self, Account, Store, Values};
use crate::totp;
use eframe::egui::{self, Color32, RichText};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub struct App {
    store: Option<Store>,
    fatal: Option<String>,
    dir: PathBuf,
    modules: Vec<Module>,
    problems: Vec<String>,
    accounts: Vec<Account>,
    /// the game chosen on the first screen
    game: Option<String>,
    selected: BTreeSet<String>,
    /// the account last clicked: the one shown and edited
    focus: Option<String>,
    draft: Option<Account>,
    draft_new: bool,
    search: String,
    shown_secrets: HashSet<(String, String)>,
    /// the action inputs (place, server ...) per game
    inputs: HashMap<String, Values>,
    running: HashMap<String, Running>,
    /// short helper actions (Refresh ...): not "running", only waited for
    tasks: Vec<(String, Running)>,
    status: HashMap<String, (String, bool)>,
    message: Option<(String, bool)>,
    confirm_delete: bool,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    ctx: Option<egui::Context>,
}

const NO_GROUP: &str = "Ungrouped";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opened: Result<Store, String>, dir: PathBuf) -> Self {
        crate::look::apply(&cc.egui_ctx);
        let (modules, problems) = module::load_all(&crate::paths::module_dirs());
        let (tx, rx) = channel();
        let mut app = Self {
            store: None,
            fatal: None,
            dir,
            modules,
            problems,
            accounts: Vec::new(),
            game: None,
            selected: BTreeSet::new(),
            focus: None,
            draft: None,
            draft_new: false,
            search: String::new(),
            shown_secrets: HashSet::new(),
            inputs: HashMap::new(),
            running: HashMap::new(),
            tasks: Vec::new(),
            status: HashMap::new(),
            message: None,
            confirm_delete: false,
            tx,
            rx,
            ctx: Some(cc.egui_ctx.clone()),
        };
        match opened.and_then(|s| s.list().map(|l| (s, l))) {
            Ok((s, list)) => {
                app.accounts = list;
                app.game = s.setting("last_game").filter(|g| app.modules.iter().any(|m| &m.id == g));
                if let Some(g) = &app.game {
                    if let Some(v) = s.setting(&format!("inputs.{g}")) {
                        app.inputs.insert(g.clone(), serde_json::from_str(&v).unwrap_or_default());
                    }
                }
                app.store = Some(s);
            }
            Err(e) => app.fatal = Some(e),
        }
        app
    }

    fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id == id)
    }

    fn say(&mut self, text: impl Into<String>, bad: bool) {
        self.message = Some((text.into(), bad));
    }

    fn title(&self, a: &Account) -> String {
        if !a.name.trim().is_empty() {
            return a.name.clone();
        }
        let t = self.module(&a.game).map(|m| a.value(&m.title_field).to_string()).unwrap_or_default();
        if t.is_empty() { "(unnamed)".into() } else { t }
    }

    fn save(&mut self, a: &Account) -> bool {
        let Some(s) = &self.store else { return false };
        match s.save(a) {
            Ok(()) => {
                match self.accounts.iter_mut().find(|x| x.id == a.id) {
                    Some(x) => *x = a.clone(),
                    None => self.accounts.push(a.clone()),
                }
                true
            }
            Err(e) => {
                self.say(format!("Not saved: {e}"), true);
                false
            }
        }
    }

    fn choose_game(&mut self, id: Option<String>) {
        if let (Some(s), Some(g)) = (&self.store, &id) {
            s.set_setting("last_game", g);
            if !self.inputs.contains_key(g) {
                if let Some(v) = s.setting(&format!("inputs.{g}")) {
                    self.inputs.insert(g.clone(), serde_json::from_str(&v).unwrap_or_default());
                }
            }
        }
        self.game = id;
        self.selected.clear();
        self.focus = None;
        self.draft = None;
        self.search.clear();
    }

    fn import_aeam(&mut self) {
        let Some(s) = &self.store else { return };
        let result = crate::aeam::import(s, &self.accounts).and_then(|n| s.list().map(|l| (n, l)));
        match result {
            Ok(((added, skipped), list)) => {
                self.accounts = list;
                let mut text = format!("Brought in {added} Roblox accounts from AEAM");
                if skipped > 0 {
                    text += &format!(" ({skipped} were here already)");
                }
                self.say(text, false);
            }
            Err(e) => self.say(format!("AEAM accounts not brought in: {e}"), true),
        }
    }

    fn focus_on(&mut self, id: &str) {
        self.focus = Some(id.to_string());
        self.draft = self.accounts.iter().find(|a| a.id == id).cloned();
        self.draft_new = false;
        self.confirm_delete = false;
    }

    // --- events from running actions -------------------------------------------------------

    fn poll(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Set { account, values } => {
                    let Some(mut a) = self.accounts.iter().find(|a| a.id == account).cloned() else { continue };
                    let secret: Vec<bool> =
                        values.keys().map(|k| self.module(&a.game).is_some_and(|m| m.is_secret(k))).collect();
                    for ((k, v), secret) in values.into_iter().zip(secret) {
                        if k == "name" {
                            a.name = v;
                            continue;
                        }
                        let (to, from) = if secret { (&mut a.secrets, &mut a.fields) } else { (&mut a.fields, &mut a.secrets) };
                        from.remove(&k);
                        if v.is_empty() { to.remove(&k); } else { to.insert(k, v); }
                    }
                    let clean = self.draft.as_ref().is_some_and(|d| d.id == a.id)
                        && self.accounts.iter().any(|x| Some(x) == self.draft.as_ref());
                    if self.save(&a) && clean {
                        self.draft = Some(a);
                    }
                }
                Event::Status { account, text } => {
                    self.status.insert(account, (text, false));
                }
                Event::Error { account, text } => {
                    let name = self.accounts.iter().find(|a| a.id == account).map(|a| self.title(a)).unwrap_or_default();
                    self.say(format!("{name}: {text}"), true);
                    self.status.insert(account, (text, true));
                }
                Event::Ended { account, action, ok } => {
                    if self.running.get(&account).is_some_and(|r| r.action == action) {
                        if let Some(mut r) = self.running.remove(&account) {
                            let _ = r.child.wait();
                        }
                    } else if let Some(i) = self.tasks.iter().position(|(a, r)| *a == account && r.action == action) {
                        let _ = self.tasks.swap_remove(i).1.child.wait();
                    }
                    if ok && self.status.get(&account).is_some_and(|(_, bad)| !bad) {
                        self.status.remove(&account);
                    }
                }
            }
        }
        // a plain command has no output to end on: look at the process itself
        let ended: Vec<String> = self
            .running
            .iter_mut()
            .filter(|(_, r)| r.is_command())
            .filter_map(|(id, r)| matches!(r.child.try_wait(), Ok(Some(_))).then(|| id.clone()))
            .collect();
        for id in ended {
            self.running.remove(&id);
        }
    }

    fn run_action(&mut self, action_id: &str, ids: Vec<String>) {
        let Some(game) = self.game.clone() else { return };
        let Some(m) = self.module(&game).cloned() else { return };
        let Some(action) = m.actions.iter().find(|a| a.id == action_id).cloned() else { return };
        let inputs = self.inputs.get(&game).cloned().unwrap_or_default();
        if let Some(s) = &self.store {
            s.set_setting(&format!("inputs.{game}"), &serde_json::to_string(&inputs).unwrap_or_default());
        }
        let mut started = 0;
        for id in ids {
            if action.runs && self.running.contains_key(&id) {
                continue;
            }
            let Some(mut a) = self.accounts.iter().find(|a| a.id == id).cloned() else { continue };
            let ctx = self.ctx.clone();
            match module::start(&m, &action, &a, &inputs, &self.dir, self.tx.clone(), move || {
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
            }) {
                Ok(r) => {
                    started += 1;
                    self.status.remove(&id);
                    match r {
                        Some(r) if action.runs => {
                            self.running.insert(id.clone(), r);
                        }
                        Some(r) => self.tasks.push((id.clone(), r)),
                        None => {}
                    }
                    if action.runs {
                        a.last_used = store::now();
                        self.save(&a);
                    }
                }
                Err(e) => {
                    self.say(format!("{}: {e}", self.title(&a)), true);
                }
            }
        }
        if started > 1 {
            self.say(format!("{} started for {started} accounts", action.label), false);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        if let Some(e) = &self.fatal {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Ludae cannot open its accounts");
                ui.label(e);
                ui.label(format!("Folder: {}", self.dir.display()));
            });
            return;
        }
        if !self.running.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        self.bottom_bar(ui);
        match self.game.clone() {
            None => self.game_picker(ui),
            Some(g) => self.game_view(ui, &g),
        }
    }
}

impl App {
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let Some((text, bad)) = self.message.clone() else { return };
        egui::Panel::bottom("message").show(ui, |ui| {
            ui.horizontal(|ui| {
                let color = if bad { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
                ui.label(RichText::new(text).color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        self.message = None;
                    }
                });
            });
        });
    }

    fn game_picker(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("What are you playing?");
            });
            ui.add_space(16.0);
            let mut chosen = None;
            let width = 240.0;
            let per_row = ((ui.available_width() + 12.0) / (width + 12.0)).floor().max(1.0) as usize;
            for row in self.modules.chunks(per_row) {
                ui.horizontal(|ui| {
                    let used = row.len() as f32 * (width + 12.0) - 12.0;
                    ui.add_space(((ui.available_width() - used) / 2.0).max(0.0));
                    for m in row {
                        let count = self.accounts.iter().filter(|a| a.game == m.id).count();
                        let running = self.accounts.iter().filter(|a| a.game == m.id && self.running.contains_key(&a.id)).count();
                        let resp = egui::Frame::group(ui.style())
                            .inner_margin(14.0)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(width - 28.0);
                                    ui.set_min_height(84.0);
                                    ui.label(RichText::new(&m.name).heading());
                                    ui.add(egui::Label::new(RichText::new(&m.description).weak()).wrap());
                                    ui.add_space(4.0);
                                    let mut line = format!("{count} account{}", if count == 1 { "" } else { "s" });
                                    if running > 0 {
                                        line += &format!(" · {running} running");
                                    }
                                    ui.label(line);
                                });
                            })
                            .response
                            .interact(egui::Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            chosen = Some(m.id.clone());
                        }
                    }
                });
                ui.add_space(12.0);
            }
            if self.modules.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label("No game modules found. They live in:");
                    for d in crate::paths::module_dirs() {
                        ui.label(RichText::new(d.display().to_string()).monospace());
                    }
                });
            }
            let aeam_waiting = cfg!(unix)
                && crate::aeam::dir().is_some()
                && self.store.as_ref().is_some_and(|s| s.setting("aeam_imported").is_none());
            if aeam_waiting {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label("Your AEAM accounts can come along: they are copied, AEAM stays as it is.");
                    if ui.button("Bring in AEAM accounts").clicked() {
                        self.import_aeam();
                    }
                });
            }
            for p in &self.problems {
                ui.label(RichText::new(p).color(ui.visuals().error_fg_color));
            }
            if let Some(c) = chosen {
                self.choose_game(Some(c));
            }
        });
    }

    fn game_view(&mut self, ui: &mut egui::Ui, game: &str) {
        let Some(m) = self.module(game).cloned() else {
            self.choose_game(None);
            return;
        };
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("◀ Games").clicked() {
                    self.choose_game(None);
                }
                ui.label(RichText::new(&m.name).strong().size(18.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("+ Add account").clicked() {
                        let a = Account {
                            id: store::new_id(),
                            game: m.id.clone(),
                            created: store::now(),
                            sort: self.accounts.iter().map(|a| a.sort).max().unwrap_or(0) + 1,
                            ..Default::default()
                        };
                        self.focus = Some(a.id.clone());
                        self.selected = [a.id.clone()].into();
                        self.draft = Some(a);
                        self.draft_new = true;
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search").desired_width(180.0));
                });
            });
            self.actions_row(ui, &m);
            ui.add_space(4.0);
        });
        egui::Panel::left("accounts").resizable(true).default_size(260.0).min_size(180.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.account_list(ui, &m));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.editor(ui, &m));
        });
    }

    fn actions_row(&mut self, ui: &mut egui::Ui, m: &Module) {
        if m.actions.is_empty() {
            return;
        }
        let ids: Vec<String> = self.selected.iter().filter(|id| self.accounts.iter().any(|a| &a.id == *id)).cloned().collect();
        let inputs = self.inputs.entry(m.id.clone()).or_default();
        let mut run = None;
        ui.horizontal_wrapped(|ui| {
            for action in &m.actions {
                for input in &action.inputs {
                    let v = inputs.entry(input.key.clone()).or_default();
                    ui.label(&input.label);
                    ui.add(egui::TextEdit::singleline(v).hint_text(&input.hint).desired_width(150.0));
                }
            }
            ui.separator();
            for action in &m.actions {
                let label = if ids.len() > 1 { format!("{} ({})", action.label, ids.len()) } else { action.label.clone() };
                let enabled = !ids.is_empty() && !(self.draft_new && ids.len() == 1);
                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                    run = Some(action.id.clone());
                }
            }
            let mine: Vec<String> = ids.iter().filter(|id| self.running.contains_key(*id)).cloned().collect();
            if !mine.is_empty() && ui.button(if mine.len() > 1 { format!("Stop ({})", mine.len()) } else { "Stop".into() }).clicked() {
                for id in mine {
                    if let Some(r) = self.running.get_mut(&id) {
                        r.stop();
                    }
                }
            }
        });
        if let Some(a) = run {
            self.run_action(&a, ids);
        }
    }

    fn account_list(&mut self, ui: &mut egui::Ui, m: &Module) {
        let q = self.search.to_lowercase();
        let mut shown: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.game == m.id)
            .filter(|a| {
                q.is_empty()
                    || self.title(a).to_lowercase().contains(&q)
                    || a.group.to_lowercase().contains(&q)
                    || a.fields.values().any(|v| v.to_lowercase().contains(&q))
            })
            .collect();
        shown.sort_by(|a, b| {
            let g = |x: &Account| if x.group.is_empty() { NO_GROUP.to_string() } else { x.group.clone() };
            (g(a), a.sort).cmp(&(g(b), b.sort))
        });
        let order: Vec<String> = shown.iter().map(|a| a.id.clone()).collect();
        let rows: Vec<(String, String, String)> = shown
            .iter()
            .map(|a| (a.id.clone(), self.title(a), if a.group.is_empty() { NO_GROUP.into() } else { a.group.clone() }))
            .collect();
        if rows.is_empty() {
            ui.label(RichText::new(if q.is_empty() { "No accounts yet: + Add account" } else { "Nothing matches" }).weak());
        }
        let groups: BTreeSet<&str> = rows.iter().map(|r| r.2.as_str()).collect();
        let mut clicked = None;
        for group in groups {
            let in_group: Vec<&(String, String, String)> = rows.iter().filter(|r| r.2 == group).collect();
            egui::CollapsingHeader::new(format!("{group}  ({})", in_group.len()))
                .id_salt(("group", &m.id, group))
                .default_open(true)
                .show(ui, |ui| {
                    for (id, title, _) in in_group {
                        let running = self.running.contains_key(id);
                        let mut text = RichText::new(title);
                        if running {
                            text = text.strong();
                        }
                        ui.horizontal(|ui| {
                            let dot = if running { ui.visuals().selection.stroke.color } else { Color32::TRANSPARENT };
                            ui.label(RichText::new("●").color(dot).size(10.0));
                            let resp = ui.selectable_label(self.selected.contains(id), text);
                            if let Some((s, bad)) = self.status.get(id) {
                                let c = if *bad { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
                                ui.label(RichText::new(s).small().color(c));
                            }
                            if resp.clicked() {
                                clicked = Some(id.clone());
                            }
                        });
                    }
                });
        }
        if let Some(id) = clicked {
            let mods = ui.input(|i| i.modifiers);
            if mods.command {
                if !self.selected.remove(&id) {
                    self.selected.insert(id.clone());
                }
            } else if mods.shift {
                let from = self.focus.as_ref().and_then(|f| order.iter().position(|x| x == f)).unwrap_or(0);
                let to = order.iter().position(|x| *x == id).unwrap_or(0);
                let (lo, hi) = (from.min(to), from.max(to));
                self.selected.extend(order[lo..=hi].iter().cloned());
            } else {
                self.selected = [id.clone()].into();
            }
            self.focus_on(&id);
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) && !ui.ctx().egui_wants_keyboard_input() {
            self.selected = order.into_iter().collect();
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, m: &Module) {
        if self.selected.len() > 1 {
            ui.label(format!("{} accounts chosen: the buttons above run for all of them.", self.selected.len()));
            ui.label(RichText::new("Ctrl+click adds or removes one, Shift+click a range.").weak());
            return;
        }
        let Some(mut d) = self.draft.take() else {
            ui.label(RichText::new("Choose an account, or add one.").weak());
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut copy = None;
        egui::Grid::new("fields").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            let fallback = self.title(&Account { name: String::new(), ..d.clone() });
            ui.add(egui::TextEdit::singleline(&mut d.name).hint_text(fallback).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Group");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.group).hint_text(NO_GROUP).desired_width(200.0));
                let groups: BTreeSet<String> =
                    self.accounts.iter().filter(|a| a.game == m.id && !a.group.is_empty()).map(|a| a.group.clone()).collect();
                if !groups.is_empty() {
                    egui::ComboBox::from_id_salt("groups").selected_text("").width(24.0).show_ui(ui, |ui| {
                        for g in groups {
                            if ui.selectable_label(d.group == g, &g).clicked() {
                                d.group = g;
                            }
                        }
                    });
                }
            });
            ui.end_row();
            for f in &m.fields {
                ui.label(&f.label);
                let map = if f.secret { &mut d.secrets } else { &mut d.fields };
                let mut v = map.get(&f.key).cloned().unwrap_or_default();
                let key = (d.id.clone(), f.key.clone());
                ui.horizontal(|ui| {
                    if f.kind == "choice" && !f.choices.is_empty() {
                        let shown = if v.is_empty() { f.choices[0].clone() } else { v.clone() };
                        egui::ComboBox::from_id_salt(("choice", &f.key)).selected_text(&shown).show_ui(ui, |ui| {
                            for c in &f.choices {
                                if ui.selectable_label(&shown == c, c).clicked() {
                                    v = c.clone();
                                }
                            }
                        });
                        if !f.hint.is_empty() {
                            ui.label(RichText::new(&f.hint).weak().small());
                        }
                    } else {
                        let show = !f.secret || self.shown_secrets.contains(&key);
                        let w = (ui.available_width() - if f.secret { 120.0 } else { 0.0 }).max(80.0);
                        ui.add(egui::TextEdit::singleline(&mut v).password(!show).hint_text(&f.hint).desired_width(w));
                        if f.secret {
                            if ui.small_button(if show { "Hide" } else { "Show" }).clicked() && !self.shown_secrets.remove(&key) {
                                self.shown_secrets.insert(key);
                            }
                            if ui.add_enabled(!v.is_empty(), egui::Button::new("Copy").small()).clicked() {
                                copy = Some(v.clone());
                            }
                        }
                    }
                });
                if v.is_empty() { map.remove(&f.key); } else { map.insert(f.key.clone(), v.clone()); }
                ui.end_row();
                if f.kind == "totp" && !v.is_empty() {
                    ui.label("");
                    ui.horizontal(|ui| match totp::code(&v, now) {
                        Some(c) => {
                            ui.label(RichText::new(format!("{} {}", &c[..3], &c[3..])).monospace().size(20.0).strong());
                            ui.label(RichText::new(format!("{} s", totp::seconds_left(now))).weak());
                            if ui.small_button("Copy").clicked() {
                                copy = Some(c);
                            }
                            ui.ctx().request_repaint_after(Duration::from_millis(500));
                        }
                        None => {
                            ui.label(RichText::new("not a valid 2FA secret").color(ui.visuals().error_fg_color));
                        }
                    });
                    ui.end_row();
                }
            }
            ui.label("Notes");
            ui.add(egui::TextEdit::multiline(&mut d.notes).desired_rows(3).desired_width(f32::INFINITY));
            ui.end_row();
        });
        if let Some(c) = copy {
            ui.ctx().copy_text(c);
            self.say("Copied", false);
        }
        ui.add_space(12.0);
        let saved = self.accounts.iter().find(|a| a.id == d.id).cloned();
        let dirty = self.draft_new || saved.as_ref() != Some(&d);
        let mut keep = true;
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty, egui::Button::new(if self.draft_new { "Add" } else { "Save" })).clicked()
                || (dirty && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)))
            {
                if self.save(&d) {
                    self.draft_new = false;
                    self.say(format!("Saved {}", self.title(&d)), false);
                }
            }
            if dirty && ui.button(if self.draft_new { "Cancel" } else { "Undo changes" }).clicked() {
                match &saved {
                    Some(s) => d = s.clone(),
                    None => keep = false,
                }
            }
            if !self.draft_new {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.confirm_delete {
                        if ui.button(RichText::new("Delete for good").color(ui.visuals().error_fg_color)).clicked() {
                            if let Some(s) = &self.store {
                                match s.delete(&d.id) {
                                    Ok(()) => {
                                        self.accounts.retain(|a| a.id != d.id);
                                        self.selected.remove(&d.id);
                                        keep = false;
                                    }
                                    Err(e) => self.message = Some((format!("Not deleted: {e}"), true)),
                                }
                            }
                            self.confirm_delete = false;
                        }
                        if ui.button("Keep").clicked() {
                            self.confirm_delete = false;
                        }
                    } else if ui.button("Delete").clicked() {
                        self.confirm_delete = true;
                    }
                });
            }
        });
        if keep {
            self.draft = Some(d);
        } else {
            self.focus = None;
            self.draft_new = false;
        }
    }
}

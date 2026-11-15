//! The window: pick a game, then its accounts (a list on the left, the chosen one on the right).

use crate::module::{self, Event, Module, Running};
use crate::store::{self, Account, Store, Values};
use crate::totp;
use crate::look;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
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
    /// the GROUP picker: None is all groups
    group_filter: Option<String>,
    shown_secrets: HashSet<(String, String)>,
    /// the action inputs (place, server ...) per game
    inputs: HashMap<String, Values>,
    running: HashMap<String, Running>,
    /// short helper actions (Refresh ...): not "running", only waited for
    tasks: Vec<(String, Running)>,
    status: HashMap<String, (String, bool)>,
    message: Option<(String, bool)>,
    confirm_delete: bool,
    /// accounts waiting for "Remove" to be confirmed
    confirm_remove: Vec<String>,
    /// the name typed into "Move to group" in the right-click menu
    new_group: String,
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
            group_filter: None,
            shown_secrets: HashSet::new(),
            inputs: HashMap::new(),
            running: HashMap::new(),
            tasks: Vec::new(),
            status: HashMap::new(),
            message: None,
            confirm_delete: false,
            confirm_remove: Vec::new(),
            new_group: String::new(),
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
        self.group_filter = None;
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
        // only this button's own boxes: a Follow target must not ride along with Join Server
        let all = self.inputs.get(&game).cloned().unwrap_or_default();
        let inputs: Values = action
            .inputs
            .iter()
            .filter_map(|i| all.get(&i.key).filter(|v| !v.trim().is_empty()).map(|v| (i.key.clone(), v.clone())))
            .collect();
        if let Some(s) = &self.store {
            s.set_setting(&format!("inputs.{game}"), &serde_json::to_string(&all).unwrap_or_default());
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
        if !self.running.is_empty() || !self.tasks.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        self.status_bar(ui);
        match self.game.clone() {
            None => self.game_picker(ui),
            Some(g) => self.game_view(ui, &g),
        }
        self.remove_dialog(&ctx);
    }
}

impl App {
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let in_game: Vec<&Account> = self.accounts.iter().filter(|a| self.game.as_ref().is_none_or(|g| &a.game == g)).collect();
        let total = in_game.len();
        let running = in_game.iter().filter(|a| self.running.contains_key(&a.id)).count();
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some((text, bad)) = self.message.clone() {
                    let color = if bad { ui.visuals().error_fg_color } else { look::colors().text };
                    if ui.small_button("✕").clicked() {
                        self.message = None;
                    }
                    ui.label(RichText::new(text).color(color));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("Total: {total}  •  Running: {running}")).weak());
                });
            });
        });
    }

    fn game_picker(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            look::section(ui, "Ludae · game accounts");
            ui.add_space(18.0);
            ui.vertical_centered(|ui| ui.heading("What are you playing?"));
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
        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                look::section(ui, &format!("Ludae · {}", m.name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Games").on_hover_text("Choose another game").clicked() {
                        self.choose_game(None);
                    }
                });
            });
        });
        egui::Panel::right("deck").resizable(true).default_size(320.0).min_size(260.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.deck(ui, &m));
        });
        egui::CentralPanel::default().show(ui, |ui| self.accounts_side(ui, &m));
    }

    // --- left: group bar, table, buttons (AEAM's layout) -------------------------------------

    fn visible(&self, m: &Module) -> Vec<&Account> {
        let q = self.search.to_lowercase();
        let mut shown: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.game == m.id)
            .filter(|a| self.group_filter.as_ref().is_none_or(|g| &a.group == g))
            .filter(|a| {
                q.is_empty()
                    || self.title(a).to_lowercase().contains(&q)
                    || a.name.to_lowercase().contains(&q)
                    || a.group.to_lowercase().contains(&q)
                    || a.notes.to_lowercase().contains(&q)
                    || a.fields.values().any(|v| v.to_lowercase().contains(&q))
            })
            .collect();
        shown.sort_by(|a, b| (a.group.is_empty(), &a.group, a.sort).cmp(&(b.group.is_empty(), &b.group, b.sort)));
        shown
    }

    fn groups(&self, game: &str) -> Vec<String> {
        let set: BTreeSet<String> = self.accounts.iter().filter(|a| a.game == game && !a.group.is_empty()).map(|a| a.group.clone()).collect();
        set.into_iter().collect()
    }

    fn accounts_side(&mut self, ui: &mut egui::Ui, m: &Module) {
        let in_game = self.accounts.iter().filter(|a| a.game == m.id).count();
        ui.horizontal(|ui| {
            ui.label(look::tag("Group:"));
            let label = match &self.group_filter {
                None => format!("All Groups ({in_game})"),
                Some(g) => format!("{g} ({})", self.accounts.iter().filter(|a| a.game == m.id && &a.group == g).count()),
            };
            let groups = self.groups(&m.id);
            egui::ComboBox::from_id_salt("group_filter").selected_text(label).width(200.0).show_ui(ui, |ui| {
                if ui.selectable_label(self.group_filter.is_none(), format!("All Groups ({in_game})")).clicked() {
                    self.group_filter = None;
                }
                for g in groups {
                    let n = self.accounts.iter().filter(|a| a.game == m.id && a.group == g).count();
                    if ui.selectable_label(self.group_filter.as_ref() == Some(&g), format!("{g} ({n})")).clicked() {
                        self.group_filter = Some(g);
                    }
                }
            });
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search").desired_width(160.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::group(ui.style()).inner_margin(egui::vec2(8.0, 2.0)).show(ui, |ui| {
                    ui.label(RichText::new(format!("{} selected", self.selected.len())).strong());
                });
            });
        });
        ui.add_space(2.0);
        let bar_h = 34.0;
        let table_h = (ui.available_height() - bar_h).max(80.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| self.table(ui, m));
        ui.horizontal(|ui| {
            if ui.button("Add Account").clicked() {
                self.add_account(m);
            }
            let ids: Vec<String> = self.selected.iter().cloned().collect();
            if ui.add_enabled(!ids.is_empty(), egui::Button::new("Remove")).clicked() {
                self.confirm_remove = ids.clone();
            }
            let mine: Vec<String> = ids.iter().filter(|id| self.running.contains_key(*id)).cloned().collect();
            if !mine.is_empty() && ui.button(format!("Stop ({})", mine.len())).clicked() {
                self.stop(&mine);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("right-click accounts for more").weak().small());
            });
        });
    }

    fn add_account(&mut self, m: &Module) {
        let a = Account {
            id: store::new_id(),
            game: m.id.clone(),
            group: self.group_filter.clone().unwrap_or_default(),
            created: store::now(),
            sort: self.accounts.iter().map(|a| a.sort).max().unwrap_or(0) + 1,
            ..Default::default()
        };
        self.focus = Some(a.id.clone());
        self.selected = [a.id.clone()].into();
        self.draft = Some(a);
        self.draft_new = true;
    }

    fn stop(&mut self, ids: &[String]) {
        for id in ids {
            if let Some(r) = self.running.get_mut(id) {
                r.stop();
                self.status.insert(id.clone(), ("stopping".into(), false));
            }
        }
    }

    fn mode(&self, m: &Module, a: &Account) -> String {
        if m.field("role").is_none() {
            return String::new();
        }
        let main = a.value("role") == "main";
        let fps = a.value("fps").parse::<u32>().unwrap_or(if main { 330 } else { 6 });
        let fps = if fps == 0 || fps >= 1000 { "∞".to_string() } else { fps.to_string() };
        format!("{} • {fps} FPS", if main { "H" } else { "L" })
    }

    fn table(&mut self, ui: &mut egui::Ui, m: &Module) {
        let rows: Vec<(String, String, String, String, String, String)> = self
            .visible(m)
            .iter()
            .map(|a| {
                let user = a.value(&m.title_field).to_string();
                let user = if user.is_empty() { self.title(a) } else { user };
                (a.id.clone(), user, a.name.clone(), a.group.clone(), self.mode(m, a), a.notes.lines().next().unwrap_or("").to_string())
            })
            .collect();
        let order: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let first_label = m.field(&m.title_field).map_or("Account".to_string(), |f| f.label.clone());
        let has_mode = m.field("role").is_some();
        let mut clicked: Option<(String, bool)> = None; // (id, secondary)
        let mut double = None;
        let mut menu_for: Option<Vec<String>> = None;
        let mut menu_actions: Option<MenuChoice> = None;
        let selected = self.selected.clone();
        let running: HashSet<String> = self.running.keys().cloned().collect();
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(120.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::initial(100.0).at_least(50.0).clip(true).resizable(true))
            .column(Column::initial(80.0).at_least(50.0).clip(true).resizable(true));
        if has_mode {
            table = table.column(Column::initial(90.0).at_least(70.0).clip(true).resizable(true));
        }
        table
            .column(Column::remainder().at_least(60.0).clip(true))
            .auto_shrink(false)
            .header(24.0, |mut h| {
                let mut heads = vec![first_label.as_str(), "Alias", "Group"];
                if has_mode {
                    heads.push("Mode");
                }
                heads.push("Description");
                for t in heads {
                    h.col(|ui| {
                        ui.label(look::tag(t));
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, rows.len(), |mut row| {
                    let (id, user, alias, group, mode, notes) = &rows[row.index()];
                    row.set_selected(selected.contains(id));
                    let on = running.contains(id);
                    row.col(|ui| {
                        let dot = if on { look::ONLINE } else { egui::Color32::TRANSPARENT };
                        ui.label(RichText::new("●").color(dot).size(9.0));
                        ui.label(if on { RichText::new(user).strong() } else { RichText::new(user) });
                    });
                    row.col(|ui| {
                        ui.label(alias);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(group).weak());
                    });
                    if has_mode {
                        row.col(|ui| {
                            ui.label(RichText::new(mode).weak());
                        });
                    }
                    row.col(|ui| {
                        match self.status.get(id) {
                            Some((s, bad)) => {
                                let c = if *bad { ui.visuals().error_fg_color } else { look::colors().accent };
                                ui.label(RichText::new(s).color(c));
                            }
                            None => {
                                ui.label(notes);
                            }
                        }
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some((id.clone(), false));
                    }
                    if resp.double_clicked() {
                        double = Some(id.clone());
                    }
                    if resp.secondary_clicked() {
                        clicked = Some((id.clone(), true));
                    }
                    resp.context_menu(|ui| {
                        let ids: Vec<String> = if selected.contains(id) { selected.iter().cloned().collect() } else { vec![id.clone()] };
                        menu_for = Some(ids.clone());
                        if let Some(c) = self.account_menu(ui, m, &ids) {
                            menu_actions = Some(c);
                        }
                    });
                });
            });
        if let Some((id, secondary)) = clicked {
            let mods = ui.input(|i| i.modifiers);
            if secondary {
                if !self.selected.contains(&id) {
                    self.selected = [id.clone()].into();
                    self.focus_on(&id);
                }
            } else if mods.command {
                if !self.selected.remove(&id) {
                    self.selected.insert(id.clone());
                }
                self.focus_on(&id);
            } else if mods.shift {
                let from = self.focus.as_ref().and_then(|f| order.iter().position(|x| x == f)).unwrap_or(0);
                let to = order.iter().position(|x| *x == id).unwrap_or(0);
                let (lo, hi) = (from.min(to), from.max(to));
                self.selected.extend(order[lo..=hi].iter().cloned());
            } else {
                self.selected = [id.clone()].into();
                self.focus_on(&id);
            }
        }
        if let Some(id) = double {
            if let Some(a) = m.actions.iter().find(|a| a.primary) {
                self.run_action(&a.id.clone(), vec![id]);
            }
        }
        if let (Some(ids), Some(choice)) = (menu_for, menu_actions) {
            self.apply_menu(m, ids, choice);
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) && !ui.ctx().egui_wants_keyboard_input() {
            self.selected = order.into_iter().collect();
        }
    }

    // --- right-click menu ----------------------------------------------------------------------

    fn account_menu(&mut self, ui: &mut egui::Ui, m: &Module, ids: &[String]) -> Option<MenuChoice> {
        let n = ids.len();
        let count = |label: &str| if n > 1 { format!("{label} ({n})") } else { label.to_string() };
        let mut choice = None;
        if n > 1 {
            ui.label(RichText::new(format!("{n} accounts")).weak());
        }
        for a in &m.actions {
            if ui.button(count(&a.label)).clicked() {
                choice = Some(MenuChoice::Run(a.id.clone()));
            }
        }
        if ids.iter().any(|id| self.running.contains_key(id)) && ui.button(count("Stop")).clicked() {
            choice = Some(MenuChoice::Stop);
        }
        ui.separator();
        if let Some(role) = m.field("role").filter(|f| !f.choices.is_empty()) {
            ui.menu_button("Mode", |ui| {
                for c in &role.choices {
                    let label = match c.as_str() {
                        "main" => "High (main)".to_string(),
                        "alt" => "Low (alt)".to_string(),
                        o => o.to_string(),
                    };
                    if ui.button(label).clicked() {
                        choice = Some(MenuChoice::Set(role.key.clone(), c.clone()));
                    }
                }
            });
        }
        ui.menu_button("Move to group", |ui| {
            for g in self.groups(&m.id) {
                if ui.button(&g).clicked() {
                    choice = Some(MenuChoice::Group(g));
                }
            }
            if ui.button(RichText::new(NO_GROUP).weak()).clicked() {
                choice = Some(MenuChoice::Group(String::new()));
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_group).hint_text("New group").desired_width(120.0));
                if ui.add_enabled(!self.new_group.trim().is_empty(), egui::Button::new("Move")).clicked() {
                    choice = Some(MenuChoice::Group(self.new_group.trim().to_string()));
                    self.new_group.clear();
                }
            });
        });
        ui.menu_button("Copy", |ui| {
            if n == 1 {
                if let Some(a) = self.accounts.iter().find(|a| a.id == ids[0]) {
                    let now = unix_now();
                    for f in &m.fields {
                        let v = a.value(&f.key);
                        if v.is_empty() {
                            continue;
                        }
                        if f.kind == "totp" {
                            if let Some(code) = totp::code(v, now) {
                                if ui.button(format!("{} code", f.label)).clicked() {
                                    choice = Some(MenuChoice::Copy(code));
                                }
                            }
                            continue;
                        }
                        if ui.button(&f.label).clicked() {
                            choice = Some(MenuChoice::Copy(v.to_string()));
                        }
                    }
                }
            } else {
                for f in m.fields.iter().filter(|f| !f.secret) {
                    if ui.button(format!("{} (one per line)", f.label)).clicked() {
                        let all: Vec<String> = ids
                            .iter()
                            .filter_map(|id| self.accounts.iter().find(|a| &a.id == id))
                            .map(|a| a.value(&f.key).to_string())
                            .filter(|v| !v.is_empty())
                            .collect();
                        choice = Some(MenuChoice::Copy(all.join("\n")));
                    }
                }
            }
        });
        ui.separator();
        if ui.button(RichText::new(count("Remove")).color(ui.visuals().error_fg_color)).clicked() {
            choice = Some(MenuChoice::Remove);
        }
        if choice.is_some() {
            ui.close();
        }
        choice
    }

    fn apply_menu(&mut self, m: &Module, ids: Vec<String>, choice: MenuChoice) {
        match choice {
            MenuChoice::Run(action) => self.run_action(&action, ids),
            MenuChoice::Stop => self.stop(&ids),
            MenuChoice::Copy(text) => {
                if let Some(c) = &self.ctx {
                    c.copy_text(text);
                }
                self.say("Copied", false);
            }
            MenuChoice::Remove => self.confirm_remove = ids,
            MenuChoice::Group(g) => {
                self.edit_all(&ids, |a| a.group = g.clone());
                self.say(format!("Moved {} to {}", plural(ids.len()), if g.is_empty() { NO_GROUP } else { &g }), false);
            }
            MenuChoice::Set(key, value) => {
                let secret = m.is_secret(&key);
                self.edit_all(&ids, |a| {
                    let map = if secret { &mut a.secrets } else { &mut a.fields };
                    map.insert(key.clone(), value.clone());
                    // a new mode drops a per-account FPS, as AEAM does
                    if key == "role" {
                        a.fields.remove("fps");
                    }
                });
            }
        }
    }

    fn edit_all(&mut self, ids: &[String], f: impl Fn(&mut Account)) {
        let list: Vec<Account> = self.accounts.iter().filter(|a| ids.contains(&a.id)).cloned().collect();
        for mut a in list {
            f(&mut a);
            self.save(&a);
            if self.draft.as_ref().is_some_and(|d| d.id == a.id) {
                self.draft = Some(a);
            }
        }
    }

    fn remove_dialog(&mut self, ctx: &egui::Context) {
        if self.confirm_remove.is_empty() {
            return;
        }
        let n = self.confirm_remove.len();
        let mut done = None;
        egui::Modal::new(egui::Id::unique("remove")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(format!("Remove {}?", plural(n)));
            ui.label("Their logins and secrets are deleted from Ludae.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Remove").color(ui.visuals().error_fg_color)).clicked() {
                    done = Some(true);
                }
                if ui.button("Keep").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                let ids = std::mem::take(&mut self.confirm_remove);
                for id in &ids {
                    let res = self.store.as_ref().map(|s| s.delete(id));
                    match res {
                        Some(Ok(())) => {
                            self.accounts.retain(|a| &a.id != id);
                            self.selected.remove(id);
                            if self.focus.as_ref() == Some(id) {
                                self.focus = None;
                                self.draft = None;
                            }
                        }
                        Some(Err(e)) => self.say(format!("Not removed: {e}"), true),
                        None => {}
                    }
                }
            }
            Some(false) => self.confirm_remove.clear(),
            None => {}
        }
    }

    // --- right: launch and the active account (AEAM's deck) -----------------------------------

    fn deck(&mut self, ui: &mut egui::Ui, m: &Module) {
        let ids: Vec<String> = self.selected.iter().filter(|id| self.accounts.iter().any(|a| &a.id == *id)).cloned().collect();
        let can_run = !ids.is_empty() && !self.draft_new;
        let mut run = None;
        look::section(ui, "Target & launch");
        {
            let inputs = self.inputs.entry(m.id.clone()).or_default();
            let full = ui.available_width();
            // the big button's boxes side by side, labels above (Place ID | Job ID)
            if let Some(p) = m.actions.iter().find(|a| a.primary) {
                if !p.inputs.is_empty() {
                    let gap = ui.spacing().item_spacing.x;
                    let n = p.inputs.len() as f32;
                    let w = ((full - gap * (n - 1.0)) / n - 1.0).floor().max(60.0);
                    ui.horizontal(|ui| {
                        for i in &p.inputs {
                            ui.vertical(|ui| {
                                ui.set_width(w);
                                ui.label(RichText::new(&i.label).weak().small());
                                let v = inputs.entry(i.key.clone()).or_default();
                                ui.add(egui::TextEdit::singleline(v).hint_text(&i.label).desired_width(w)).on_hover_text(&i.hint);
                            });
                        }
                    });
                }
                let label = if ids.len() > 1 { format!("{} ({})", p.label, ids.len()) } else { p.label.clone() };
                if ui.add_enabled(can_run, look::primary(&label).min_size(egui::vec2(full - 1.0, 30.0))).clicked() {
                    run = Some(p.id.clone());
                }
            }
            // one-box buttons as a row: [box][Follow]
            for a in m.actions.iter().filter(|a| !a.primary && a.inputs.len() == 1) {
                let i = &a.inputs[0];
                row(ui, |ui| {
                    if ui.add_enabled(can_run, egui::Button::new(&a.label).min_size(egui::vec2(94.0, 0.0))).clicked() {
                        run = Some(a.id.clone());
                    }
                    let v = inputs.entry(i.key.clone()).or_default();
                    ui.add(egui::TextEdit::singleline(v).hint_text(&i.hint).desired_width(ui.available_width()));
                });
            }
        }
        if let Some(a) = run {
            self.run_action(&a, ids.clone());
        }

        look::section(ui, "Active account");
        self.editor(ui, m);

        let others: Vec<_> = m.actions.iter().filter(|a| !a.primary && a.inputs.len() != 1).cloned().collect();
        let mine: Vec<String> = ids.iter().filter(|id| self.running.contains_key(*id)).cloned().collect();
        if !others.is_empty() || !mine.is_empty() {
            look::section(ui, "Controls");
            ui.horizontal_wrapped(|ui| {
                for a in &others {
                    let label = if ids.len() > 1 { format!("{} ({})", a.label, ids.len()) } else { a.label.clone() };
                    if ui.add_enabled(can_run, egui::Button::new(label)).clicked() {
                        self.run_action(&a.id, ids.clone());
                    }
                }
                if !mine.is_empty() && ui.button(if mine.len() > 1 { format!("Stop ({})", mine.len()) } else { "Stop".into() }).clicked() {
                    self.stop(&mine);
                }
            });
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, m: &Module) {
        if self.selected.len() > 1 {
            ui.label(format!("{} accounts selected.", self.selected.len()));
            ui.label(RichText::new("The buttons above run for all of them; right-click for more.").weak());
            return;
        }
        let Some(mut d) = self.draft.take() else {
            ui.label(RichText::new("Choose an account, or Add Account.").weak());
            return;
        };
        let now = unix_now();
        let saved = self.accounts.iter().find(|a| a.id == d.id).cloned();
        let mut copy = None;
        let mut save_now = false;
        // Alias row, as AEAM: [alias][Set Alias]
        let fallback = self.title(&Account { name: String::new(), ..d.clone() });
        row(ui, |ui| {
            if !self.draft_new && ui.add_enabled(saved.as_ref().is_some_and(|s| s.name != d.name), egui::Button::new("Set Alias").min_size(egui::vec2(94.0, 0.0))).clicked() {
                save_now = true;
            }
            ui.add(egui::TextEdit::singleline(&mut d.name).hint_text(format!("Alias ({fallback})")).desired_width(ui.available_width()));
        });
        egui::Grid::new("fields").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new("Group").weak());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.group).hint_text(NO_GROUP).desired_width(150.0));
                let groups = self.groups(&m.id);
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
                ui.label(RichText::new(&f.label).weak());
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
                        })
                        .response
                        .on_hover_text(&f.hint);
                    } else {
                        let show = !f.secret || self.shown_secrets.contains(&key);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if f.secret {
                                if ui.add_enabled(!v.is_empty(), egui::Button::new("Copy").small()).clicked() {
                                    copy = Some(v.clone());
                                }
                                if ui.small_button(if show { "Hide" } else { "Show" }).clicked() && !self.shown_secrets.remove(&key) {
                                    self.shown_secrets.insert(key);
                                }
                            }
                            ui.add(egui::TextEdit::singleline(&mut v).password(!show).hint_text(&f.hint).desired_width(ui.available_width()));
                        });
                    }
                });
                if v.is_empty() { map.remove(&f.key); } else { map.insert(f.key.clone(), v.clone()); }
                ui.end_row();
                if f.kind == "totp" && !v.is_empty() {
                    ui.label("");
                    ui.horizontal(|ui| match totp::code(&v, now) {
                        Some(c) => {
                            ui.label(RichText::new(format!("{} {}", &c[..3], &c[3..])).monospace().size(18.0).strong());
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
        });
        ui.add_space(4.0);
        ui.add(egui::TextEdit::multiline(&mut d.notes).hint_text("Notes & Description...").desired_rows(6).desired_width(f32::INFINITY));
        if let Some(c) = copy {
            ui.ctx().copy_text(c);
            self.say("Copied", false);
        }
        let dirty = self.draft_new || saved.as_ref() != Some(&d);
        let mut keep = true;
        ui.horizontal(|ui| {
            let label = if self.draft_new { "Add" } else { "Save" };
            if ui.add_enabled(dirty, egui::Button::new(label)).clicked()
                || (dirty && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)))
            {
                save_now = true;
            }
            if dirty && ui.button(if self.draft_new { "Cancel" } else { "Undo changes" }).clicked() {
                match &saved {
                    Some(s) => d = s.clone(),
                    None => keep = false,
                }
            }
        });
        if save_now && self.save(&d) {
            self.say(format!("Saved {}", self.title(&d)), false);
            if self.draft_new {
                self.draft_new = false;
                // a new account of a game with a helper gets its name filled in at once
                if let Some(check) = m.actions.iter().find(|a| a.id == "check") {
                    let (id, aid) = (d.id.clone(), check.id.clone());
                    self.draft = Some(d.clone());
                    self.run_action(&aid, vec![id]);
                    return;
                }
            }
        }
        if keep {
            if self.draft.is_none() {
                self.draft = Some(d);
            }
        } else {
            self.focus = None;
            self.draft_new = false;
            self.selected.clear();
        }
    }
}

enum MenuChoice {
    Run(String),
    Stop,
    Copy(String),
    Remove,
    Group(String),
    Set(String, String),
}

fn plural(n: usize) -> String {
    if n == 1 { "1 account".into() } else { format!("{n} accounts") }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A row filling the width it has, laid out from the right: buttons first, then a box taking the rest.
fn row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), h), egui::Layout::right_to_left(egui::Align::Center), add);
}

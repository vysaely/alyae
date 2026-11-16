//! The window: pick a game, then its accounts (a list on the left, the chosen one on the right).

use crate::look;
use crate::module::{self, Event, Module, Running};
use crate::store::{self, Account, Store, Values};
use alyae_otp as totp;
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
    /// the column the table is sorted by (and whether reversed); None: groups, then own order
    sort_by: Option<(Sort, bool)>,
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
    /// the module's watch (online status): its values by account, the run in progress, the last run
    watch_values: HashMap<String, String>,
    watching: Option<std::process::Child>,
    last_watch: Option<(String, std::time::Instant)>,
    /// the watch's last error, shown once until it changes or the watch works again
    watch_error: Option<String>,
    /// extra wait before the next watch run after the game refused for too many requests:
    /// 1 minute, doubling to 10 while it keeps refusing, back to none once a run works
    watch_backoff: Duration,
    /// an action waiting in its form: (action id, accounts)
    form: Option<(String, Vec<String>)>,
    /// accounts made by Add account or Import whose action (login window, check) is still running:
    /// kept when it works, dropped when it does not
    adding: HashSet<String>,
    /// the Import dialog's pasted text, while it is open
    import_text: Option<String>,
    /// the name typed for a new bookmark
    bookmark_name: String,
    /// the watch column shows its text (else only the dot, the text on hover)
    watch_text: bool,
    /// the accounts-file dialog, when open
    transfer: Option<Transfer>,
    /// the Edit Account window (every field of the chosen account) is open
    edit_open: bool,
    /// the Alias and Description boxes on the right, and the account they were filled from
    side_for: Option<String>,
    alias_buf: String,
    notes_buf: String,
}

/// The accounts-file dialog: write all accounts to a file, or bring a file's accounts in.
#[derive(Default)]
struct Transfer {
    export: bool,
    path: String,
    password: String,
    again: String,
}

const NO_GROUP: &str = "Ungrouped";

impl App {
    pub fn new(
        ctx: &egui::Context,
        opened: Result<Store, String>,
        dir: PathBuf,
        (modules, problems): (Vec<Module>, Vec<String>),
    ) -> Self {
        crate::look::apply(ctx);
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
            sort_by: None,
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
            ctx: Some(ctx.clone()),
            watch_values: HashMap::new(),
            watching: None,
            last_watch: None,
            watch_error: None,
            watch_backoff: Duration::ZERO,
            form: None,
            adding: HashSet::new(),
            import_text: None,
            bookmark_name: String::new(),
            watch_text: true,
            transfer: None,
            edit_open: false,
            side_for: None,
            alias_buf: String::new(),
            notes_buf: String::new(),
        };
        match opened.and_then(|s| s.list().map(|l| (s, l))) {
            Ok((s, (list, problems))) => {
                app.accounts = list;
                app.watch_text = s.setting("watch_text").is_none_or(|v| v != "0");
                let last = s
                    .setting("last_game")
                    .filter(|g| app.modules.iter().any(|m| &m.id == g));
                app.store = Some(s);
                if last.is_some() {
                    app.choose_game(last);
                }
                if !problems.is_empty() {
                    app.say(problems.join("; "), true);
                }
            }
            Err(e) => app.fatal = Some(e),
        }
        app
    }

    fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id == id)
    }

    fn say(&mut self, text: impl Into<String>, bad: bool) {
        let text = text.into();
        crate::log::line("", &text);
        self.message = Some((text, bad));
    }

    /// An account's name for the log, by id.
    fn name_of(&self, id: &str) -> String {
        self.accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| self.title(a))
            .unwrap_or_else(|| id.to_string())
    }

    fn title(&self, a: &Account) -> String {
        if !a.name.trim().is_empty() {
            return a.name.clone();
        }
        let t = self
            .module(&a.game)
            .map(|m| a.value(&m.title_field).to_string())
            .unwrap_or_default();
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
            if !self.inputs.contains_key(g)
                && let Some(v) = s.setting(&format!("inputs.{g}"))
            {
                self.inputs
                    .insert(g.clone(), serde_json::from_str(&v).unwrap_or_default());
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
        let result = crate::aeam::import(s, &self.accounts).and_then(|r| s.list().map(|l| (r, l)));
        match result {
            Ok((report, (list, problems))) => {
                self.accounts = list;
                let mut text = format!("Brought in {} Roblox accounts from AEAM", report.added);
                if report.skipped > 0 {
                    text += &format!(" ({} were here already)", report.skipped);
                }
                let bad_total: usize = report.bad_secrets.iter().map(|(_, n)| *n).sum();
                if bad_total > 0 {
                    let who: Vec<String> = report
                        .bad_secrets
                        .iter()
                        .map(|(name, n)| {
                            if *n > 1 {
                                format!("{name} ({n})")
                            } else {
                                name.clone()
                            }
                        })
                        .collect();
                    text += &format!(
                        "; {bad_total} secret{} could not be decrypted ({})",
                        if bad_total == 1 { "" } else { "s" },
                        who.join(", ")
                    );
                }
                if !problems.is_empty() {
                    text += &format!("; {}", problems.join("; "));
                }
                self.say(text, bad_total > 0 || !problems.is_empty());
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
                    // field names only: the values may be secrets
                    let keys: Vec<&str> = values.keys().map(String::as_str).collect();
                    crate::log::line(
                        &self.name_of(&account),
                        &format!("saved {}", keys.join(", ")),
                    );
                    let Some(mut a) = self.accounts.iter().find(|a| a.id == account).cloned()
                    else {
                        continue;
                    };
                    let secret: Vec<bool> = values
                        .keys()
                        .map(|k| self.module(&a.game).is_some_and(|m| m.is_secret(k)))
                        .collect();
                    let clean = self.draft.as_ref().is_some_and(|d| d.id == a.id)
                        && self.accounts.iter().any(|x| Some(x) == self.draft.as_ref());
                    for ((k, v), secret) in values.iter().zip(&secret) {
                        set_value(&mut a, k, v, *secret);
                    }
                    if self.save(&a) {
                        match self.draft.as_mut() {
                            Some(_) if clean => self.draft = Some(a),
                            // unsaved edits stay, with the helper's new values (a renewed cookie
                            // or password) on top, so Save does not write the old ones back
                            Some(d) if d.id == a.id => {
                                for ((k, v), secret) in values.iter().zip(&secret) {
                                    set_value(d, k, v, *secret);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::Status { account, text } => {
                    crate::log::line(&self.name_of(&account), &text);
                    self.status.insert(account, (text, false));
                }
                Event::Error { account, text } => {
                    let name = self
                        .accounts
                        .iter()
                        .find(|a| a.id == account)
                        .map(|a| self.title(a))
                        .unwrap_or_default();
                    self.say(format!("{name}: {text}"), true);
                    self.status.insert(account, (text, true));
                }
                Event::Watch(values) => {
                    self.watch_error = None;
                    self.watch_backoff = Duration::ZERO;
                    self.watch_values.extend(values);
                }
                Event::WatchError(e) => {
                    if is_rate_limit(&e) {
                        self.watch_backoff = (self.watch_backoff * 2)
                            .clamp(Duration::from_secs(60), Duration::from_secs(600));
                    }
                    if self.watch_error.as_ref() != Some(&e) {
                        let column = self
                            .game
                            .as_deref()
                            .and_then(|g| self.module(g))
                            .and_then(|m| m.watch.as_ref())
                            .map_or_else(|| "Watch".to_string(), |w| w.column.clone());
                        self.say(format!("{column}: {e}"), true);
                        self.watch_error = Some(e);
                    }
                }
                Event::WatchEnded => {
                    if let Some(mut c) = self.watching.take() {
                        let _ = c.wait();
                    }
                }
                Event::Ended {
                    account,
                    action,
                    ok,
                } => {
                    crate::log::line(
                        &self.name_of(&account),
                        &format!("{action} ended{}", if ok { "" } else { " with an error" }),
                    );
                    if self
                        .running
                        .get(&account)
                        .is_some_and(|r| r.action == action)
                    {
                        if let Some(mut r) = self.running.remove(&account) {
                            let _ = r.child.wait();
                        }
                    } else if let Some(i) = self
                        .tasks
                        .iter()
                        .position(|(a, r)| *a == account && r.action == action)
                    {
                        let _ = self.tasks.swap_remove(i).1.child.wait();
                    }
                    if ok && self.status.get(&account).is_some_and(|(_, bad)| !bad) {
                        self.status.remove(&account);
                    }
                    if self.adding.remove(&account) {
                        self.finish_add(&account, ok);
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
        let Some(game) = self.game.clone() else {
            return;
        };
        let Some(m) = self.module(&game).cloned() else {
            return;
        };
        let Some(action) = m.actions.iter().find(|a| a.id == action_id).cloned() else {
            return;
        };
        // only this button's own boxes: a Follow target must not ride along with Join Server
        let all = self.inputs.get(&game).cloned().unwrap_or_default();
        let inputs: Values = action
            .inputs
            .iter()
            .filter_map(|i| {
                all.get(&i.key)
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| (i.key.clone(), v.clone()))
            })
            .collect();
        if let Some(s) = &self.store {
            s.set_setting(
                &format!("inputs.{game}"),
                &serde_json::to_string(&all).unwrap_or_default(),
            );
        }
        let mut started = 0;
        for (index, id) in ids.into_iter().enumerate() {
            // the account's place in the batch: helpers space requests out by it
            let mut inputs = inputs.clone();
            inputs.insert("index".into(), index.to_string());
            if action.runs && self.running.contains_key(&id) {
                continue;
            }
            let Some(mut a) = self.accounts.iter().find(|a| a.id == id).cloned() else {
                continue;
            };
            let ctx = self.ctx.clone();
            match module::start(
                &m,
                &action,
                &a,
                &inputs,
                &self.dir,
                self.tx.clone(),
                move || {
                    if let Some(c) = &ctx {
                        c.request_repaint();
                    }
                },
            ) {
                Ok(r) => {
                    crate::log::line(&self.title(&a), &format!("{} started", action.label));
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
            self.say(
                format!("{} started for {started} accounts", action.label),
                false,
            );
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        if let Some(e) = &self.fatal {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Alyae cannot open its accounts");
                ui.label(e);
                ui.label(format!("Folder: {}", self.dir.display()));
            });
            return;
        }
        // helpers wake the window with their events; a plain command can only be looked at
        if self.running.values().any(Running::is_command) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        match self.game.clone() {
            None => self.game_picker(ui),
            Some(g) => {
                self.tick_watch(&ctx, &g);
                self.game_view(ui, &g);
            }
        }
        self.edit_dialog(&ctx);
        self.remove_dialog(&ctx);
        self.form_dialog(&ctx);
        // an accounts file dropped on the window opens the import
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .find_map(|f| Some(f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()))
        });
        if let Some(path) = dropped.filter(|p| p.extension().is_some_and(|e| e == "alyae")) {
            self.transfer = Some(Transfer {
                export: false,
                path: path.display().to_string(),
                ..Default::default()
            });
        }
        self.transfer_dialog(&ctx);
        self.import_dialog(&ctx);
    }
}

impl App {
    fn game_picker(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| ui.heading("What are you playing?"));
            ui.add_space(16.0);
            let mut chosen = None;
            let width = 240.0;
            let per_row = ((ui.available_width() + 12.0) / (width + 12.0))
                .floor()
                .max(1.0) as usize;
            for row in self.modules.chunks(per_row) {
                ui.horizontal(|ui| {
                    let used = row.len() as f32 * (width + 12.0) - 12.0;
                    ui.add_space(((ui.available_width() - used) / 2.0).max(0.0));
                    for m in row {
                        let count = self.accounts.iter().filter(|a| a.game == m.id).count();
                        let running = self
                            .accounts
                            .iter()
                            .filter(|a| a.game == m.id && self.running.contains_key(&a.id))
                            .count();
                        let resp = egui::Frame::group(ui.style())
                            .inner_margin(14.0)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(width - 28.0);
                                    ui.set_min_height(84.0);
                                    ui.label(RichText::new(&m.name).heading());
                                    ui.add(
                                        egui::Label::new(RichText::new(&m.description).weak())
                                            .wrap(),
                                    );
                                    ui.add_space(4.0);
                                    let mut line = format!(
                                        "{count} account{}",
                                        if count == 1 { "" } else { "s" }
                                    );
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
                && self
                    .store
                    .as_ref()
                    .is_some_and(|s| s.setting("aeam_imported").is_none());
            if aeam_waiting {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        "Your AEAM accounts can come along: they are copied, AEAM stays as it is.",
                    );
                    if ui.button("Bring in AEAM accounts").clicked() {
                        self.import_aeam();
                    }
                });
            }
            if self.store.is_some() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Save accounts to a file")
                            .on_hover_text(
                                "all accounts, sealed with a password: for another PC or a backup",
                            )
                            .clicked()
                        {
                            self.transfer = Some(Transfer {
                                export: true,
                                path: crate::transfer::default_path().display().to_string(),
                                ..Default::default()
                            });
                        }
                        if ui
                            .button("Bring in accounts from a file")
                            .on_hover_text("an .alyae file (or drop one on the window)")
                            .clicked()
                        {
                            self.transfer = Some(Transfer::default());
                        }
                    });
                });
            }
            for p in &self.problems {
                ui.label(RichText::new(p).color(ui.visuals().error_fg_color));
            }
            ui.vertical_centered(|ui| self.message_line(ui));
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
        // AEAM's layout: the status line along the bottom, the account's controls on the right,
        // the list on the left
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .show(ui, |ui| self.status_bar(ui, &m));
        egui::Panel::right("deck")
            .default_size(340.0)
            .min_size(280.0)
            .show(ui, |ui| self.deck(ui, &m));
        egui::CentralPanel::default().show(ui, |ui| self.accounts_side(ui, &m));
    }

    /// The bottom line: the last message on the left; counts, the accounts file and the log on
    /// the right.
    fn status_bar(&mut self, ui: &mut egui::Ui, m: &Module) {
        ui.horizontal_centered(|ui| {
            self.message_line(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Open log")
                    .on_hover_text(self.dir.join("alyae.log").display().to_string())
                    .clicked()
                {
                    let path = self.dir.join("alyae.log");
                    let opener = if cfg!(windows) {
                        "explorer"
                    } else {
                        "xdg-open"
                    };
                    if let Err(e) = std::process::Command::new(opener).arg(&path).spawn() {
                        self.say(format!("cannot open {}: {e}", path.display()), true);
                    }
                }
                // moving accounts to another PC (or a backup): every game's accounts in one file
                if ui
                    .small_button("Accounts file")
                    .on_hover_text(
                        "Save all accounts to a password-sealed file, or bring one in \
                         (an .alyae file can also be dropped on the window)",
                    )
                    .clicked()
                {
                    self.transfer = Some(Transfer {
                        export: true,
                        path: crate::transfer::default_path().display().to_string(),
                        ..Default::default()
                    });
                }
                let total = self.accounts.iter().filter(|a| a.game == m.id).count();
                let running = self
                    .accounts
                    .iter()
                    .filter(|a| a.game == m.id && self.running.contains_key(&a.id))
                    .count();
                let text = format!("Total: {total}  \u{2022}  Running: {running}");
                let color = if running > 0 {
                    look::colors().accent
                } else {
                    look::colors().muted
                };
                ui.label(RichText::new(text).color(color));
            });
        });
    }

    /// The last message (an error in red); clicking it dismisses it.
    fn message_line(&mut self, ui: &mut egui::Ui) {
        if let Some((text, bad)) = self.message.clone() {
            let color = if bad {
                ui.visuals().error_fg_color
            } else {
                look::colors().muted
            };
            let resp = ui.add(
                egui::Label::new(RichText::new(text).color(color))
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            if resp.on_hover_text("Click to dismiss").clicked() {
                self.message = None;
            }
        }
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
        shown.sort_by(|a, b| {
            (a.group.is_empty(), &a.group, a.sort).cmp(&(b.group.is_empty(), &b.group, b.sort))
        });
        if let Some((by, reverse)) = self.sort_by {
            let lower = |s: &str| s.to_lowercase();
            let user = |a: &Account| lower(&self.title_value(m, a));
            // empty values last, whichever way the column runs
            let key = |a: &Account| -> (bool, String, String) {
                let v = match by {
                    Sort::User => user(a),
                    Sort::Alias => lower(&a.name),
                    Sort::Group => lower(&a.group),
                    Sort::Mode => self.mode(m, a),
                    Sort::Notes => lower(a.notes.lines().next().unwrap_or("")),
                    // a coloured status (in game, online) before one without (offline), then by text
                    Sort::Watch => {
                        let v = self.watch_values.get(&a.id).map_or("", String::as_str);
                        let lit = m.watch.as_ref().and_then(|w| w.color_of(v)).is_some();
                        format!("{}{}", if lit { 0 } else { 1 }, lower(v))
                    }
                };
                (v.is_empty(), v, user(a))
            };
            shown.sort_by(|a, b| {
                let (ka, kb) = (key(a), key(b));
                let o = ka.1.cmp(&kb.1).then(ka.2.cmp(&kb.2));
                ka.0.cmp(&kb.0).then(if reverse { o.reverse() } else { o })
            });
        }
        shown
    }

    /// The account's name in the first column: its title field (the username), else its title.
    fn title_value(&self, m: &Module, a: &Account) -> String {
        let v = a.value(&m.title_field);
        if v.is_empty() {
            self.title(a)
        } else {
            v.to_string()
        }
    }

    /// A column heading that sorts by `by`: first click A→Z, second Z→A, third back to no sort.
    fn sort_heading(&mut self, ui: &mut egui::Ui, label: &str, by: Sort) -> egui::Response {
        let arrow = match self.sort_by {
            Some((b, false)) if b == by => " A-Z",
            Some((b, true)) if b == by => " Z-A",
            _ => "",
        };
        let resp = ui.add(
            egui::Label::new(look::tag(&format!("{label}{arrow}"))).sense(egui::Sense::click()),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            self.sort_by = match self.sort_by {
                Some((b, false)) if b == by => Some((by, true)),
                Some((b, true)) if b == by => None,
                _ => Some((by, false)),
            };
        }
        resp
    }

    fn groups(&self, game: &str) -> Vec<String> {
        let set: BTreeSet<String> = self
            .accounts
            .iter()
            .filter(|a| a.game == game && !a.group.is_empty())
            .map(|a| a.group.clone())
            .collect();
        set.into_iter().collect()
    }

    fn accounts_side(&mut self, ui: &mut egui::Ui, m: &Module) {
        let in_game = self.accounts.iter().filter(|a| a.game == m.id).count();
        ui.horizontal(|ui| {
            // the game: switching here, the picker only on the first start
            let mut switch = None;
            egui::ComboBox::from_id_salt("game")
                .selected_text(RichText::new(&m.name).strong())
                .show_ui(ui, |ui| {
                    for g in &self.modules {
                        if ui.selectable_label(g.id == m.id, &g.name).clicked() {
                            switch = Some(g.id.clone());
                        }
                    }
                });
            let label = match &self.group_filter {
                None => format!("All groups ({in_game})"),
                Some(g) => format!(
                    "{g} ({})",
                    self.accounts
                        .iter()
                        .filter(|a| a.game == m.id && &a.group == g)
                        .count()
                ),
            };
            let groups = self.groups(&m.id);
            ui.label(look::tag("Group:"));
            egui::ComboBox::from_id_salt("group_filter")
                .selected_text(label)
                .width(200.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            self.group_filter.is_none(),
                            format!("All groups ({in_game})"),
                        )
                        .clicked()
                    {
                        self.group_filter = None;
                    }
                    for g in groups {
                        let n = self
                            .accounts
                            .iter()
                            .filter(|a| a.game == m.id && a.group == g)
                            .count();
                        if ui
                            .selectable_label(
                                self.group_filter.as_ref() == Some(&g),
                                format!("{g} ({n})"),
                            )
                            .clicked()
                        {
                            self.group_filter = Some(g);
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // AEAM's "1 selected" box
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, look::colors().edge))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{} selected", self.selected.len())).strong(),
                        );
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search")
                        .desired_width(ui.available_width().clamp(80.0, 220.0)),
                );
            });
            if let Some(g) = switch.filter(|g| g != &m.id) {
                self.choose_game(Some(g));
            }
        });
        let bar_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let table_h = (ui.available_height() - bar_h).max(80.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| {
            self.table(ui, m)
        });
        ui.horizontal(|ui| {
            let flow = m.add.clone().unwrap_or_default();
            let import = (!flow.import_field.is_empty()).then(|| {
                if flow.import_label.is_empty() {
                    "Import".to_string()
                } else {
                    flow.import_label.clone()
                }
            });
            let busy = self.adding.len();
            let label = if busy > 0 {
                format!("Add Account ({busy} signing in)")
            } else {
                "Add Account".to_string()
            };
            // AEAM's Add Account: the button does the usual way (sign in on the game's page when
            // the game has one), its arrow and right-click hold the other ways
            let more = |ui: &mut egui::Ui, app: &mut App| {
                if !flow.action.is_empty() && ui.button("Sign in with browser").clicked() {
                    app.add_via(m, &flow.action);
                    ui.close();
                }
                if let Some(i) = &import
                    && ui.button(i).clicked()
                {
                    app.import_text = Some(String::new());
                    ui.close();
                }
                if ui.button("Add manually").clicked() {
                    app.add_account(m);
                    ui.close();
                }
                if ui.button("From an accounts file").clicked() {
                    app.transfer = Some(Transfer::default());
                    ui.close();
                }
            };
            let hover = if flow.action.is_empty() {
                "Fill in a new account".to_string()
            } else {
                "Opens the login page: sign in, and the account is added".to_string()
            };
            let b = ui.button(label).on_hover_text(hover);
            if b.clicked() {
                if flow.action.is_empty() {
                    self.add_account(m);
                } else {
                    self.add_via(m, &flow.action);
                }
            }
            b.context_menu(|ui| more(ui, self));
            let arrow = ui.menu_button("    ", |ui| more(ui, self)).response;
            caret(ui, &arrow);
            arrow.on_hover_text("Other ways to add");
            let ids: Vec<String> = self.selected.iter().cloned().collect();
            if ui
                .add_enabled(!ids.is_empty(), egui::Button::new("Remove"))
                .clicked()
            {
                self.confirm_remove = ids.clone();
            }
            if let Some(browser) = m.actions.iter().find(|a| a.id == "browser")
                && ui
                    .add_enabled(!ids.is_empty(), egui::Button::new("Open Browser"))
                    .on_hover_text("roblox.com signed in as the chosen account, in its own window")
                    .clicked()
            {
                let id = browser.id.clone();
                self.start_action(m, &id, ids);
            }
        });
    }

    fn new_account(&self, m: &Module) -> Account {
        Account {
            id: store::new_id(),
            game: m.id.clone(),
            group: self.group_filter.clone().unwrap_or_default(),
            created: store::now(),
            sort: self.accounts.iter().map(|a| a.sort).max().unwrap_or(0) + 1,
            ..Default::default()
        }
    }

    /// Add account, by hand: an empty account in the form.
    fn add_account(&mut self, m: &Module) {
        let a = self.new_account(m);
        self.focus = Some(a.id.clone());
        self.selected = [a.id.clone()].into();
        self.draft = Some(a);
        self.draft_new = true;
    }

    /// Add account, through the module: a new account and its action (a login window) at once;
    /// what the action sends fills it in, and `finish_add` keeps it or drops it.
    fn add_via(&mut self, m: &Module, action: &str) {
        let a = self.new_account(m);
        if self.save(&a) {
            self.adding.insert(a.id.clone());
            self.run_action(action, vec![a.id.clone()]);
            // the action could not start: nothing waits on it
            if !self.tasks.iter().any(|(id, _)| *id == a.id) && !self.running.contains_key(&a.id) {
                self.adding.remove(&a.id);
                self.drop_account(&a.id);
            }
        }
    }

    /// Import: one account per pasted line, then the module's check runs on all of them.
    fn import_lines(&mut self, m: &Module, text: &str) {
        let Some(flow) = m.add.clone() else { return };
        let secret = m.is_secret(&flow.import_field);
        let mut ids = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut a = self.new_account(m);
            set_value(&mut a, &flow.import_field, line, secret);
            if self.save(&a) {
                ids.push(a.id);
            }
        }
        if ids.is_empty() {
            return;
        }
        let n = ids.len();
        if flow.import_action.is_empty() {
            self.say(format!("Imported {}", plural(n)), false);
            return;
        }
        self.adding.extend(ids.iter().cloned());
        self.run_action(&flow.import_action, ids.clone());
        for id in ids {
            if !self.tasks.iter().any(|(t, _)| *t == id) && !self.running.contains_key(&id) {
                self.adding.remove(&id);
            }
        }
        self.say(format!("Checking {}", plural(n)), false);
    }

    /// An account from Add or Import whose action ended: kept when it worked and got its title
    /// field; merged into an account already here with the same `same` field; else dropped.
    fn finish_add(&mut self, id: &str, ok: bool) {
        let Some(a) = self.accounts.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let Some(m) = self.module(&a.game).cloned() else {
            return;
        };
        if !ok || a.value(&m.title_field).is_empty() {
            self.drop_account(id);
            crate::log::line(
                "Add account",
                "left out: not signed in, or the sign-in did not work",
            );
            if self.adding.is_empty() {
                self.say(
                    "Not added: the login window closed before signing in, or the sign-in did not work"
                        .to_string(),
                    true,
                );
            }
            return;
        }
        let same = m.add.as_ref().map(|f| f.same.clone()).unwrap_or_default();
        let twin = (!same.is_empty() && !a.value(&same).is_empty())
            .then(|| {
                self.accounts
                    .iter()
                    .find(|x| x.id != a.id && x.game == a.game && x.value(&same) == a.value(&same))
                    .cloned()
            })
            .flatten();
        match twin {
            Some(mut old) => {
                for (k, v) in &a.fields {
                    set_value(&mut old, k, v, false);
                }
                for (k, v) in &a.secrets {
                    set_value(&mut old, k, v, true);
                }
                if self.save(&old) {
                    self.drop_account(id);
                    let t = self.title(&old);
                    self.say(
                        format!("{t} was already here: its sign-in is updated"),
                        false,
                    );
                }
            }
            None => {
                let t = self.title(&a);
                crate::log::line(&t, "added");
                self.say(format!("Added {t}"), false);
            }
        }
    }

    /// Deletes an account row (no module clean-up: it never ran).
    fn drop_account(&mut self, id: &str) {
        if let Some(Err(e)) = self.store.as_ref().map(|s| s.delete(id)) {
            self.say(format!("Not removed: {e}"), true);
            return;
        }
        self.accounts.retain(|a| a.id != id);
        self.selected.remove(id);
        self.status.remove(id);
        if self.focus.as_deref() == Some(id) {
            self.focus = None;
            self.draft = None;
        }
    }

    fn import_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut text) = self.import_text.take() else {
            return;
        };
        let Some(m) = self.game.clone().and_then(|g| self.module(&g).cloned()) else {
            return;
        };
        let Some(flow) = m.add.clone() else { return };
        let field = m
            .field(&flow.import_field)
            .map_or(flow.import_field.clone(), |f| f.label.clone());
        let mut done = None;
        egui::Modal::new(egui::Id::unique("import")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(if flow.import_label.is_empty() {
                "Import"
            } else {
                flow.import_label.as_str()
            });
            ui.label(format!(
                "One {} per line. Each becomes an account; ones that do not work are left out.",
                field.to_lowercase()
            ));
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_rows(8)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                });
            let n = text.lines().filter(|l| !l.trim().is_empty()).count();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(n > 0, look::primary(&format!("Import {}", plural(n))))
                    .clicked()
                {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => self.import_lines(&m, &text),
            Some(false) => {}
            None => self.import_text = Some(text),
        }
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
        let fps = a
            .value("fps")
            .parse::<u32>()
            .unwrap_or(if main { 330 } else { 6 });
        let fps = if fps == 0 || fps >= 1000 {
            "no cap".to_string()
        } else {
            format!("{fps} FPS")
        };
        format!("{} • {fps}", if main { "H" } else { "L" })
    }

    fn table(&mut self, ui: &mut egui::Ui, m: &Module) {
        let rows: Vec<(String, String, String, String, String, String, String)> = self
            .visible(m)
            .iter()
            .map(|a| {
                let user = self.title_value(m, a);
                (
                    a.id.clone(),
                    user,
                    a.name.clone(),
                    a.group.clone(),
                    self.mode(m, a),
                    a.notes.lines().next().unwrap_or("").to_string(),
                    self.watch_values.get(&a.id).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let order: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let first_label = m
            .field(&m.title_field)
            .map_or("Account".to_string(), |f| f.label.clone());
        let has_mode = m.field("role").is_some();
        let watch = m.watch.clone();
        let watch_col = watch.as_ref().map(|w| w.column.clone());
        let show_group = self.group_filter.is_none();
        let mut clicked: Option<(String, bool)> = None; // (id, secondary)
        let mut double = None;
        let mut menu_for: Option<Vec<String>> = None;
        let mut menu_actions: Option<MenuChoice> = None;
        let selected = self.selected.clone();
        let running: HashSet<String> = self.running.keys().cloned().collect();
        // every row the same colour; the chosen ones stand out in the accent (look::apply)
        let mut table = TableBuilder::new(ui)
            .striped(false)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        // with its text hidden the status is a dot at the start of the name: no column of its own
        let watch_column = watch_col.is_some() && self.watch_text;
        if watch_column {
            table = table.column(Column::initial(90.0).at_least(60.0).clip(true));
        }
        // AEAM's columns: Username, Alias, Group, Mode, Description
        table = table
            .column(Column::initial(130.0).at_least(80.0).clip(true))
            .column(Column::initial(100.0).at_least(50.0).clip(true));
        if show_group {
            table = table.column(Column::initial(80.0).at_least(50.0).clip(true));
        }
        if has_mode {
            table = table.column(Column::initial(90.0).at_least(70.0).clip(true));
        }
        table
            .column(Column::remainder().at_least(60.0).clip(true))
            .auto_shrink(false)
            .header(20.0, |mut h| {
                let mut show = self.watch_text;
                let mut sort_watch = false;
                let watch_menu = |ui: &mut egui::Ui, show: &mut bool, sort_watch: &mut bool| {
                    if let Some(c) = &watch_col {
                        ui.checkbox(show, format!("{c}: show text"));
                        if ui.button(format!("Sort by {c}")).clicked() {
                            *sort_watch = true;
                            ui.close();
                        }
                    }
                };
                if watch_column {
                    h.col(|ui| {
                        let c = watch_col.clone().unwrap_or_default();
                        self.sort_heading(ui, &c, Sort::Watch)
                            .context_menu(|ui| watch_menu(ui, &mut show, &mut sort_watch));
                    });
                }
                h.col(|ui| {
                    if watch_col.is_some() && !watch_column {
                        ui.add_space(12.0);
                    }
                    self.sort_heading(ui, &first_label, Sort::User)
                        .context_menu(|ui| watch_menu(ui, &mut show, &mut sort_watch));
                });
                h.col(|ui| {
                    self.sort_heading(ui, "Alias", Sort::Alias);
                });
                if show != self.watch_text {
                    self.watch_text = show;
                    if let Some(s) = &self.store {
                        s.set_setting("watch_text", if show { "1" } else { "0" });
                    }
                }
                if sort_watch {
                    self.sort_by = Some((Sort::Watch, false));
                }
                if show_group {
                    h.col(|ui| {
                        self.sort_heading(ui, "Group", Sort::Group);
                    });
                }
                if has_mode {
                    h.col(|ui| {
                        self.sort_heading(ui, "Mode", Sort::Mode);
                    });
                }
                h.col(|ui| {
                    self.sort_heading(ui, "Description", Sort::Notes);
                });
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let (id, user, alias, group, mode, notes, watched) = &rows[row.index()];
                    row.set_selected(selected.contains(id));
                    let on = running.contains(id);
                    let watch_dot = |ui: &mut egui::Ui| -> Option<egui::Response> {
                        let w = watch.as_ref()?;
                        if watched.is_empty() {
                            return None;
                        }
                        let c = w
                            .color_of(watched)
                            .and_then(look::hex)
                            .unwrap_or(look::colors().muted);
                        Some(dot(ui, c))
                    };
                    if watch_column {
                        row.col(|ui| {
                            if watch_dot(ui).is_some() {
                                ui.label(watched);
                            }
                        });
                    }
                    row.col(|ui| {
                        if !watch_column {
                            match watch_dot(ui) {
                                Some(d) => {
                                    d.on_hover_text(watched);
                                }
                                None if watch.is_some() => ui.add_space(8.0),
                                None => {}
                            }
                        }
                        // the accent marks what runs
                        ui.label(if on {
                            RichText::new(user).color(look::colors().accent)
                        } else {
                            RichText::new(user)
                        });
                    });
                    row.col(|ui| {
                        ui.label(alias);
                    });
                    if show_group {
                        row.col(|ui| {
                            ui.label(RichText::new(group).weak());
                        });
                    }
                    if has_mode {
                        row.col(|ui| {
                            ui.label(RichText::new(mode).weak());
                        });
                    }
                    row.col(|ui| match self.status.get(id) {
                        Some((s, bad)) => {
                            let c = if *bad {
                                ui.visuals().error_fg_color
                            } else {
                                look::colors().accent
                            };
                            dot(ui, c);
                            ui.label(RichText::new(s).color(c));
                        }
                        None => {
                            ui.label(notes);
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
                        let ids: Vec<String> = if selected.contains(id) {
                            selected.iter().cloned().collect()
                        } else {
                            vec![id.clone()]
                        };
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
                let from = self
                    .focus
                    .as_ref()
                    .and_then(|f| order.iter().position(|x| x == f))
                    .unwrap_or(0);
                let to = order.iter().position(|x| *x == id).unwrap_or(0);
                let (lo, hi) = (from.min(to), from.max(to));
                self.selected.extend(order[lo..=hi].iter().cloned());
            } else {
                self.selected = [id.clone()].into();
                self.focus_on(&id);
            }
        }
        if let Some(id) = double
            && let Some(a) = m.actions.iter().find(|a| a.primary)
        {
            self.run_action(&a.id.clone(), vec![id]);
        }
        if let (Some(ids), Some(choice)) = (menu_for, menu_actions) {
            self.apply_menu(m, ids, choice);
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
            && !ui.ctx().egui_wants_keyboard_input()
        {
            self.selected = order.into_iter().collect();
        }
    }

    // --- right-click menu ----------------------------------------------------------------------

    fn account_menu(
        &mut self,
        ui: &mut egui::Ui,
        m: &Module,
        ids: &[String],
    ) -> Option<MenuChoice> {
        let n = ids.len();
        let count = |label: &str| {
            if n > 1 {
                format!("{label} ({n})")
            } else {
                label.to_string()
            }
        };
        let mut choice = None;
        if n > 1 {
            ui.label(RichText::new(format!("{n} accounts")).weak());
        }
        for a in &m.actions {
            if ui.button(count(&a.label)).clicked() {
                choice = Some(MenuChoice::Run(a.id.clone()));
            }
        }
        if ids.iter().any(|id| self.running.contains_key(id)) && ui.button(count("Stop")).clicked()
        {
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
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_group)
                        .hint_text("New group")
                        .desired_width(120.0),
                );
                if ui
                    .add_enabled(!self.new_group.trim().is_empty(), egui::Button::new("Move"))
                    .clicked()
                {
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
                            if let Some(code) = totp::code(v, now)
                                && ui.button(format!("{} code", f.label)).clicked()
                            {
                                choice = Some(MenuChoice::Copy(code));
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
        if ui
            .button(RichText::new(count("Remove")).color(ui.visuals().error_fg_color))
            .clicked()
        {
            choice = Some(MenuChoice::Remove);
        }
        if choice.is_some() {
            ui.close();
        }
        choice
    }

    fn apply_menu(&mut self, m: &Module, ids: Vec<String>, choice: MenuChoice) {
        match choice {
            MenuChoice::Run(action) => self.start_action(m, &action, ids),
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
                self.say(
                    format!(
                        "Moved {} to {}",
                        plural(ids.len()),
                        if g.is_empty() { NO_GROUP } else { &g }
                    ),
                    false,
                );
            }
            MenuChoice::Set(key, value) => {
                let secret = m.is_secret(&key);
                self.edit_all(&ids, |a| {
                    let map = if secret {
                        &mut a.secrets
                    } else {
                        &mut a.fields
                    };
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
        let list: Vec<Account> = self
            .accounts
            .iter()
            .filter(|a| ids.contains(&a.id))
            .cloned()
            .collect();
        for mut a in list {
            f(&mut a);
            self.save(&a);
            if self.draft.as_ref().is_some_and(|d| d.id == a.id) {
                self.draft = Some(a);
            }
        }
    }

    fn transfer_dialog(&mut self, ctx: &egui::Context) {
        let Some(t) = self.transfer.as_mut() else {
            return;
        };
        let mut go = false;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("transfer")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                let was = t.export;
                ui.selectable_value(&mut t.export, true, "Save to a file");
                ui.selectable_value(&mut t.export, false, "Bring in from a file");
                if t.export != was {
                    t.path = if t.export {
                        crate::transfer::default_path().display().to_string()
                    } else {
                        String::new()
                    };
                    t.again.clear();
                }
            });
            ui.separator();
            if t.export {
                ui.heading("Save accounts to a file");
                ui.label(
                    "Every account with its passwords, cookies and 2FA secrets, sealed with the \
                     password below. Anyone with the file and the password has the accounts.",
                );
            } else {
                ui.heading("Bring in accounts from a file");
                ui.label("Accounts Alyae has already stay as they are.");
            }
            ui.add_space(6.0);
            ui.label("File");
            ui.add(egui::TextEdit::singleline(&mut t.path).desired_width(f32::INFINITY));
            ui.label("Password");
            let pw = ui.add(egui::TextEdit::singleline(&mut t.password).password(true));
            if t.export {
                ui.label("Password again");
                ui.add(egui::TextEdit::singleline(&mut t.again).password(true));
            }
            let enter = pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if t.export { "Save" } else { "Bring in" };
                if ui.add(crate::look::primary(label)).clicked() || (enter && !t.export) {
                    go = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
        if close {
            self.transfer = None;
            return;
        }
        if !go {
            return;
        }
        let t = self.transfer.take().expect("open dialog");
        let path = std::path::PathBuf::from(t.path.trim());
        if t.export {
            if t.password != t.again {
                self.say("The two passwords differ", true);
                self.transfer = Some(t);
                return;
            }
            let written = crate::transfer::export(&self.accounts, &t.password)
                .and_then(|text| crate::transfer::write_new(&path, &text));
            match written {
                Ok(()) => self.say(
                    format!(
                        "{} saved to {}",
                        plural(self.accounts.len()),
                        path.display()
                    ),
                    false,
                ),
                Err(e) => {
                    self.say(format!("Not saved: {e}"), true);
                    self.transfer = Some(t);
                }
            }
            return;
        }
        let Some(s) = &self.store else { return };
        let result = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))
            .and_then(|text| crate::transfer::read(&text, &t.password))
            .and_then(|list| crate::transfer::import(s, &self.accounts, list))
            .and_then(|r| s.list().map(|l| (r, l)));
        match result {
            Ok((r, (list, _))) => {
                self.accounts = list;
                let mut text = format!("Brought in {}", plural(r.added));
                if r.skipped > 0 {
                    text += &format!(" ({} were here already)", r.skipped);
                }
                let warn = !r.with_command.is_empty();
                if warn {
                    text += &format!(
                        "; check the launch command of {} before launching: a file can carry any command",
                        r.with_command.join(", ")
                    );
                }
                self.say(text, warn);
            }
            Err(e) => {
                self.say(format!("Not brought in: {e}"), true);
                self.transfer = Some(Transfer {
                    password: String::new(),
                    ..t
                });
            }
        }
    }

    fn remove_dialog(&mut self, ctx: &egui::Context) {
        if self.confirm_remove.is_empty() {
            return;
        }
        let n = self.confirm_remove.len();
        // modules that delete an account's files on removal (Roblox Linux: its profile folder)
        let with_files = self.confirm_remove.iter().any(|id| {
            self.accounts
                .iter()
                .find(|a| &a.id == id)
                .and_then(|a| self.module(&a.game))
                .is_some_and(|m| !m.on_remove.is_empty())
        });
        let mut done = None;
        egui::Modal::new(egui::Id::unique("remove")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(format!("Remove {}?", plural(n)));
            ui.label("Their logins and secrets are deleted from Alyae.");
            if with_files {
                ui.label(
                    "Their game files kept by Alyae are deleted too (folders shared with AEAM stay, \
                     only the saved sign-in in them goes).",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Remove").color(ui.visuals().error_fg_color))
                    .clicked()
                {
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
                    // the module's files go first (quick); the row goes even when that fails
                    let account = self.accounts.iter().find(|a| &a.id == id).cloned();
                    if let Some(a) = account
                        && let Some(m) = self.module(&a.game).filter(|m| !m.on_remove.is_empty())
                        && let Err(e) = module::run_and_wait(m, &m.on_remove, &a, &self.dir)
                    {
                        self.say(format!("{}: files not deleted: {e}", self.title(&a)), true);
                    }
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

    /// Runs `action`, or first opens its form when it asks for values or a confirmation.
    fn start_action(&mut self, m: &Module, action: &str, ids: Vec<String>) {
        if m.actions.iter().any(|a| a.id == action && a.needs_form()) {
            self.form = Some((action.to_string(), ids));
        } else {
            self.run_action(action, ids);
        }
    }

    fn form_dialog(&mut self, ctx: &egui::Context) {
        let Some((action_id, ids)) = self.form.clone() else {
            return;
        };
        let Some(game) = self.game.clone() else {
            return;
        };
        let Some(action) = self
            .module(&game)
            .and_then(|m| m.actions.iter().find(|a| a.id == action_id))
            .cloned()
        else {
            self.form = None;
            return;
        };
        let mut done = None;
        let inputs = self.inputs.entry(game).or_default();
        egui::Modal::new(egui::Id::unique("form")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(format!("{} for {}", action.label, plural(ids.len())));
            if !action.confirm.is_empty() {
                ui.label(&action.confirm);
            }
            egui::Grid::new("form_inputs")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    for i in &action.inputs {
                        ui.label(RichText::new(&i.label).weak());
                        let v = inputs.entry(i.key.clone()).or_default();
                        if i.choices.is_empty() {
                            ui.add(
                                egui::TextEdit::singleline(v)
                                    .hint_text(&i.hint)
                                    .desired_width(220.0),
                            );
                        } else {
                            if v.is_empty() {
                                *v = i.choices[0].clone();
                            }
                            egui::ComboBox::from_id_salt(("form", &i.key))
                                .selected_text(v.as_str())
                                .show_ui(ui, |ui| {
                                    for c in &i.choices {
                                        ui.selectable_value(v, c.clone(), c);
                                    }
                                });
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(look::primary(&action.label)).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                self.form = None;
                self.run_action(&action_id, ids);
            }
            Some(false) => self.form = None,
            None => {}
        }
    }

    /// Runs the shown game's watch (online status) when it is due, at most one run at a time.
    /// A run already counts from its start, so `every` is the gap between requests.
    fn tick_watch(&mut self, ctx: &egui::Context, game: &str) {
        let Some(m) = self.module(game).cloned() else {
            return;
        };
        let Some(w) = m.watch.clone() else { return };
        // paused while the window is in the background or minimized: no repaint is asked for.
        // Focusing it again runs the watch only once its interval has passed since the last run:
        // switching between game windows and Alyae must not send a request on every switch
        // (the game's servers limit how often they answer)
        let (focused, minimized) =
            ctx.input(|i| (i.focused, i.viewport().minimized.unwrap_or(false)));
        let every = Duration::from_secs(w.every.max(5)) + self.watch_backoff;
        if !focused || minimized {
            return;
        }
        let due = match &self.last_watch {
            Some((g, t)) if g == game => t.elapsed() >= every,
            _ => {
                self.watch_values.clear();
                true
            }
        };
        if due && self.watching.is_none() {
            self.last_watch = Some((game.to_string(), std::time::Instant::now()));
            let accounts: Vec<&Account> = self.accounts.iter().filter(|a| a.game == game).collect();
            if !accounts.is_empty() {
                let wake_ctx = self.ctx.clone();
                match module::watch(&m, &w, &accounts, &self.dir, self.tx.clone(), move || {
                    if let Some(c) = &wake_ctx {
                        c.request_repaint();
                    }
                }) {
                    Ok(child) => self.watching = Some(child),
                    Err(e) => self.say(format!("{}: {e}", w.column), true),
                }
            }
        }
        ctx.request_repaint_after(every);
    }

    // --- right: launch and the chosen account -----------------------------------------------

    /// The right side, as AEAM's: the join boxes and Join Server, Follow, Alias, Description,
    /// and the four buttons under it.
    fn deck(&mut self, ui: &mut egui::Ui, m: &Module) {
        let ids: Vec<String> = self
            .selected
            .iter()
            .filter(|id| self.accounts.iter().any(|a| &a.id == *id))
            .cloned()
            .collect();
        let can_run = !ids.is_empty() && !self.draft_new;
        let count = |label: &str| {
            if ids.len() > 1 {
                format!("{label} ({})", ids.len())
            } else {
                label.to_string()
            }
        };
        // the Alias and Description boxes follow the account last clicked
        if self.side_for != self.focus {
            self.side_for = self.focus.clone();
            let a = self
                .focus
                .as_ref()
                .and_then(|id| self.accounts.iter().find(|a| &a.id == id));
            self.alias_buf = a.map(|a| a.name.clone()).unwrap_or_default();
            self.notes_buf = a.map(|a| a.notes.clone()).unwrap_or_default();
        }
        let mut run = None;
        let full = ui.available_width();
        let gap = ui.spacing().item_spacing.x;
        let half = ((full - gap) / 2.0).floor();
        let h = 28.0;
        {
            let inputs = self.inputs.entry(m.id.clone()).or_default();
            if let Some(p) = m.actions.iter().find(|a| a.primary) {
                if !p.inputs.is_empty() {
                    let n = p.inputs.len() as f32;
                    let w = ((full - gap * (n - 1.0)) / n).floor();
                    // AEAM's labels over the boxes, the bookmarks at the end of the line
                    ui.horizontal(|ui| {
                        for (k, i) in p.inputs.iter().enumerate() {
                            if k + 1 < p.inputs.len() {
                                ui.allocate_ui(egui::vec2(w, 16.0), |ui| {
                                    ui.set_width(w);
                                    ui.label(look::tag(&i.label));
                                });
                            } else {
                                ui.label(look::tag(&i.label));
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            bookmarks_menu(
                                ui,
                                self.store.as_ref(),
                                &m.id,
                                p,
                                inputs,
                                &mut self.bookmark_name,
                                84.0,
                            );
                        });
                    });
                    ui.horizontal(|ui| {
                        for i in &p.inputs {
                            let v = inputs.entry(i.key.clone()).or_default();
                            ui.add_sized(
                                [w, h],
                                egui::TextEdit::singleline(v)
                                    .hint_text(&i.label)
                                    .align(egui::Align2::LEFT_CENTER),
                            )
                            .on_hover_text(&i.hint);
                        }
                    });
                }
                if ui
                    .add_enabled_ui(can_run, |ui| {
                        ui.add_sized([full, 30.0], look::primary(&count(&p.label)))
                    })
                    .inner
                    .clicked()
                {
                    run = Some(p.id.clone());
                }
            }
            // one-box buttons: [box][Follow]
            for a in m
                .actions
                .iter()
                .filter(|a| !a.primary && a.inputs.len() == 1 && !a.needs_form())
            {
                let i = &a.inputs[0];
                row(ui, |ui| {
                    if ui
                        .add_enabled(
                            can_run,
                            egui::Button::new(&a.label).min_size(egui::vec2(96.0, h)),
                        )
                        .clicked()
                    {
                        run = Some(a.id.clone());
                    }
                    let v = inputs.entry(i.key.clone()).or_default();
                    ui.add(
                        egui::TextEdit::singleline(v)
                            .hint_text(&i.hint)
                            .desired_width(ui.available_width())
                            .min_size(egui::vec2(0.0, h)),
                    );
                });
            }
        }
        if let Some(a) = run {
            self.start_action(m, &a, ids.clone());
        }
        let one = self.focus.is_some() && !ids.is_empty();
        // Alias: [box][Set Alias]
        let mut set_alias = false;
        row(ui, |ui| {
            set_alias = ui
                .add_enabled(
                    one,
                    egui::Button::new(count("Set Alias")).min_size(egui::vec2(96.0, h)),
                )
                .clicked();
            ui.add_enabled(
                one,
                egui::TextEdit::singleline(&mut self.alias_buf)
                    .hint_text("Alias")
                    .desired_width(ui.available_width())
                    .min_size(egui::vec2(0.0, h)),
            );
        });
        if set_alias {
            let v = self.alias_buf.trim().to_string();
            self.edit_all(&ids, |a| a.name = v.clone());
            self.say(format!("Alias set for {}", plural(ids.len())), false);
        }
        // Description fills the height left over the four buttons
        let buttons_h = 2.0 * h + 3.0 * ui.spacing().item_spacing.y;
        let note_h = (ui.available_height() - buttons_h).max(60.0);
        ui.add_enabled_ui(one, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("description")
                .max_height(note_h)
                .min_scrolled_height(note_h)
                .show(ui, |ui| {
                    ui.add_sized(
                        egui::vec2(ui.available_width(), note_h),
                        egui::TextEdit::multiline(&mut self.notes_buf).hint_text("Description"),
                    );
                });
        });
        let mut choice = None;
        let mut set_notes = false;
        let mut edit = false;
        ui.horizontal(|ui| {
            set_notes = ui
                .add_enabled(
                    one,
                    egui::Button::new(count("Set Description")).min_size(egui::vec2(half, h)),
                )
                .clicked();
            ui.add_enabled_ui(!ids.is_empty(), |ui| {
                ui.allocate_ui(egui::vec2(half, h), |ui| {
                    let r = ui.add_sized([half, h], egui::Button::new("Account Utilities"));
                    caret(ui, &r);
                    egui::Popup::menu(&r).show(|ui| {
                        choice = self.actions_menu(ui, m, &ids, false);
                    });
                });
            });
        });
        ui.horizontal(|ui| {
            edit = ui
                .add_enabled(
                    ids.len() == 1,
                    egui::Button::new("Edit Account").min_size(egui::vec2(half, h)),
                )
                .on_hover_text("every field: cookie, password, 2FA, mode, FPS")
                .clicked();
            ui.add_enabled_ui(!ids.is_empty(), |ui| {
                ui.allocate_ui(egui::vec2(half, h), |ui| {
                    let r = ui.add_sized([half, h], egui::Button::new("Account Control"));
                    caret(ui, &r);
                    egui::Popup::menu(&r).show(|ui| {
                        choice = choice.take().or(self.actions_menu(ui, m, &ids, true));
                    });
                });
            });
        });
        if set_notes {
            let v = self.notes_buf.clone();
            self.edit_all(&ids, |a| a.notes = v.clone());
            self.say(format!("Description set for {}", plural(ids.len())), false);
        }
        if edit {
            self.edit_open = true;
        }
        if let Some(c) = choice {
            self.apply_menu(m, ids, c);
        }
    }

    /// Account Utilities (`control` false): the game's everyday buttons, Mode, groups, Copy.
    /// Account Control (`control` true): Stop, the buttons that change the account on the
    /// game's side (each asks first), Remove.
    fn actions_menu(
        &mut self,
        ui: &mut egui::Ui,
        m: &Module,
        ids: &[String],
        control: bool,
    ) -> Option<MenuChoice> {
        let n = ids.len();
        let count = |label: &str| {
            if n > 1 {
                format!("{label} ({n})")
            } else {
                label.to_string()
            }
        };
        let mut choice = None;
        // the buttons already on the right (Join Server, Follow) and the left (Open Browser) stay off
        let on_deck = |a: &module::Action| {
            a.primary || a.id == "browser" || (a.inputs.len() == 1 && !a.needs_form())
        };
        for a in m
            .actions
            .iter()
            .filter(|a| !on_deck(a) && a.confirm.is_empty() != control)
        {
            if ui.button(count(&a.label)).clicked() {
                choice = Some(MenuChoice::Run(a.id.clone()));
            }
        }
        if control {
            if ids.iter().any(|id| self.running.contains_key(id))
                && ui.button(count("Stop")).clicked()
            {
                choice = Some(MenuChoice::Stop);
            }
            ui.separator();
            if ui
                .button(RichText::new(count("Remove")).color(ui.visuals().error_fg_color))
                .clicked()
            {
                choice = Some(MenuChoice::Remove);
            }
        } else {
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
            });
        }
        if choice.is_some() {
            ui.close();
        }
        choice
    }

    /// Edit Account: every field of the chosen account in a window of its own (a new account
    /// from Add manually opens here too).
    fn edit_dialog(&mut self, ctx: &egui::Context) {
        if !(self.edit_open || self.draft_new) || self.draft.is_none() {
            self.edit_open = false;
            return;
        }
        let Some(m) = self.game.clone().and_then(|g| self.module(&g).cloned()) else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::unique("edit")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(if self.draft_new {
                "Add Account"
            } else {
                "Edit Account"
            });
            egui::ScrollArea::vertical()
                .max_height(520.0)
                .show(ui, |ui| {
                    self.editor(ui, &m);
                });
            if !self.draft_new
                && (ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)))
            {
                close = true;
            }
        });
        if close {
            // unsaved edits go: the window shows the saved account again
            if let Some(id) = self.draft.as_ref().map(|d| d.id.clone()) {
                self.draft = self.accounts.iter().find(|a| a.id == id).cloned();
            }
            self.edit_open = false;
        }
        // the new account was added or cancelled
        if !self.draft_new && self.draft.is_none() {
            self.edit_open = false;
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, m: &Module) {
        if self.selected.len() > 1 {
            ui.label(format!("{} accounts chosen. The buttons above run for all of them; right-click them for everything else.", self.selected.len()));
            return;
        }
        let Some(mut d) = self.draft.take() else {
            ui.label(RichText::new("Choose an account.").weak());
            return;
        };
        let now = unix_now();
        let saved = self.accounts.iter().find(|a| a.id == d.id).cloned();
        let mut copy = None;
        egui::Grid::new("fields")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Alias").weak());
                let fallback = self.title(&Account {
                    name: String::new(),
                    ..d.clone()
                });
                ui.add(
                    egui::TextEdit::singleline(&mut d.name)
                        .hint_text(fallback)
                        .desired_width(ui.available_width()),
                );
                ui.end_row();
                ui.label(RichText::new("Group").weak());
                let groups = self.groups(&m.id);
                row(ui, |ui| {
                    if !groups.is_empty() {
                        egui::ComboBox::from_id_salt("groups")
                            .selected_text("")
                            .width(20.0)
                            .show_ui(ui, |ui| {
                                for g in groups {
                                    if ui.selectable_label(d.group == g, &g).clicked() {
                                        d.group = g;
                                    }
                                }
                            });
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut d.group)
                            .hint_text(NO_GROUP)
                            .desired_width(ui.available_width()),
                    );
                });
                ui.end_row();
                for f in &m.fields {
                    ui.label(RichText::new(&f.label).weak());
                    let map = if f.secret {
                        &mut d.secrets
                    } else {
                        &mut d.fields
                    };
                    let mut v = map.get(&f.key).cloned().unwrap_or_default();
                    let key = (d.id.clone(), f.key.clone());
                    if f.kind == "choice" && !f.choices.is_empty() {
                        let shown = if v.is_empty() {
                            f.choices[0].clone()
                        } else {
                            v.clone()
                        };
                        egui::ComboBox::from_id_salt(("choice", &f.key))
                            .selected_text(&shown)
                            .show_ui(ui, |ui| {
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
                        row(ui, |ui| {
                            // Show and Copy only for a secret that is there
                            if f.secret && !v.is_empty() {
                                if ui.small_button("Copy").clicked() {
                                    copy = Some(v.clone());
                                }
                                if ui
                                    .small_button(if show { "Hide" } else { "Show" })
                                    .clicked()
                                    && !self.shown_secrets.remove(&key)
                                {
                                    self.shown_secrets.insert(key);
                                }
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut v)
                                    .password(!show)
                                    .hint_text(&f.hint)
                                    .desired_width(ui.available_width()),
                            );
                        });
                    }
                    if v.is_empty() {
                        map.remove(&f.key);
                    } else {
                        map.insert(f.key.clone(), v.clone());
                    }
                    ui.end_row();
                    if f.kind == "totp" && !v.is_empty() {
                        ui.label("");
                        ui.horizontal(|ui| match totp::code(&v, now) {
                            Some(c) => {
                                ui.label(
                                    RichText::new(format!("{} {}", &c[..3], &c[3..]))
                                        .monospace()
                                        .size(16.0)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(format!("{} s", totp::seconds_left(now))).weak(),
                                );
                                if ui.small_button("Copy").clicked() {
                                    copy = Some(c);
                                }
                                ui.ctx().request_repaint_after(Duration::from_millis(500));
                            }
                            None => {
                                ui.label(
                                    RichText::new("not a valid 2FA secret")
                                        .color(ui.visuals().error_fg_color),
                                );
                            }
                        });
                        ui.end_row();
                    }
                }
            });
        ui.add(
            egui::TextEdit::multiline(&mut d.notes)
                .hint_text("Notes")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        if let Some(c) = copy {
            ui.ctx().copy_text(c);
            self.say("Copied", false);
        }
        // Save and Undo only when something changed
        let dirty = self.draft_new || saved.as_ref() != Some(&d);
        let mut keep = true;
        let mut save_now = false;
        if dirty {
            ui.horizontal(|ui| {
                if ui
                    .button(if self.draft_new { "Add" } else { "Save" })
                    .clicked()
                    || ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S))
                {
                    save_now = true;
                }
                if ui
                    .button(if self.draft_new { "Cancel" } else { "Undo" })
                    .clicked()
                {
                    match &saved {
                        Some(s) => d = s.clone(),
                        None => keep = false,
                    }
                }
            });
        }
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

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    User,
    Alias,
    Group,
    Mode,
    Notes,
    Watch,
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
    if n == 1 {
        "1 account".into()
    } else {
        format!("{n} accounts")
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A row filling the width it has, laid out from the right: buttons first, then a box taking the rest.
fn row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), h),
        egui::Layout::right_to_left(egui::Align::Center),
        add,
    );
}

/// Saved values for the main button's boxes (AEAM's Bookmarks), kept per game.
fn bookmarks_menu(
    ui: &mut egui::Ui,
    store: Option<&Store>,
    game: &str,
    primary: &module::Action,
    inputs: &mut Values,
    name: &mut String,
    width: f32,
) {
    let Some(store) = store else { return };
    let key = format!("bookmarks.{game}");
    let load = || -> Vec<(String, Values)> {
        store
            .setting(&key)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    };
    let save = |list: &Vec<(String, Values)>| {
        store.set_setting(
            &key,
            &serde_json::to_string(list).expect("a list of strings serializes"),
        );
    };
    ui.allocate_ui(egui::vec2(width, ui.spacing().interact_size.y), |ui| {
        ui.menu_button("Bookmarks", |ui| {
            let mut list = load();
            let mut changed = false;
            let mut remove = None;
            for (i, (label, values)) in list.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.button(label).clicked() {
                        for inp in &primary.inputs {
                            inputs.insert(
                                inp.key.clone(),
                                values.get(&inp.key).cloned().unwrap_or_default(),
                            );
                        }
                        ui.close();
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
                changed = true;
            }
            if !list.is_empty() {
                ui.separator();
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("Name")
                        .desired_width(140.0),
                );
                let has_values = primary
                    .inputs
                    .iter()
                    .any(|i| inputs.get(&i.key).is_some_and(|v| !v.trim().is_empty()));
                if ui
                    .add_enabled(
                        has_values && !name.trim().is_empty(),
                        egui::Button::new("Save"),
                    )
                    .clicked()
                {
                    let values: Values = primary
                        .inputs
                        .iter()
                        .filter_map(|i| {
                            inputs
                                .get(&i.key)
                                .map(|v| (i.key.clone(), v.trim().to_string()))
                        })
                        .collect();
                    list.retain(|(l, _)| l != name.trim());
                    list.push((name.trim().to_string(), values));
                    name.clear();
                    changed = true;
                }
            });
            if changed {
                save(&list);
            }
        })
        .response
        .on_hover_text("Saved places and servers");
    });
}

/// AEAM's small down arrow at the right end of a menu button (drawn: the font has no arrow).
fn caret(ui: &egui::Ui, r: &egui::Response) {
    let c = egui::pos2(r.rect.right() - 12.0, r.rect.center().y);
    let color = if r.enabled() {
        look::colors().text
    } else {
        look::colors().muted
    };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 3.0),
        ],
        color,
        egui::Stroke::NONE,
    ));
}

/// A small filled circle (drawn, not a glyph: the font has none) before a status.
fn dot(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
    resp
}

/// One value from a helper into an account: `name`, a secret or a plain field (an empty value
/// removes it).
fn set_value(a: &mut crate::store::Account, k: &str, v: &str, secret: bool) {
    if k == "name" {
        a.name = v.to_string();
        return;
    }
    let (to, from) = if secret {
        (&mut a.secrets, &mut a.fields)
    } else {
        (&mut a.fields, &mut a.secrets)
    };
    from.remove(k);
    if v.is_empty() {
        to.remove(k);
    } else {
        to.insert(k.to_string(), v.to_string());
    }
}

/// A helper's error saying the game's servers refused for too many requests.
fn is_rate_limit(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("rate limit") || e.contains("too many requests") || e.contains("429")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// An account from Add or Import is kept when its action works, merged into one with the same
    /// user ID, and dropped when the action fails or fills in no name.
    #[test]
    fn added_accounts_kept_merged_or_dropped() {
        let mods = std::env::temp_dir().join(format!("alyae-addmods-{}", store::new_id()));
        std::fs::create_dir_all(mods.join("t")).unwrap();
        std::fs::write(
            mods.join("t/module.toml"),
            "id = \"t\"\nname = \"T\"\ntitle_field = \"username\"\n\
             [add]\naction = \"login\"\nimport_field = \"cookie\"\nimport_action = \"check\"\nsame = \"user_id\"\n\
             [[field]]\nkey = \"username\"\nlabel = \"U\"\n[[field]]\nkey = \"user_id\"\nlabel = \"I\"\n\
             [[field]]\nkey = \"cookie\"\nlabel = \"C\"\nsecret = true\n\
             [[action]]\nid = \"login\"\nlabel = \"Log in\"\n[[action]]\nid = \"check\"\nlabel = \"Refresh\"\n",
        )
        .unwrap();
        let (mut h, dir) = window_with("t", mods.clone());
        let tx = h.state().tx.clone();
        let make = |id: &str, user: &str, uid: &str, cookie: &str| {
            let mut a = Account {
                id: id.into(),
                game: "t".into(),
                ..Default::default()
            };
            set_value(&mut a, "username", user, false);
            set_value(&mut a, "user_id", uid, false);
            set_value(&mut a, "cookie", cookie, true);
            a
        };
        for a in [
            make("old", "me", "5", "old-cookie"),
            make("again", "me", "5", "new-cookie"),
            make("fresh", "alt", "6", "c6"),
            make("closed", "", "", ""),
            make("bad", "x", "7", "c7"),
        ] {
            h.state_mut().save(&a);
        }
        h.state_mut().adding = ["again", "fresh", "closed", "bad"].map(String::from).into();
        for (id, ok) in [
            ("again", true),
            ("fresh", true),
            ("closed", true),
            ("bad", false),
        ] {
            tx.send(Event::Ended {
                account: id.into(),
                action: "login".into(),
                ok,
            })
            .unwrap();
        }
        h.run();
        let mut ids: Vec<String> = h.state().accounts.iter().map(|a| a.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, ["fresh", "old"]);
        let old = h.state().accounts.iter().find(|a| a.id == "old").unwrap();
        assert_eq!(old.value("cookie"), "new-cookie");
        assert!(h.state().adding.is_empty());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(mods);
    }

    #[test]
    fn rate_limit_backs_off() {
        assert!(is_rate_limit(
            "Roblox is rate limiting: try again in a minute"
        ));
        assert!(!is_rate_limit("online status: Roblox answered 500"));
        let (mut h, dir) = window("roblox");
        let tx = h.state().tx.clone();
        for want in [60, 120, 240, 480, 600, 600] {
            tx.send(Event::WatchError(
                "Roblox is rate limiting: try again in a minute".into(),
            ))
            .unwrap();
            h.run();
            assert_eq!(h.state().watch_backoff, Duration::from_secs(want));
        }
        tx.send(Event::Watch(Values::default())).unwrap();
        h.run();
        assert_eq!(h.state().watch_backoff, Duration::ZERO);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The window on a fresh data folder, opened on `game`, with the repository's modules.
    fn window(game: &str) -> (Harness<'static, App>, PathBuf) {
        window_with(
            game,
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../modules")),
        )
    }

    fn window_with(game: &str, modules_dir: PathBuf) -> (Harness<'static, App>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("alyae-ui-{}", store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir.join("t.db"), crate::vault::Vault::from_key(&[3; 32])).unwrap();
        s.set_setting("last_game", game);
        let modules = module::load_all(&[modules_dir]);
        let d = dir.clone();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 580.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| App::new(&cc.egui_ctx, Ok(s), d, modules));
        h.run();
        (h, dir)
    }

    #[test]
    fn add_account() {
        let (mut h, dir) = window("generic");
        h.get_by_label("Add Account").click();
        h.run();
        h.get_by_label("Add").click();
        h.run();
        assert_eq!(
            h.state().accounts.len(),
            1,
            "Add did not save the new account"
        );
        // the sort arrows draw (a glyph the font lacks stops the window)
        h.get_by_label("DESCRIPTION").click();
        h.run();
        h.get_by_label("DESCRIPTION A-Z").click();
        h.run();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A button with a confirmation opens its form; the form's button runs it, for the chosen accounts.
    #[test]
    fn form_runs_after_confirming() {
        let mods = std::env::temp_dir().join(format!("alyae-mods-{}", store::new_id()));
        std::fs::create_dir_all(mods.join("t")).unwrap();
        let marker = mods.join("ran");
        // creates the marker file: `touch` under sh, `type nul >` under cmd (no `touch` on Windows)
        let touch = if cfg!(windows) { "type nul >" } else { "touch" };
        // TOML literal string: a Windows path's backslashes stay as they are
        let command = format!("{touch} \"{}\"", marker.display());
        std::fs::write(
            mods.join("t/module.toml"),
            format!(
                "id = \"t\"\nname = \"T\"\ntitle_field = \"login\"\n[[field]]\nkey = \"login\"\nlabel = \"Login\"\n\
                 [[action]]\nid = \"wipe\"\nlabel = \"Wipe\"\nconfirm = \"Sure?\"\ncommand = '{command}'\n"
            ),
        )
        .unwrap();
        let (mut h, dir) = window_with("t", mods.clone());
        h.get_by_label("Add Account").click();
        h.run();
        h.get_by_label("Add").click();
        h.run();
        // from the right-click menu of the account's row
        h.get_by_label("(unnamed)").click_secondary();
        h.run();
        h.get_by_label("Wipe").click();
        h.run();
        assert!(h.state().form.is_some(), "the form did not open");
        assert!(!marker.exists(), "ran before it was confirmed");
        h.get_all_by_label("Wipe").last().unwrap().click();
        h.run();
        assert!(h.state().form.is_none());
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "the confirmed action did not run");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(mods);
    }

    /// Right-clicking the Online heading switches its text off and on.
    #[test]
    fn online_text_toggles() {
        let (mut h, dir) = window("roblox");
        assert!(h.state().watch_text);
        h.get_by_label("ONLINE").click_secondary();
        h.run();
        h.get_by_label("Online: show text").click();
        h.run();
        assert!(!h.state().watch_text, "Show text did not switch off");
        // the column is gone; the Username heading switches it back
        assert!(h.query_by_label("ONLINE").is_none());
        h.get_by_label("USERNAME").click_secondary();
        h.run();
        h.get_by_label("Online: show text").click();
        h.run();
        assert!(h.state().watch_text, "Show text did not switch back on");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The accounts file is reachable from the accounts window (Alyae opens on the last game, so
    /// the start screen is rarely seen): its button opens the dialog on Save, switchable to
    /// Bring in.
    #[test]
    fn accounts_file_from_the_accounts_window() {
        let (mut h, dir) = window("generic");
        h.get_by_label("Accounts file").click();
        h.run();
        assert!(h.state().transfer.as_ref().is_some_and(|t| t.export));
        h.get_by_label("Bring in from a file").click();
        h.run();
        assert!(h.state().transfer.as_ref().is_some_and(|t| !t.export));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// AEAM's right side: Set Alias and Set Description write to the chosen account, Edit
    /// Account opens every field in its own window.
    #[test]
    fn set_alias_description_and_edit() {
        let (mut h, dir) = window("generic");
        let mut a = Account {
            id: "g1".into(),
            game: "generic".into(),
            ..Default::default()
        };
        a.fields.insert("login".into(), "me@x".into());
        h.state_mut().save(&a);
        h.run();
        h.get_by_label("me@x").click();
        h.run();
        h.state_mut().alias_buf = "Main".into();
        h.state_mut().notes_buf = "farm alt".into();
        h.get_by_label("Set Alias").click();
        h.run();
        h.get_by_label("Set Description").click();
        h.run();
        let saved = h.state().store.as_ref().unwrap().list().unwrap().0;
        assert_eq!(
            (saved[0].name.as_str(), saved[0].notes.as_str()),
            ("Main", "farm alt")
        );
        h.get_by_label("Edit Account").click();
        h.run();
        assert!(h.state().edit_open);
        h.get_by_label("Close").click();
        h.run();
        assert!(!h.state().edit_open);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_roblox_account() {
        let (mut h, dir) = window("roblox");
        // the by-hand form: where the module has a login window, a click would open it, so the
        // form is in the button's right-click menu
        let has_login = h
            .state()
            .module("roblox")
            .and_then(|m| m.add.as_ref())
            .is_some_and(|f| !f.action.is_empty());
        if has_login {
            h.get_by_label("Add Account").click_secondary();
            h.run();
            h.get_by_label("Add manually").click();
        } else {
            h.get_by_label("Add Account").click();
        }
        h.run();
        h.get_by_label("Add").click();
        h.run();
        assert_eq!(
            h.state().accounts.len(),
            1,
            "Add did not save the new account"
        );
        assert_eq!(h.state().accounts[0].game, "roblox");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_account_shows_non_fatal_warning() {
        let dir = std::env::temp_dir().join(format!("alyae-ui-bad-{}", store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.db");
        let s = Store::open(&db_path, crate::vault::Vault::from_key(&[3; 32])).unwrap();
        s.save(&Account {
            id: "good1".into(),
            game: "generic".into(),
            name: "good".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_setting("last_game", "generic");
        {
            let db = rusqlite::Connection::open(&db_path).unwrap();
            let bad_blob = crate::vault::Vault::from_key(&[9; 32]).seal(b"{\"password\":\"x\"}");
            db.execute(
                "INSERT INTO accounts (id, game, name, secrets) VALUES ('bad1', 'generic', 'bad', ?1)",
                rusqlite::params![bad_blob],
            )
            .unwrap();
        }
        let modules = module::load_all(&[PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../modules"
        ))]);
        let d = dir.clone();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 580.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| App::new(&cc.egui_ctx, Ok(s), d, modules));
        h.run();
        assert!(h.state().fatal.is_none());
        assert_eq!(h.state().accounts.len(), 1);
        assert_eq!(h.state().accounts[0].id, "good1");
        let (msg, bad) = h
            .state()
            .message
            .as_ref()
            .expect("expected warning message");
        assert!(*bad);
        assert!(msg.contains("bad1"), "got: {msg}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

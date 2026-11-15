//! Game modules: a folder with `module.toml` naming the fields an account of the game keeps and
//! the buttons it offers. A button runs a command template, or the module's helper program which
//! speaks JSON lines (see PLAN.md, "Helper protocol").

use crate::store::Account;
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

#[derive(Clone, Debug, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub secret: bool,
    /// "text" (default), "totp" (shows the live code), "choice"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub hint: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Input {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub hint: String,
    /// the values to pick from, when the input is a choice
    #[serde(default)]
    pub choices: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    /// a command line with {field} placeholders; without one the helper runs the action
    #[serde(default)]
    pub command: String,
    /// the account counts as running while the action runs
    #[serde(default)]
    pub runs: bool,
    /// shown as the big button (AEAM's Join Server)
    #[serde(default)]
    pub primary: bool,
    #[serde(default, rename = "input")]
    pub inputs: Vec<Input>,
    /// asked before the action runs (it changes the accounts)
    #[serde(default)]
    pub confirm: String,
}

impl Action {
    /// Asks for its values (or a confirmation) in a small form before running; one plain input
    /// is a box beside the button instead.
    pub fn needs_form(&self) -> bool {
        !self.confirm.is_empty()
            || (!self.primary
                && (self.inputs.len() > 1 || self.inputs.iter().any(|i| !i.choices.is_empty())))
    }
}

/// A helper action Ludae runs for all of a game's accounts every `every` seconds while the game is
/// shown; its answers fill the column `column`.
#[derive(Clone, Debug, Deserialize)]
pub struct Watch {
    pub action: String,
    pub every: u64,
    pub column: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Module {
    /// the game's id: accounts belong to it, so the Linux and Windows modules of one game share them
    pub id: String,
    pub name: String,
    /// "linux" or "windows" when the module only works there
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub description: String,
    /// helper program: a path beside module.toml, beside Ludae, or a name on PATH
    #[serde(default)]
    pub helper: String,
    /// the field an account is named after when it gets no name of its own
    #[serde(default)]
    pub title_field: String,
    #[serde(default, rename = "field")]
    pub fields: Vec<Field>,
    #[serde(default, rename = "action")]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub watch: Option<Watch>,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Module {
    pub fn parse(text: &str, dir: &Path) -> Result<Self, String> {
        let mut m: Module = toml::from_str(text).map_err(|e| e.to_string())?;
        if m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "module id '{}' must be letters, digits, - or _",
                m.id
            ));
        }
        m.dir = dir.to_path_buf();
        Ok(m)
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    pub fn is_secret(&self, key: &str) -> bool {
        self.field(key).is_some_and(|f| f.secret)
    }

    fn helper_path(&self) -> Option<PathBuf> {
        if self.helper.is_empty() {
            return None;
        }
        let exe = |p: PathBuf| {
            if cfg!(windows) {
                p.with_extension("exe")
            } else {
                p
            }
        };
        let beside_module = exe(self.dir.join(&self.helper));
        if beside_module.is_file() {
            return Some(beside_module);
        }
        let beside_ludae = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| exe(d.join(&self.helper))));
        if let Some(p) = beside_ludae.filter(|p| p.is_file()) {
            return Some(p);
        }
        Some(PathBuf::from(&self.helper))
    }
}

/// Every module in `dirs` (later ones replace earlier ones of the same id), with the problems of
/// those that did not load.
pub fn load_all(dirs: &[PathBuf]) -> (Vec<Module>, Vec<String>) {
    let mut mods: Vec<Module> = Vec::new();
    let mut problems = Vec::new();
    for root in dirs {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for dir in entries {
            let file = dir.join("module.toml");
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            match Module::parse(&text, &dir) {
                Ok(m) if !m.platform.is_empty() && m.platform != std::env::consts::OS => {}
                Ok(m) => {
                    mods.retain(|o| o.id != m.id);
                    mods.push(m);
                }
                Err(e) => problems.push(format!("{}: {e}", file.display())),
            }
        }
    }
    mods.sort_by(|a, b| (a.id != "generic", &a.name).cmp(&(b.id != "generic", &b.name)));
    (mods, problems)
}

/// `{key}` in `template` replaced by the account's value (inputs first); `{{` and `}}` are literal braces.
pub fn fill(template: &str, account: &Account, inputs: &crate::store::Values) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i].replace("}}", "}"));
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('{') {
            out.push('{');
            rest = r;
            continue;
        }
        let Some(j) = rest.find('}') else {
            out.push('{');
            break;
        };
        let key = &rest[..j];
        out.push_str(
            inputs
                .get(key)
                .map_or_else(|| account.value(key), String::as_str),
        );
        rest = &rest[j + 1..];
    }
    out.push_str(&rest.replace("}}", "}"));
    out
}

/// What a running action tells the window.
#[derive(Debug)]
pub enum Event {
    Set {
        account: String,
        values: crate::store::Values,
    },
    Status {
        account: String,
        text: String,
    },
    Error {
        account: String,
        text: String,
    },
    Ended {
        account: String,
        action: String,
        ok: bool,
    },
    /// a watch's values, by account id
    Watch(crate::store::Values),
    WatchEnded,
}

pub struct Running {
    pub action: String,
    pub child: Child,
    stdin: Option<ChildStdin>,
}

impl Running {
    /// a plain command (no helper to report its end)
    pub fn is_command(&self) -> bool {
        self.stdin.is_none()
    }

    /// Asks the helper to stop its game; a plain command is killed.
    pub fn stop(&mut self) {
        match self.stdin.as_mut() {
            Some(s) => {
                let _ = s.write_all(b"stop\n").and_then(|_| s.flush());
            }
            None => {
                let _ = self.child.kill();
            }
        }
    }
}

fn shell(cmd: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn account_json(a: &Account) -> serde_json::Map<String, serde_json::Value> {
    a.fields
        .iter()
        .chain(a.secrets.iter())
        .map(|(k, v)| (k.clone(), v.clone().into()))
        .collect()
}

/// The module's helper started for `action`, with its data folder.
fn spawn_helper(
    module: &Module,
    action: &str,
    label: &str,
    data_dir: &Path,
) -> Result<(Child, PathBuf, PathBuf), String> {
    let helper = module
        .helper_path()
        .ok_or_else(|| format!("{} has no helper for '{label}'", module.name))?;
    let dir = data_dir.join("games").join(&module.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let child = Command::new(&helper)
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", helper.display()))?;
    Ok((child, dir, helper))
}

/// Runs the module's watch once for `accounts`; its values arrive as one `Event::Watch`, then
/// `Event::WatchEnded`.
pub fn watch(
    module: &Module,
    w: &Watch,
    accounts: &[&Account],
    data_dir: &Path,
    tx: Sender<Event>,
    wake: impl Fn() + Send + 'static,
) -> Result<Child, String> {
    let (mut child, dir, helper) = spawn_helper(module, &w.action, &w.column, data_dir)?;
    let list: Vec<serde_json::Value> = accounts
        .iter()
        .map(|a| serde_json::json!({ "id": a.id, "account": account_json(a) }))
        .collect();
    let request = serde_json::json!({ "accounts": list, "data_dir": dir });
    // stdin closes here: a watch answers once and ends
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| format!("{} did not take the request: {e}", helper.display()))?;
    drop(stdin);
    let out = child.stdout.take().expect("piped stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(map) = v.get("watch").and_then(|m| m.as_object()) {
                let values = map
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect();
                let _ = tx.send(Event::Watch(values));
                wake();
            }
        }
        let _ = tx.send(Event::WatchEnded);
        wake();
    });
    Ok(child)
}

/// Starts `action` for `account`; its output arrives as events on `tx` (and `wake` is called so
/// the window redraws). None: a short command that reports its end as an event.
pub fn start(
    module: &Module,
    action: &Action,
    account: &Account,
    inputs: &crate::store::Values,
    data_dir: &Path,
    tx: Sender<Event>,
    wake: impl Fn() + Send + 'static,
) -> Result<Option<Running>, String> {
    let id = account.id.clone();
    if !action.command.is_empty() {
        let cmd = fill(&action.command, account, inputs);
        let mut child = shell(&cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run '{cmd}': {e}"))?;
        if !action.runs {
            let act = action.id.clone();
            std::thread::spawn(move || {
                let ok = child.wait().is_ok_and(|s| s.success());
                let _ = tx.send(Event::Ended {
                    account: id,
                    action: act,
                    ok,
                });
                wake();
            });
            return Ok(None);
        }
        return Ok(Some(Running {
            action: action.id.clone(),
            child,
            stdin: None,
        }));
    }
    let (mut child, dir, helper) = spawn_helper(module, &action.id, &action.label, data_dir)?;
    let request = serde_json::json!({
        "id": account.id,
        "name": account.name,
        "account": account_json(account),
        "inputs": inputs,
        "data_dir": dir,
    });
    let mut stdin = child.stdin.take();
    if let Some(s) = stdin.as_mut() {
        s.write_all(format!("{request}\n").as_bytes())
            .and_then(|_| s.flush())
            .map_err(|e| format!("{} did not take the request: {e}", helper.display()))?;
    }
    let out = child.stdout.take().expect("piped stdout");
    let act = action.id.clone();
    std::thread::spawn(move || {
        let mut ok = true;
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let ev = if let Some(set) = v.get("set").and_then(|s| s.as_object()) {
                let values = set
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect();
                Event::Set {
                    account: id.clone(),
                    values,
                }
            } else if let Some(t) = v.get("status").and_then(|s| s.as_str()) {
                Event::Status {
                    account: id.clone(),
                    text: t.to_string(),
                }
            } else if let Some(t) = v.get("error").and_then(|s| s.as_str()) {
                ok = false;
                Event::Error {
                    account: id.clone(),
                    text: t.to_string(),
                }
            } else {
                continue;
            };
            let _ = tx.send(ev);
            wake();
        }
        let _ = tx.send(Event::Ended {
            account: id,
            action: act,
            ok,
        });
        wake();
    });
    Ok(Some(Running {
        action: action.id.clone(),
        child,
        stdin,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_fill() {
        let m = Module::parse(
            r#"
id = "generic"
name = "Any game"
title_field = "login"
[[field]]
key = "login"
label = "Login"
[[field]]
key = "password"
label = "Password"
secret = true
[[action]]
id = "play"
label = "Launch"
command = "run {login} {{x}} {place}"
runs = true
[[action.input]]
key = "place"
label = "Place"
"#,
            Path::new("/m"),
        )
        .unwrap();
        assert!(m.is_secret("password") && !m.is_secret("login"));
        assert_eq!(m.actions[0].inputs[0].key, "place");
        let mut a = Account::default();
        a.fields.insert("login".into(), "me".into());
        let mut inputs = crate::store::Values::new();
        inputs.insert("place".into(), "42".into());
        assert_eq!(fill(&m.actions[0].command, &a, &inputs), "run me {x} 42");
        assert!(Module::parse("id = \"a b\"\nname = \"x\"", Path::new("/")).is_err());
        // one plain input: a box beside the button; a choice or a confirmation: a form
        assert!(!m.actions[0].needs_form());
        let f = Module::parse(
            "id = \"x\"\nname = \"x\"\n[watch]\naction = \"p\"\nevery = 15\ncolumn = \"Online\"\n\
             [[action]]\nid = \"a\"\nlabel = \"A\"\n[[action.input]]\nkey = \"k\"\nlabel = \"K\"\nchoices = [\"1\", \"2\"]\n\
             [[action]]\nid = \"b\"\nlabel = \"B\"\nconfirm = \"Sure?\"\n",
            Path::new("/"),
        )
        .unwrap();
        assert!(f.actions.iter().all(Action::needs_form));
        assert_eq!(f.watch.unwrap().column, "Online");
    }
}

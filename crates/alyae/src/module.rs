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

/// A helper action Alyae runs for all of a game's accounts every `every` seconds while the game is
/// shown; its answers fill the column `column`.
#[derive(Clone, Debug, Deserialize)]
pub struct Watch {
    pub action: String,
    pub every: u64,
    pub column: String,
    /// the colour of the dot beside a value, by how the value starts ("In game" = "#3dd68c");
    /// values matching none get a grey dot
    #[serde(default)]
    pub colors: std::collections::BTreeMap<String, String>,
    /// the account fields the helper gets (only plain fields; none declared: only the account id)
    #[serde(default)]
    pub fields: Vec<String>,
    /// at most one secret field the helper also gets, when the check cannot work without it
    #[serde(default)]
    pub secret: Option<String>,
}

impl Watch {
    /// What the helper gets of `a`: the declared plain fields and the one declared secret, nothing
    /// else (a watch runs every few seconds, so it carries as little as it can).
    pub fn account_json(
        &self,
        module: &Module,
        a: &Account,
    ) -> serde_json::Map<String, serde_json::Value> {
        let plain = self
            .fields
            .iter()
            .filter(|k| !module.is_secret(k))
            .filter_map(|k| a.fields.get(k).map(|v| (k, v)));
        let secret = self
            .secret
            .iter()
            .filter(|k| module.is_secret(k))
            .filter_map(|k| a.secrets.get(k).map(|v| (k, v)));
        plain
            .chain(secret)
            .map(|(k, v)| (k.clone(), v.clone().into()))
            .collect()
    }

    pub fn color_of(&self, value: &str) -> Option<&str> {
        self.colors
            .iter()
            .filter(|(start, _)| value.starts_with(start.as_str()))
            .max_by_key(|(start, _)| start.len())
            .map(|(_, c)| c.as_str())
    }
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
    /// helper program: a path beside module.toml, beside Alyae, or a name on PATH
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
    /// a helper action run (and waited for) before an account is removed, to delete its files
    #[serde(default)]
    pub on_remove: String,
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
        let beside_alyae = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| exe(d.join(&self.helper))));
        if let Some(p) = beside_alyae.filter(|p| p.is_file()) {
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

/// The command of an action: `{key}` becomes the account's value or the form's input, `{{` and
/// `}}` stay as braces. A template that is one placeholder alone (`{launch}`) is a command the
/// person wrote and is taken as it is; anywhere else a value is one quoted argument, so a name
/// or password holding `;`, `&` or quotes cannot run anything (Windows' cmd has no safe quoting
/// for `"`, `%`, `!` and `^`: a value with one of those is refused there).
pub fn fill(
    template: &str,
    account: &Account,
    inputs: &crate::store::Values,
) -> Result<String, String> {
    let lookup = |key: &str| {
        inputs
            .get(key)
            .map_or_else(|| account.value(key).to_string(), String::clone)
    };
    let whole = template.trim();
    if let Some(key) = whole.strip_prefix('{').and_then(|r| r.strip_suffix('}'))
        && !key.contains(['{', '}'])
    {
        return Ok(lookup(key));
    }
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
        out.push_str(&quote(&lookup(key)).ok_or_else(|| {
            format!("the value of {key} has a character a command cannot take safely")
        })?);
        rest = &rest[j + 1..];
    }
    out.push_str(&rest.replace("}}", "}"));
    Ok(out)
}

/// One shell argument holding `v` exactly.
fn quote(v: &str) -> Option<String> {
    if cfg!(windows) {
        (!v.contains(['"', '%', '!', '^', '\n', '\r'])).then(|| format!("\"{v}\""))
    } else {
        Some(format!("'{}'", v.replace('\'', r"'\''")))
    }
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
    /// a watch's helper could not answer
    WatchError(String),
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

#[cfg(windows)]
fn shell(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    // cmd parses its own command line: quotes in `cmd` must reach it unescaped
    c.raw_arg("/C").raw_arg(cmd);
    no_console(&mut c);
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

/// A console program started from the window gets no console window of its own on Windows.
fn no_console(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
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
    let mut cmd = Command::new(&helper);
    no_console(&mut cmd);
    let child = cmd
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // the helper's own messages (and Mocktail's, through it) land in Alyae's log
        .stderr(crate::log::file().map_or_else(Stdio::null, Stdio::from))
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", helper.display()))?;
    Ok((child, dir, helper))
}

fn request_json(account: &Account, inputs: &crate::store::Values, dir: &Path) -> serde_json::Value {
    serde_json::json!({
        "id": account.id,
        "name": account.name,
        "account": account_json(account),
        "inputs": inputs,
        "data_dir": dir,
    })
}

/// Runs the helper's `action` for `account` and waits for it to end: for short actions the
/// window waits on (the module's `on_remove`). Its errors are joined into the Err.
pub fn run_and_wait(
    module: &Module,
    action: &str,
    account: &Account,
    data_dir: &Path,
) -> Result<(), String> {
    let (mut child, dir, helper) = spawn_helper(module, action, action, data_dir)?;
    let request = request_json(account, &Default::default(), &dir);
    // stdin closes after the request: the helper sees no more lines
    let mut stdin = child.stdin.take().expect("piped stdin");
    let sent = stdin.write_all(format!("{request}\n").as_bytes());
    drop(stdin);
    let out = child.stdout.take().expect("piped stdout");
    let errors: Vec<String> = BufReader::new(out)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .filter_map(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .collect();
    let status = child.wait().map_err(|e| e.to_string())?;
    sent.map_err(|e| format!("{} did not take the request: {e}", helper.display()))?;
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    if !status.success() {
        return Err(format!("{} '{action}' failed ({status})", helper.display()));
    }
    Ok(())
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
        .map(|a| serde_json::json!({ "id": a.id, "account": w.account_json(module, a) }))
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
            } else if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                let _ = tx.send(Event::WatchError(e.to_string()));
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
        let cmd = fill(&action.command, account, inputs)?;
        let mut child = shell(&cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run the command: {e}"))?;
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
    let request = request_json(account, inputs, &dir);
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

    /// A value cannot add a command of its own; a template that is one field is the person's
    /// own command, taken as written.
    #[cfg(unix)]
    #[test]
    fn values_are_one_argument() {
        let mut a = Account::default();
        let evil = "x'; echo owned; '";
        a.fields.insert("login".into(), evil.into());
        a.fields
            .insert("launch".into(), "steam steam://rungameid/1".into());
        let none = Default::default();
        assert_eq!(
            fill("{launch}", &a, &none).unwrap(),
            "steam steam://rungameid/1"
        );
        let cmd = fill("printf %s {login}", &a, &none).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), evil);
    }

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
        let quoted = if cfg!(windows) {
            "run \"me\" {x} \"42\""
        } else {
            "run 'me' {x} '42'"
        };
        assert_eq!(fill(&m.actions[0].command, &a, &inputs).unwrap(), quoted);
        assert!(Module::parse("id = \"a b\"\nname = \"x\"", Path::new("/")).is_err());
        // one plain input: a box beside the button; a choice or a confirmation: a form
        assert!(!m.actions[0].needs_form());
        let f = Module::parse(
            "id = \"x\"\nname = \"x\"\non_remove = \"forget\"\n[watch]\naction = \"p\"\nevery = 15\ncolumn = \"Online\"\n\
             [[action]]\nid = \"a\"\nlabel = \"A\"\n[[action.input]]\nkey = \"k\"\nlabel = \"K\"\nchoices = [\"1\", \"2\"]\n\
             [[action]]\nid = \"b\"\nlabel = \"B\"\nconfirm = \"Sure?\"\n",
            Path::new("/"),
        )
        .unwrap();
        assert!(f.actions.iter().all(Action::needs_form));
        assert!(m.on_remove.is_empty());
        assert_eq!(f.on_remove, "forget");
        assert_eq!(f.watch.unwrap().column, "Online");
        // the longest matching start decides a value's colour
        let w: Watch = toml::from_str(
            "action = \"p\"\nevery = 15\ncolumn = \"Online\"\n[colors]\n\"In game\" = \"#00ff00\"\n\"In\" = \"#000000\"\n",
        )
        .unwrap();
        assert_eq!(w.color_of("In game: Rogue Lineage"), Some("#00ff00"));
        assert_eq!(w.color_of("In Studio"), Some("#000000"));
        assert_eq!(w.color_of("Offline"), None);
    }

    #[test]
    fn watch_sends_only_declared_fields() {
        let m = Module::parse(
            "id = \"x\"\nname = \"x\"\n\
             [[field]]\nkey = \"user_id\"\nlabel = \"U\"\n\
             [[field]]\nkey = \"username\"\nlabel = \"N\"\n\
             [[field]]\nkey = \"cookie\"\nlabel = \"C\"\nsecret = true\n\
             [[field]]\nkey = \"password\"\nlabel = \"P\"\nsecret = true\n\
             [watch]\naction = \"p\"\nevery = 15\ncolumn = \"Online\"\nfields = [\"user_id\", \"password\"]\n",
            Path::new("/"),
        )
        .unwrap();
        let mut a = Account::default();
        for (k, v) in [("user_id", "1"), ("username", "me")] {
            a.fields.insert(k.into(), v.into());
        }
        for (k, v) in [("cookie", "c"), ("password", "p")] {
            a.secrets.insert(k.into(), v.into());
        }
        let mut w = m.watch.clone().unwrap();
        // a secret named in `fields` is not sent: secrets only come through `secret`
        let j = w.account_json(&m, &a);
        assert_eq!(j.len(), 1);
        assert_eq!(j["user_id"], "1");
        // one declared secret is added
        w.secret = Some("cookie".into());
        let j = w.account_json(&m, &a);
        assert_eq!(j.len(), 2);
        assert_eq!(j["cookie"], "c");
        // a plain field named as the secret is not sent
        w.secret = Some("username".into());
        assert_eq!(w.account_json(&m, &a).len(), 1);
        // nothing declared: nothing sent
        w.fields.clear();
        w.secret = None;
        assert!(w.account_json(&m, &a).is_empty());
    }
}

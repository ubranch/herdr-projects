//! Root, config directory, herdr binary and socket resolution.
//!
//! Nothing here reads the process environment directly: callers pass an `Env`,
//! so resolution order is testable and never depends on plugin variables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::herdr;
use crate::runner::Runner;

#[derive(Debug, Clone)]
pub struct Env {
    vars: BTreeMap<String, String>,
    pub home: PathBuf,
}

impl Env {
    pub fn from_process() -> Result<Self> {
        let vars: BTreeMap<String, String> = std::env::vars().collect();
        Self::from_vars(vars)
    }

    fn from_vars(vars: BTreeMap<String, String>) -> Result<Self> {
        #[cfg(windows)]
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.to_ascii_uppercase(), value))
            .collect::<BTreeMap<_, _>>();
        let home = ["HOME", "USERPROFILE"]
            .into_iter()
            .filter_map(|key| vars.get(key).filter(|value| !value.is_empty()))
            .map(PathBuf::from)
            .find(|path| !cfg!(windows) || path.is_absolute());
        #[cfg(windows)]
        let home = home.or_else(|| {
            let drive = vars.get("HOMEDRIVE")?;
            let path = vars.get("HOMEPATH")?;
            let home = PathBuf::from(format!("{drive}{path}"));
            home.is_absolute().then_some(home)
        });
        let home = home.context("HOME or USERPROFILE is not set to a home directory")?;
        Ok(Env { vars, home })
    }

    #[cfg(test)]
    pub fn for_test(home: &Path, vars: &[(&str, &str)]) -> Self {
        Env {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            home: home.to_path_buf(),
        }
    }

    /// A variable's value; an empty value counts as unset.
    pub fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The fixed user-level config directory, `~/.config/herdr-projects`.
    pub fn config_dir(&self) -> PathBuf {
        self.home.join(".config").join("herdr-projects")
    }

    /// Herdr's runtime directory for sockets/plugins; a TOML override does not move it.
    pub fn herdr_config_dir(&self) -> PathBuf {
        let config = self.var("XDG_CONFIG_HOME").map(PathBuf::from);
        #[cfg(windows)]
        let config = config.or_else(|| self.var("APPDATA").map(PathBuf::from));
        let config = config.unwrap_or_else(|| self.home.join(".config"));
        config.join("herdr")
    }

    /// `HERDR_BIN_PATH` when set, else `herdr` on `PATH`.
    pub fn herdr_bin(&self) -> String {
        self.var("HERDR_BIN_PATH").unwrap_or("herdr").to_string()
    }

    fn expand_tilde(&self, path: &str) -> PathBuf {
        match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            Some(rest) => self.home.join(rest),
            None if path == "~" => self.home.clone(),
            None => PathBuf::from(path),
        }
    }
}

/// This binary's own path with symbolic links resolved, so a path written into
/// hooks, AGENTS.md or the tab bar survives `~/.local/bin` links changing.
pub fn binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not find this binary's own path")?;
    let exe = canonicalize(&exe).unwrap_or(exe);
    #[cfg(windows)]
    let exe = crate::command_link::installed_source(&exe).unwrap_or(exe);
    Ok(exe)
}

/// Canonical paths suitable for Herdr and shell arguments, without changing
/// Windows names that require verbatim path semantics.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let normal_names = path.components().all(|component| match component {
            Component::Normal(name) => name.to_str().is_some_and(|name| {
                if name.ends_with(['.', ' '])
                    || name.contains(['<', '>', ':', '"', '|', '?', '*'])
                    || name.chars().any(|c| c < ' ')
                {
                    return false;
                }
                let stem = name.split('.').next().unwrap_or("");
                let reserved = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
                    .iter()
                    .any(|word| stem.eq_ignore_ascii_case(word));
                let numbered = stem.get(..3).is_some_and(|prefix| {
                    prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
                }) && stem.get(3..).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
                !reserved && !numbered
            }),
            _ => true,
        });
        if normal_names
            && let Some(text) = path.to_str()
            && let Some(Component::Prefix(prefix)) = path.components().next()
        {
            match prefix.kind() {
                Prefix::VerbatimDisk(_) => return Ok(PathBuf::from(&text[4..])),
                Prefix::VerbatimUNC(_, _) => {
                    return Ok(PathBuf::from(format!("\\\\{}", &text[8..])));
                }
                _ => {}
            }
        }
    }
    Ok(path)
}

/// Native directory identity: cheap lexical spelling first, then physical
/// paths for aliases. Failed resolution never makes two different paths equal.
pub fn same_dir(left: &Path, right: &Path) -> bool {
    if left.as_os_str().is_empty() || right.as_os_str().is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    let Ok(left) = canonicalize(left) else {
        return false;
    };
    let Ok(right) = canonicalize(right) else {
        return false;
    };
    left == right
}

/// Native descendant ownership, resolving both spellings when they exist.
/// Missing paths keep lexical component checks for already-removed worktrees.
pub fn within_dir(path: &Path, dir: &Path) -> bool {
    if path.as_os_str().is_empty() || dir.as_os_str().is_empty() {
        return false;
    }
    if path == dir {
        return true;
    }
    let canonical_path = canonicalize(path).ok();
    let canonical_dir = canonicalize(dir).ok();
    canonical_path
        .as_deref()
        .unwrap_or(path)
        .starts_with(canonical_dir.as_deref().unwrap_or(dir))
}

#[cfg(all(test, windows))]
pub fn windows_cmd() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("Windows test process has SystemRoot");
    PathBuf::from(root).join("System32").join("cmd.exe")
}

/// What every subcommand works from: the environment, the resolved root and
/// config directory, and the runner all external commands go through.
pub struct Ctx<'a> {
    pub env: &'a Env,
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub runner: &'a dyn Runner,
    /// False in tests, so commands that ensure a ticker never spawn a process.
    pub detached_ticker: bool,
}

/// The part of `config.toml` that resolution needs. Safety tables are read by
/// the `project` module from the same file.
#[derive(Debug, Default, Deserialize)]
struct RootConfig {
    root: Option<String>,
}

/// Projects root: `--root`, then `HERDR_PROJECTS_ROOT`, then `root` in
/// `<config_dir>/config.toml`, then `~/.herdr-projects`.
pub fn resolve_root(flag: Option<&Path>, env: &Env, config_dir: &Path) -> Result<PathBuf> {
    if let Some(flag) = flag {
        return absolute(flag);
    }
    if let Some(var) = env.var("HERDR_PROJECTS_ROOT") {
        return absolute(&env.expand_tilde(var));
    }
    let config_file = config_dir.join("config.toml");
    if let Ok(text) = std::fs::read_to_string(&config_file) {
        let config: RootConfig = toml::from_str(&text)
            .with_context(|| format!("{} does not parse", config_file.display()))?;
        if let Some(root) = config.root.filter(|r| !r.is_empty()) {
            return absolute(&env.expand_tilde(&root));
        }
    }
    Ok(env.home.join(".herdr-projects"))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("bad path {}", path.display()))
}

/// Which herdr session a command should talk to, as given on the command line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFlags {
    pub session: Option<String>,
    pub socket: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub socket: PathBuf,
    /// Known only when the session was chosen by name.
    pub name: Option<String>,
}

/// `--session`, then `--socket`, then `HERDR_SOCKET_PATH`, then `HERDR_SESSION`,
/// then herdr's default socket. A name is turned into a socket path by asking
/// herdr (`session list --json`), never by guessing herdr's directory layout.
pub fn resolve_session(flags: &SessionFlags, env: &Env, runner: &dyn Runner) -> Result<Session> {
    if flags.session.is_some() && flags.socket.is_some() {
        bail!("pass --session or --socket, not both");
    }
    if let Some(name) = &flags.session {
        return session_by_name(name, env, runner);
    }
    if let Some(socket) = &flags.socket {
        return Ok(Session {
            socket: absolute(socket)?,
            name: None,
        });
    }
    if let Some(socket) = env.var("HERDR_SOCKET_PATH") {
        return Ok(Session {
            socket: PathBuf::from(socket),
            name: None,
        });
    }
    if let Some(name) = env.var("HERDR_SESSION") {
        return session_by_name(name, env, runner);
    }
    let mut sessions = herdr::session_list(&env.herdr_bin(), runner).unwrap_or_default();
    if let Some(index) = sessions.iter().position(|s| s.default) {
        let found = sessions.swap_remove(index);
        return Ok(Session {
            socket: found.socket_path,
            name: None,
        });
    }
    if sessions.len() == 1 {
        let found = sessions.into_iter().next().unwrap();
        return Ok(Session {
            socket: found.socket_path,
            name: Some(found.name),
        });
    }
    if !sessions.is_empty() {
        bail!("herdr has multiple sessions but no default; pass --session or --socket");
    }
    Ok(Session {
        socket: env.herdr_config_dir().join("herdr.sock"),
        name: None,
    })
}

fn session_by_name(name: &str, env: &Env, runner: &dyn Runner) -> Result<Session> {
    let sessions = herdr::session_list(&env.herdr_bin(), runner)?;
    match sessions.into_iter().find(|s| s.name == name) {
        Some(found) => Ok(Session {
            socket: found.socket_path,
            name: Some(name.to_string()),
        }),
        None => {
            bail!("herdr has no session named `{name}`; start it with `herdr --session {name}`")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::{FakeRunner, ok};

    const SESSIONS: &str = r#"{"sessions":[
        {"default":true,"name":"default","running":true,"session_dir":"/h/.config/herdr","socket_path":"/h/.config/herdr/herdr.sock"},
        {"default":false,"name":"hp-dev","running":true,"session_dir":"/h/.config/herdr/sessions/hp-dev","socket_path":"/h/.config/herdr/sessions/hp-dev/herdr.sock"}]}"#;

    #[test]
    fn root_order_flag_env_config_default() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.toml"), "root = \"~/from-config\"\n").unwrap();

        let from_env = home.path().join("from-env");
        let env = Env::for_test(
            home.path(),
            &[("HERDR_PROJECTS_ROOT", from_env.to_str().unwrap())],
        );
        let flag = home.path().join("from-flag");
        assert_eq!(resolve_root(Some(&flag), &env, &config_dir).unwrap(), flag);
        assert_eq!(resolve_root(None, &env, &config_dir).unwrap(), from_env);

        let env = Env::for_test(home.path(), &[]);
        assert_eq!(
            resolve_root(None, &env, &config_dir).unwrap(),
            home.path().join("from-config")
        );
        assert_eq!(
            resolve_root(None, &env, &home.path().join("missing")).unwrap(),
            home.path().join(".herdr-projects")
        );
    }

    #[test]
    fn root_config_that_does_not_parse_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "root = [").unwrap();
        let env = Env::for_test(home.path(), &[]);
        assert!(resolve_root(None, &env, home.path()).is_err());
    }

    #[test]
    fn empty_variable_counts_as_unset() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(
            home.path(),
            &[("HERDR_PROJECTS_ROOT", ""), ("HERDR_BIN_PATH", "")],
        );
        assert_eq!(
            resolve_root(None, &env, &home.path().join("none")).unwrap(),
            home.path().join(".herdr-projects")
        );
        assert_eq!(env.herdr_bin(), "herdr");
    }

    #[test]
    fn herdr_bin_prefers_the_variable() {
        let env = Env::for_test(Path::new("/h"), &[("HERDR_BIN_PATH", "/opt/herdr")]);
        assert_eq!(env.herdr_bin(), "/opt/herdr");
    }

    #[test]
    fn session_order_flag_socket_env_default() {
        let runner = FakeRunner::new();
        runner.on("session list --json", ok(SESSIONS));
        let both = [
            ("HERDR_SOCKET_PATH", "/env.sock"),
            ("HERDR_SESSION", "hp-dev"),
        ];
        let env = Env::for_test(Path::new("/h"), &both);

        let by_name = SessionFlags {
            session: Some("hp-dev".into()),
            socket: None,
        };
        let got = resolve_session(&by_name, &env, &runner).unwrap();
        assert_eq!(
            got.socket,
            PathBuf::from("/h/.config/herdr/sessions/hp-dev/herdr.sock")
        );
        assert_eq!(got.name.as_deref(), Some("hp-dev"));

        let by_socket = SessionFlags {
            session: None,
            socket: Some("/flag.sock".into()),
        };
        let got = resolve_session(&by_socket, &env, &runner).unwrap();
        assert_eq!(
            got,
            Session {
                socket: std::path::absolute(Path::new("/flag.sock")).unwrap(),
                name: None
            }
        );

        let none = SessionFlags::default();
        let got = resolve_session(&none, &env, &runner).unwrap();
        assert_eq!(
            got,
            Session {
                socket: "/env.sock".into(),
                name: None
            }
        );

        let env = Env::for_test(Path::new("/h"), &[("HERDR_SESSION", "hp-dev")]);
        let got = resolve_session(&none, &env, &runner).unwrap();
        assert_eq!(got.name.as_deref(), Some("hp-dev"));

        let env = Env::for_test(Path::new("/h"), &[]);
        let got = resolve_session(&none, &env, &runner).unwrap();
        assert_eq!(
            got,
            Session {
                socket: "/h/.config/herdr/herdr.sock".into(),
                name: None
            }
        );
    }

    #[test]
    fn unknown_session_name_is_refused() {
        let runner = FakeRunner::new();
        runner.on("session list --json", ok(SESSIONS));
        let env = Env::for_test(Path::new("/h"), &[]);
        let flags = SessionFlags {
            session: Some("nope".into()),
            socket: None,
        };
        assert!(resolve_session(&flags, &env, &runner).is_err());
    }

    #[test]
    fn session_and_socket_together_are_refused() {
        let runner = FakeRunner::new();
        let env = Env::for_test(Path::new("/h"), &[]);
        let flags = SessionFlags {
            session: Some("a".into()),
            socket: Some("/b".into()),
        };
        assert!(resolve_session(&flags, &env, &runner).is_err());
    }

    #[test]
    fn home_falls_back_to_userprofile_and_runtime_config_follows_host() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join("项目 home");
        let vars = [
            ("HOME".to_string(), String::new()),
            (
                "USERPROFILE".to_string(),
                profile.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let env = Env::from_vars(vars).unwrap();
        assert_eq!(env.home, profile);
        assert_eq!(env.config_dir(), profile.join(".config/herdr-projects"));
        let xdg = home.path().join("xdg config");
        let appdata = home.path().join("roaming");
        let config = home.path().join("isolated/herdr.toml");
        let env = Env::for_test(
            &profile,
            &[
                ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
                ("APPDATA", appdata.to_str().unwrap()),
            ],
        );
        assert_eq!(env.herdr_config_dir(), xdg.join("herdr"));
        let env = Env::for_test(
            &profile,
            &[
                ("HERDR_CONFIG_PATH", config.to_str().unwrap()),
                ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ],
        );
        assert_eq!(env.herdr_config_dir(), xdg.join("herdr"));
        assert_eq!(crate::setup::herdr_config_path(&env), config);
        let runner = FakeRunner::new();
        runner.on("session list --json", ok(r#"{"sessions":[]}"#));
        assert_eq!(
            resolve_session(&SessionFlags::default(), &env, &runner)
                .unwrap()
                .socket,
            xdg.join("herdr/herdr.sock")
        );
        let socket = home.path().join("injected.sock");
        let env = Env::for_test(
            &profile,
            &[
                ("HERDR_CONFIG_PATH", config.to_str().unwrap()),
                ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
                ("HERDR_SOCKET_PATH", socket.to_str().unwrap()),
            ],
        );
        assert_eq!(
            resolve_session(&SessionFlags::default(), &env, &runner)
                .unwrap()
                .socket,
            socket
        );
        #[cfg(windows)]
        {
            let env = Env::for_test(&profile, &[("APPDATA", appdata.to_str().unwrap())]);
            assert_eq!(env.herdr_config_dir(), appdata.join("herdr"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_uses_absolute_profile_or_home_drive() {
        let home = tempfile::tempdir().unwrap();
        let vars = [
            ("HOME".into(), "relative-home".into()),
            (
                "UserProfile".into(),
                home.path().to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(Env::from_vars(vars).unwrap().home, home.path());
        let vars = [
            ("HOMEDRIVE".into(), "C:".into()),
            ("HOMEPATH".into(), r"\Users\项目 person".into()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            Env::from_vars(vars).unwrap().home,
            Path::new(r"C:\Users\项目 person")
        );
    }

    #[test]
    fn canonical_paths_preserve_spaces_and_unicode() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("项目 with spaces");
        std::fs::create_dir(&dir).unwrap();
        let canonical = canonicalize(&dir).unwrap();
        assert!(canonical.is_dir());
        assert_eq!(canonical.file_name(), dir.file_name());
        #[cfg(windows)]
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn native_directory_identity_resolves_aliases_but_not_unknown_paths() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("项目 directory");
        let other = home.path().join("different directory");
        std::fs::create_dir_all(dir.join("child")).unwrap();
        std::fs::create_dir(&other).unwrap();
        let trailing = PathBuf::from(format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR));
        assert!(same_dir(&dir, &trailing));
        assert!(same_dir(&dir, &dir.join("child").join("..")));
        assert!(!same_dir(&dir, &other));
        assert!(!same_dir(
            &home.path().join("missing"),
            &home.path().join("other missing")
        ));
        assert!(!same_dir(
            &home.path().join("MissingCase"),
            &home.path().join("missingcase")
        ));
        assert!(!same_dir(Path::new(""), Path::new("")));
        #[cfg(windows)]
        {
            assert!(same_dir(
                &dir,
                &PathBuf::from(dir.to_string_lossy().replace('\\', "/"))
            ));
            assert!(same_dir(&dir, &std::fs::canonicalize(&dir).unwrap()));
        }
    }

    #[test]
    fn native_descendant_ownership_normalizes_both_operands() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("项目 worktree");
        let child = dir.join("child");
        let other = home.path().join("different directory");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(&other).unwrap();
        let alias = child.join("..");
        assert!(within_dir(&child, &alias));
        assert!(within_dir(&alias, &dir));
        assert!(!within_dir(&other, &dir));
        assert!(!within_dir(
            &child.join("../..").join("different directory"),
            &dir
        ));
        assert!(!within_dir(
            &home.path().join("项目 worktree sibling"),
            &dir
        ));
        assert!(!within_dir(Path::new(""), &dir));
        assert!(!within_dir(&child, Path::new("")));
        #[cfg(windows)]
        {
            let verbatim = std::fs::canonicalize(&dir).unwrap();
            assert!(within_dir(&child, &verbatim));
            assert!(within_dir(&std::fs::canonicalize(&child).unwrap(), &dir));
        }
    }

    #[cfg(windows)]
    #[test]
    fn names_requiring_verbatim_semantics_stay_verbatim() {
        let home = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(home.path())
            .unwrap()
            .join("trailing.");
        std::fs::create_dir(&dir).unwrap();
        let canonical = canonicalize(&dir).unwrap();
        assert!(canonical.is_dir());
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
        std::fs::remove_dir(&canonical).unwrap();
    }

    #[test]
    fn default_session_uses_discovery_before_platform_fallback() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let runner = FakeRunner::new();
        let socket = home.path().join("项目 session/socket.sock");
        let reply = serde_json::json!({"sessions": [{"name": "only", "running": true, "socket_path": socket}]});
        runner.on("session list --json", ok(&reply.to_string()));
        assert_eq!(
            resolve_session(&SessionFlags::default(), &env, &runner).unwrap(),
            Session {
                socket,
                name: Some("only".into())
            }
        );
        let runner = FakeRunner::new();
        runner.on("session list --json", ok(r#"{"sessions":[]}"#));
        assert_eq!(
            resolve_session(&SessionFlags::default(), &env, &runner)
                .unwrap()
                .socket,
            env.herdr_config_dir().join("herdr.sock")
        );
        let runner = FakeRunner::new();
        runner.on(
            "session list --json",
            ok(&SESSIONS.replace("\"default\":true", "\"default\":false")),
        );
        assert!(resolve_session(&SessionFlags::default(), &env, &runner).is_err());
    }
}

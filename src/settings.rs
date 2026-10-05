//! `set`, `routine toggle` and `open-file`: the small edits the popup's keys
//! make, each a CLI command. Front matter is edited with toml_edit, so the
//! rest of the file keeps its formatting.

use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::paths::Ctx;
use crate::project::{self, Project, Settings};

/// The keys `set` accepts, with what they hold.
pub const KEYS: [(&str, &str); 10] = [
    ("name", "text"),
    ("goal", "text"),
    ("coordinator_profile", "profile name"),
    ("thread_profile", "profile name"),
    ("max_parallel_threads", "number"),
    ("auto_resolve_days", "number (0 = never)"),
    ("nudge", "true/false"),
    ("mute", "true/false"),
    ("repos.add", "PATH or PATH@MACHINE"),
    ("repos.remove", "PATH"),
];

/// Splits `+++` front matter from the rest, keeping both verbatim.
fn split(text: &str) -> Result<(&str, &str)> {
    let rest = text
        .strip_prefix("+++\n")
        .context("the file must start with a `+++` line")?;
    match rest.find("\n+++\n") {
        Some(end) => Ok((&rest[..end + 1], &rest[end + 5..])),
        None => rest
            .strip_suffix("\n+++")
            .map(|f| (f, ""))
            .context("no closing `+++` line"),
    }
}

fn join(front: &str, body: &str) -> String {
    let front = front.trim_end_matches('\n');
    if body.is_empty() {
        format!("+++\n{front}\n+++\n")
    } else {
        format!("+++\n{front}\n+++\n{body}")
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" | "on" | "yes" => Ok(true),
        "false" | "off" | "no" => Ok(false),
        other => bail!("`{other}` is not true or false"),
    }
}

/// `new` writes `repos = []` (or inline tables): turn it into `[[repos]]`
/// tables so entries can be added and removed.
fn normalize_repos(doc: &mut DocumentMut) -> Result<()> {
    let tables = match doc.get("repos") {
        None => Some(ArrayOfTables::new()),
        Some(Item::ArrayOfTables(_)) => None,
        Some(Item::Value(toml_edit::Value::Array(array))) => {
            let mut tables = ArrayOfTables::new();
            for value in array.iter() {
                let inline = value
                    .as_inline_table()
                    .context("`repos` entries must be tables")?;
                tables.push(inline.clone().into_table());
            }
            Some(tables)
        }
        Some(_) => bail!("`repos` must be a list of tables"),
    };
    if let Some(tables) = tables {
        // Array-of-tables must come after the plain keys: re-insert at the end.
        doc.remove("repos");
        doc["repos"] = Item::ArrayOfTables(tables);
    }
    Ok(())
}

/// Applies one setting to PROJECT.md's text, validating the result.
pub fn set_in(text: &str, key: &str, value: &str) -> Result<String> {
    let (front, body) = split(text)?;
    let mut doc = front
        .parse::<DocumentMut>()
        .context("PROJECT.md front matter does not parse")?;
    let value = value.trim();
    match key {
        "name" | "goal" => {
            if key == "name" && value.is_empty() {
                bail!("the name may not be empty");
            }
            doc[key] = toml_edit::value(value);
        }
        "coordinator_profile" | "thread_profile" | "coordinator_agent" | "thread_agent" => {
            crate::profiles::validate_name(value)?;
            // The old key names the same setting: keep one of them.
            let role = key.split('_').next().unwrap_or_default();
            doc.remove(&format!("{role}_agent"));
            doc[&format!("{role}_profile")] = toml_edit::value(value);
        }
        "max_parallel_threads" | "auto_resolve_days" => {
            let n: i64 = value
                .parse()
                .ok()
                .filter(|n: &i64| *n >= 0 && *n <= 1000)
                .with_context(|| format!("`{value}` is not a number from 0 to 1000"))?;
            if key == "max_parallel_threads" && n == 0 {
                bail!("max_parallel_threads must be at least 1");
            }
            doc[key] = toml_edit::value(n);
        }
        "nudge" | "mute" => doc[key] = toml_edit::value(parse_bool(value)?),
        "repos.add" => {
            let repo = project::parse_repo_arg(value);
            if repo.path.is_empty() {
                bail!("give a repository path");
            }
            let path = match repo.machine {
                Some(_) => repo.path.clone(),
                None => crate::paths::canonicalize(Path::new(&repo.path))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(repo.path.clone()),
            };
            normalize_repos(&mut doc)?;
            let repos = doc["repos"]
                .as_array_of_tables_mut()
                .context("`repos` must be [[repos]] tables")?;
            if repos.iter().any(|t| {
                t.get("path").and_then(Item::as_str) == Some(path.as_str())
                    && t.get("machine").and_then(Item::as_str) == repo.machine.as_deref()
            }) {
                bail!("{value} is already listed");
            }
            let mut table = Table::new();
            table["path"] = toml_edit::value(path);
            if let Some(machine) = repo.machine {
                table["machine"] = toml_edit::value(machine);
            }
            repos.push(table);
        }
        "repos.remove" => {
            normalize_repos(&mut doc)?;
            let repos = doc
                .get_mut("repos")
                .and_then(Item::as_array_of_tables_mut)
                .context("no repos are listed")?;
            let before = repos.len();
            let wanted = project::parse_repo_arg(value);
            repos.retain(|t| {
                let path = t.get("path").and_then(Item::as_str).unwrap_or("");
                !(path == value
                    || (path == wanted.path
                        && t.get("machine").and_then(Item::as_str) == wanted.machine.as_deref()))
            });
            if repos.len() == before {
                bail!("{value} is not listed");
            }
        }
        other => bail!(
            "unknown setting `{other}`; one of: {}",
            KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
        ),
    }
    let front = doc.to_string();
    // The result must still be the settings the binary reads.
    toml::from_str::<Settings>(&front).context("the edited settings do not parse")?;
    Ok(join(&front, body))
}

/// `set <slug> <key> <value>`.
pub fn set(ctx: &Ctx, slug: &str, key: &str, value: &str) -> Result<()> {
    let project = Project::load(&ctx.root, slug)?;
    let _lock = project.lock()?;
    if let Some(role) = key.strip_suffix("_profile").or(key.strip_suffix("_agent")) {
        // A default must be a profile this project may use.
        let role = crate::profiles::Role::parse(role)?;
        let config = crate::profiles::load(&ctx.config_dir)?;
        config.get(value.trim()).with_context(|| {
            format!(
                "there is no profile `{}`; `profile list` shows them",
                value.trim()
            )
        })?;
        crate::profiles::check_allowed(
            &config,
            &project.safety(&ctx.config_dir)?,
            role,
            value.trim(),
            slug,
        )?;
    }
    let text = std::fs::read_to_string(project.project_md())?;
    let edited = set_in(&text, key, value)?;
    project::write_atomic(&project.project_md(), edited.as_bytes())?;
    if key == "name" {
        // The priming file names the project.
        project::write_priming(&project, &crate::coordinator::current_prefix(&ctx.root)?)?;
    }
    println!("{slug}: {key} = {value}");
    Ok(())
}

/// `routine toggle <slug> <name> [--on|--off]`: flips `enabled`.
pub fn routine_toggle(ctx: &Ctx, slug: &str, name: &str, to: Option<bool>) -> Result<()> {
    let project = Project::load(&ctx.root, slug)?;
    project::validate_slug(name)?;
    let _lock = project.lock()?;
    let path = project.dir().join("routines").join(format!("{name}.md"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no routine `{name}` in `{slug}`"))?;
    let (front, body) = split(&text)?;
    let mut doc = front
        .parse::<DocumentMut>()
        .context("the routine's front matter does not parse")?;
    let current = doc.get("enabled").and_then(Item::as_bool).unwrap_or(true);
    let enabled = to.unwrap_or(!current);
    doc["enabled"] = toml_edit::value(enabled);
    let edited = join(&doc.to_string(), body);
    crate::routine::parse(name, &edited)?;
    project::write_atomic(&path, edited.as_bytes())?;
    println!(
        "routine `{name}` is now {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

/// Whether a file looks like text: no NUL byte in its first 8 KB.
pub fn is_text(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0; 8192];
    let n = file.read(&mut buf).unwrap_or(0);
    !buf[..n].contains(&0)
}

/// `VISUAL` / `EDITOR` are trusted shell fragments, including any arguments.
/// An existing executable path is invoked literally, even with spaces or quotes.
fn editor_command(editor: Option<&str>, path: &str) -> String {
    match editor {
        Some(editor) if Path::new(editor).is_file() => {
            crate::remote::local_command(editor, &[path])
        }
        Some(editor) => format!("{editor} {}", crate::remote::quote_local(path)),
        None => {
            crate::remote::local_command(if cfg!(windows) { "notepad.exe" } else { "vi" }, &[path])
        }
    }
}

/// `open-file <path> [--workspace W]`: a text file opens in a new Herdr tab
/// running `VISUAL` / `EDITOR` (default: `notepad.exe` on Windows, `vi` on Unix);
/// anything else uses the system opener. No viewer.
pub fn open_file(ctx: &Ctx, path: &Path, workspace: Option<&str>) -> Result<()> {
    let path = crate::paths::canonicalize(path)
        .with_context(|| format!("{} does not exist", path.display()))?;
    if path.is_dir() || !is_text(&path) {
        return system_open(ctx, &path.to_string_lossy());
    }
    let socket = ctx
        .env
        .var("HERDR_SOCKET_PATH")
        .context("not inside Herdr: HERDR_SOCKET_PATH is not set")?;
    let herdr = crate::herdr::Herdr::new(ctx.env.herdr_bin(), socket, ctx.runner);
    let dir = path.parent().unwrap_or(Path::new("/"));
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let workspace = workspace
        .map(str::to_string)
        .or_else(|| ctx.env.var("HERDR_WORKSPACE_ID").map(str::to_string))
        .unwrap_or_default();
    let dir_text = dir.to_string_lossy();
    let mut args = vec![
        "tab", "create", "--cwd", &dir_text, "--label", &label, "--focus",
    ];
    if !workspace.is_empty() {
        args.extend(["--workspace", workspace.as_str()]);
    }
    let created = herdr
        .call(&args, crate::herdr::CALL_TIMEOUT)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let pane = created["root_pane"]["pane_id"]
        .as_str()
        .context("herdr's tab reply has no pane")?
        .to_string();
    let editor = ctx.env.var("VISUAL").or(ctx.env.var("EDITOR"));
    let command = editor_command(editor, &path.to_string_lossy());
    // A fresh pane's shell needs a moment before it takes input.
    std::thread::sleep(std::time::Duration::from_millis(300));
    herdr
        .call(
            &["pane", "run", &pane, &command],
            crate::herdr::CALL_TIMEOUT,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("opened {} in a new tab", path.display());
    Ok(())
}

/// Opens a URL or non-text file with PowerShell `Start-Process` on Windows,
/// `open` on macOS, or `xdg-open` on other Unix systems.
pub fn system_open(ctx: &Ctx, target: &str) -> Result<()> {
    let opener = if cfg!(windows) {
        "pwsh.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let command = crate::runner::Cmd::new(opener, std::time::Duration::from_secs(10));
    let command = if cfg!(windows) {
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Start-Process -FilePath {}",
                crate::remote::quote_local(target)
            ),
        ])
    } else {
        command.arg(target)
    };
    let out = ctx.runner.run(&command)?;
    if !out.success() {
        bail!("{opener} {target}: {}", out.error_text());
    }
    println!("opened {target}");
    Ok(())
}

/// `set`'s array form for tests and the popup: the current repos as text.
pub fn repos_text(settings: &Settings) -> String {
    if settings.repos.is_empty() {
        return "(none)".into();
    }
    settings
        .repos
        .iter()
        .map(|r| match &r.machine {
            Some(m) => format!("{}@{m}", r.path),
            None => r.path.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_paths_and_trusted_fragments_pass_hostile_filenames_literally() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("editor's O’Connor folder");
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join(if cfg!(windows) {
            "editor's O’Connor tool.ps1"
        } else {
            "editor's O’Connor tool.sh"
        });
        let received = root.path().join("received.txt");
        let received_text = received.to_string_lossy();
        let body = if cfg!(windows) {
            format!(
                "[IO.File]::WriteAllLines({}, [string[]]$args)",
                crate::remote::quote_local(&received_text)
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                crate::remote::quote(&received_text)
            )
        };
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let target = root
            .path()
            .join("notes '‘’‚‛ ; mkdir injected ; $(mkdir injected) #.txt");
        std::fs::write(&target, "text").unwrap();
        let script_text = script.to_string_lossy();
        let target_text = target.to_string_lossy();
        let fragment = crate::remote::local_command(&script_text, &["trusted prefix"]);
        for (editor, expected) in [
            (script_text.as_ref(), vec![target_text.as_ref()]),
            (
                fragment.as_str(),
                vec!["trusted prefix", target_text.as_ref()],
            ),
        ] {
            let command = editor_command(Some(editor), &target_text);
            let cmd = if cfg!(windows) {
                crate::runner::Cmd::new("pwsh.exe", std::time::Duration::from_secs(10)).args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &command,
                ])
            } else {
                crate::runner::Cmd::new("sh", std::time::Duration::from_secs(10))
                    .args(["-c", &command])
            }
            .cwd(root.path());
            let out = crate::runner::Runner::run(&crate::runner::RealRunner, &cmd).unwrap();
            assert!(out.success(), "{}", out.error_text());
            let actual = std::fs::read_to_string(&received).unwrap();
            assert_eq!(actual.lines().collect::<Vec<_>>(), expected);
            assert!(!root.path().join("injected").exists());
        }
    }

    #[cfg(windows)]
    #[test]
    fn system_opener_preserves_smart_quotes_without_launching_gui_or_path_statements() {
        use crate::runner::{RealRunner, Runner};
        let root = tempfile::tempdir().unwrap();
        let received = root.path().join("received.txt");
        let capture = received.to_string_lossy().into_owned();
        let cwd = root.path().to_path_buf();
        let env = crate::paths::Env::for_test(root.path(), &[]);
        let runner = crate::runner::fake::FakeRunner::new();
        runner.on_fn(
            |cmd| cmd.program == "pwsh.exe",
            move |cmd| {
                // Replace only the GUI boundary; the generated command is parsed
                // and its FilePath parameter is bound by real PowerShell.
                let mut cmd = cmd.clone().env("HP_OPEN_TARGET", &capture).cwd(&cwd);
                let script = cmd.args.last_mut().unwrap();
                *script = format!(
                    "function Start-Process {{ param([string]$FilePath) [IO.File]::WriteAllText($env:HP_OPEN_TARGET, $FilePath, [Text.UTF8Encoding]::new($false)) }}\n{script}"
                );
                RealRunner.run(&cmd)
            },
        );
        let ctx = Ctx {
            env: &env,
            root: root.path().to_path_buf(),
            config_dir: root.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        system_open(&ctx, "O’Connor").unwrap();
        assert_eq!(std::fs::read(&received).unwrap(), "O’Connor".as_bytes());
        for delimiter in ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
            let target =
                format!("notes{delimiter}; Set-Content -LiteralPath injected -Value yes; #.txt");
            system_open(&ctx, &target).unwrap();
            assert_eq!(std::fs::read(&received).unwrap(), target.as_bytes());
            assert!(!root.path().join("injected").exists(), "path data executed");
        }
    }

    #[test]
    fn renaming_reports_priming_conflicts_without_overwriting_user_files() {
        let root = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let claude = project.dir().join("CLAUDE.md");
        let backup = project.dir().join("CLAUDE.md.before-herdr-projects");
        std::fs::write(&claude, "newer user instructions").unwrap();
        std::fs::write(&backup, "older user instructions").unwrap();
        let env = crate::paths::Env::for_test(root.path(), &[]);
        let runner = crate::runner::fake::FakeRunner::new();
        let ctx = Ctx {
            env: &env,
            root: root.path().to_path_buf(),
            config_dir: root.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        let error = set(&ctx, "demo", "name", "Renamed")
            .unwrap_err()
            .to_string();
        assert!(error.contains(&claude.display().to_string()), "{error}");
        assert!(error.contains(&backup.display().to_string()), "{error}");
        assert!(error.contains("both files were preserved"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&claude).unwrap(),
            "newer user instructions"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "older user instructions"
        );
        let (settings, _) =
            project::parse_project_md(&std::fs::read_to_string(project.project_md()).unwrap())
                .unwrap();
        assert_eq!(settings.name, "Renamed");
    }

    const MD: &str = "+++\nname = \"Demo\" # the label\ngoal = \"\"\nmax_parallel_threads = 3\nnudge = false\n\n[[repos]]\npath = \"/srv/app\"\nmachine = \"box\"\n+++\n\n# Instructions\nBody with +++ inside\n";

    #[test]
    fn settings_are_edited_in_place_and_validated() {
        let edited = set_in(MD, "max_parallel_threads", "5").unwrap();
        assert!(edited.contains("max_parallel_threads = 5"));
        assert!(edited.contains("# the label"), "comments survive");
        assert!(edited.ends_with("\n# Instructions\nBody with +++ inside\n"));
        let (settings, body) = project::parse_project_md(&edited).unwrap();
        assert_eq!(settings.max_parallel_threads, 5);
        assert!(body.starts_with("# Instructions"));

        assert!(set_in(MD, "max_parallel_threads", "0").is_err());
        assert!(set_in(MD, "max_parallel_threads", "many").is_err());
        assert!(set_in(MD, "thread_profile", "not a name").is_err());
        // The old key is replaced by the new one, never duplicated.
        let old = set_in(
            "+++\nthread_agent = \"claude\"\n+++\n",
            "thread_agent",
            "luna",
        )
        .unwrap();
        assert_eq!(old, "+++\nthread_profile = \"luna\"\n+++\n");
        assert_eq!(
            project::parse_project_md(&set_in(&old, "coordinator_profile", "sol").unwrap())
                .unwrap()
                .0
                .coordinator_profile,
            "sol"
        );
        assert!(set_in(MD, "whatever", "x").is_err());
        let muted = set_in(MD, "mute", "on").unwrap();
        assert!(project::parse_project_md(&muted).unwrap().0.mute);
        let goal = set_in(MD, "goal", "Ship \"it\"").unwrap();
        assert_eq!(
            project::parse_project_md(&goal).unwrap().0.goal,
            "Ship \"it\""
        );
    }

    #[test]
    fn repos_are_added_and_removed() {
        let added = set_in(MD, "repos.add", "/srv/lib@box").unwrap();
        let (settings, _) = project::parse_project_md(&added).unwrap();
        assert_eq!(settings.repos.len(), 2);
        assert!(set_in(&added, "repos.add", "/srv/lib@box").is_err());
        let removed = set_in(&added, "repos.remove", "/srv/app@box").unwrap();
        let (settings, _) = project::parse_project_md(&removed).unwrap();
        assert_eq!(settings.repos.len(), 1);
        assert_eq!(settings.repos[0].path, "/srv/lib");
        assert!(set_in(&removed, "repos.remove", "/nope").is_err());
        // What `new` writes: an empty inline array, then more keys.
        let fresh = "+++\nname = \"X\"\nrepos = []\nnudge = false\n+++\nBody\n";
        let added = set_in(fresh, "repos.add", "/x@m").unwrap();
        let (settings, body) = project::parse_project_md(&added).unwrap();
        assert_eq!(
            (settings.repos.len(), settings.nudge, body.as_str()),
            (1, false, "Body\n")
        );
        // A file with no repos yet.
        let bare = "+++\nname = \"X\"\n+++\n";
        let added = set_in(bare, "repos.add", "/x@m").unwrap();
        assert_eq!(project::parse_project_md(&added).unwrap().0.repos.len(), 1);
    }

    #[test]
    fn routines_toggle_and_keep_their_prompt() {
        let root = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let path = project.dir().join("routines/nightly.md");
        std::fs::write(&path, "+++\nschedule = \"daily 02:00\"\n+++\nCheck it.\n").unwrap();
        let env = crate::paths::Env::for_test(root.path(), &[]);
        let runner = crate::runner::fake::FakeRunner::new();
        let ctx = Ctx {
            env: &env,
            root: root.path().to_path_buf(),
            config_dir: root.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        routine_toggle(&ctx, "demo", "nightly", None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("enabled = false") && text.ends_with("Check it.\n"));
        assert!(!crate::routine::parse("nightly", &text).unwrap().enabled);
        routine_toggle(&ctx, "demo", "nightly", None).unwrap();
        assert!(
            crate::routine::parse("nightly", &std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .enabled
        );
        assert!(routine_toggle(&ctx, "demo", "missing", None).is_err());
    }

    #[test]
    fn text_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello").unwrap();
        std::fs::write(dir.path().join("b.png"), [0x89, 0x50, 0, 1]).unwrap();
        assert!(is_text(&dir.path().join("a.md")));
        assert!(!is_text(&dir.path().join("b.png")));
    }
}

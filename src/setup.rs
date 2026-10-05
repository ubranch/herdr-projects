//! `configure` and `unconfigure`: the edits the plugin makes to the user's
//! files, each recorded in an ownership journal so `unconfigure` restores
//! exactly what `configure` changed. Hook files are edited as JSONC through
//! a concrete syntax tree, so comments and formatting survive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde::{Deserialize, Serialize};

use crate::paths::{Ctx, Env};
#[cfg(windows)]
use crate::remote::local_command;
use crate::remote::quote_local;
#[cfg(windows)]
use base64::Engine as _;

/// A harness with a native, user-level hook system that can put text in the
/// model's context. Every other agent learns to report from its thread brief
/// or the coordinator skill alone; adding a harness here is one entry.
pub struct Harness {
    /// Herdr's agent kind, which is also the `hook --agent` value.
    pub agent: &'static str,
    /// The variable that moves the harness's config directory, if any.
    home_env: Option<&'static str>,
    /// The config directory under the home directory.
    home: &'static str,
    /// The hook file, relative to the config directory.
    file: &'static str,
    /// The harness's own names for SessionStart, UserPromptSubmit and PostToolUse.
    pub events: [&'static str; 3],
    /// Flat `{type, command, timeoutSec}` entries in a `version: 1` file of our
    /// own (Copilot CLI) instead of Claude Code's `{matcher, hooks: [...]}`.
    flat: bool,
    /// The hook timeout in the harness's unit.
    timeout: u64,
    /// The injected text goes in top-level `additionalContext`, not `hookSpecificOutput`.
    pub top_level_output: bool,
}

pub const HARNESSES: [Harness; 5] = [
    Harness {
        agent: "claude",
        home_env: Some("CLAUDE_CONFIG_DIR"),
        home: ".claude",
        file: "settings.json",
        events: ["SessionStart", "UserPromptSubmit", "PostToolUse"],
        flat: false,
        timeout: 10,
        top_level_output: false,
    },
    Harness {
        agent: "codex",
        home_env: Some("CODEX_HOME"),
        home: ".codex",
        file: "hooks.json",
        events: ["SessionStart", "UserPromptSubmit", "PostToolUse"],
        flat: false,
        timeout: 10,
        top_level_output: false,
    },
    // Factory Droid: Claude Code's format, in its settings.json.
    Harness {
        agent: "droid",
        home_env: None,
        home: ".factory",
        file: "settings.json",
        events: ["SessionStart", "UserPromptSubmit", "PostToolUse"],
        flat: false,
        timeout: 10,
        top_level_output: false,
    },
    // Gemini CLI: its own event names; timeouts in milliseconds.
    Harness {
        agent: "gemini",
        home_env: None,
        home: ".gemini",
        file: "settings.json",
        events: ["SessionStart", "BeforeAgent", "AfterTool"],
        flat: false,
        timeout: 10_000,
        top_level_output: false,
    },
    // Copilot CLI reads every file in hooks/, so ours is a file of its own.
    // PascalCase event names select its Claude-style payload (snake_case,
    // `hook_event_name`); prompt-submit output is dropped, but the event
    // still clears an answered question.
    Harness {
        agent: "copilot",
        home_env: Some("COPILOT_HOME"),
        home: ".copilot",
        file: "hooks/herdr-projects.json",
        events: ["SessionStart", "UserPromptSubmit", "PostToolUse"],
        flat: true,
        timeout: 10,
        top_level_output: true,
    },
];

pub const AGENTS: [&str; 5] = ["claude", "codex", "droid", "gemini", "copilot"];

pub fn harness(agent: &str) -> Option<&'static Harness> {
    HARNESSES.iter().find(|h| h.agent == agent)
}

/// The harness a hook command was written for (`… hook --agent <name> …`).
fn harness_of(command: &str) -> &'static Harness {
    #[cfg(windows)]
    let decoded = powershell_script(command);
    #[cfg(windows)]
    let command = decoded.as_deref().unwrap_or(command);
    let agent = command
        .rsplit_once(" hook --agent ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .unwrap_or("claude");
    harness(agent).unwrap_or(&HARNESSES[0])
}

/// One file the plugin edited: its text before the first edit, after the last
/// one, what kind of edit, and the hook command (for hook files).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Owned {
    pub before: Option<String>,
    pub after: String,
    pub kind: String,
    #[serde(default)]
    pub command: Option<String>,
}

pub type Journal = BTreeMap<String, Owned>;

pub fn journal_path(config_dir: &Path) -> PathBuf {
    config_dir.join("owned.json")
}

pub fn load_journal(config_dir: &Path) -> Journal {
    crate::project::read_json(&journal_path(config_dir)).unwrap_or_default()
}

pub fn save_journal(config_dir: &Path, journal: &Journal) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    crate::project::write_json(&journal_path(config_dir), journal)
}

/// Reads a config file, refusing a file that is a symbolic link (a dotfile
/// manager's link would be replaced by a plain file) rather than editing it.
pub fn read(path: &Path) -> Result<Option<String>> {
    if let Ok(m) = std::fs::symlink_metadata(path) {
        ensure!(
            !m.file_type().is_symlink(),
            "refusing to edit {}: it is a symbolic link; edit its target's hooks by hand or pass --claude-home/--codex-home",
            path.display()
        );
    }
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Replaces a file's text only if it still reads as `before`.
pub fn replace(path: &Path, before: &Option<String>, after: &str) -> Result<()> {
    ensure!(
        &read(path)? == before,
        "{} changed while configuring; run the command again",
        path.display()
    );
    std::fs::create_dir_all(path.parent().context("config path has no parent")?)?;
    let tmp = path.with_file_name(format!(".herdr-projects-{}.tmp", std::process::id()));
    std::fs::write(&tmp, after)?;
    if path.exists() {
        std::fs::set_permissions(&tmp, std::fs::metadata(path)?.permissions())?;
    }
    if &read(path)? != before {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "{} changed while configuring; run the command again",
            path.display()
        );
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The removal baseline after a repeated `configure`: the original text when
/// nothing else changed in between, else the current text with our entries
/// taken out, so a later `unconfigure` keeps edits made since.
pub fn removal_baseline(previous: &Owned, current: &Owned) -> Result<Option<String>> {
    if current.before.as_ref() == Some(&previous.after) {
        return Ok(previous.before.clone());
    }
    current
        .before
        .as_deref()
        .map(|text| remove_ours(&current.kind, text, current.command.as_deref()))
        .transpose()
}

/// A file's text with only this plugin's entries taken out.
fn remove_ours(kind: &str, text: &str, command: Option<&str>) -> Result<String> {
    match kind {
        "hooks" => hooks(text, command.context("missing hook command")?, true),
        "config" => crate::sidebar::config_edit(
            text,
            &crate::sidebar::Spec {
                key: String::new(),
                tab_command: command.unwrap_or("").to_string(),
            },
            true,
        ),
        other => bail!("unknown ownership kind {other}"),
    }
}

/// Herdr's config file: `HERDR_CONFIG_PATH`, else the platform Herdr config
/// directory (`%APPDATA%\herdr` on Windows).
pub fn herdr_config_path(env: &Env) -> PathBuf {
    if let Some(path) = env.var("HERDR_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    env.herdr_config_dir().join("config.toml")
}

/// The tab-bar command uses absolute paths and has no plugin environment.
/// Herdr's Windows tab-bar runner uses cmd.exe, not the interactive pane shell.
pub fn tab_command(binary: &Path, root: &Path) -> String {
    let command = format!(
        "{} needs-you --line",
        crate::coordinator::command_prefix(binary, root)
    );
    #[cfg(windows)]
    {
        powershell_command(&command)
    }
    #[cfg(not(windows))]
    {
        command
    }
}

#[cfg(windows)]
const POWERSHELL_PREFIX: &str = "pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ";

/// Base64 UTF-16LE avoids both cmd.exe expansion (including `%`) and another
/// PowerShell quoting layer. Hooks and the tab bar are not interactive shells.
#[cfg(windows)]
fn powershell_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "{POWERSHELL_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

#[cfg(windows)]
fn powershell_script(command: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(command.strip_prefix(POWERSHELL_PREFIX)?)
        .ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    String::from_utf16(&units).ok()
}

/// Identifies our tab-bar entry even when a Windows host needs an encoded
/// PowerShell wrapper. Used when replacing or removing a previous install.
pub fn is_tab_command(command: &str) -> bool {
    #[cfg(windows)]
    let decoded = powershell_script(command);
    #[cfg(windows)]
    let command = decoded.as_deref().unwrap_or(command);
    command.contains("needs-you --line")
}

fn hook_entry(harness: &Harness, command: &str) -> serde_json::Value {
    if harness.flat {
        serde_json::json!({"type":"command","command":command,"timeoutSec":harness.timeout})
    } else {
        serde_json::json!({"matcher":"*","hooks":[{"type":"command","command":command,"timeout":harness.timeout}]})
    }
}

fn cst_value(value: &serde_json::Value) -> CstInputValue {
    match value {
        serde_json::Value::Object(map) => {
            CstInputValue::Object(map.iter().map(|(k, v)| (k.clone(), cst_value(v))).collect())
        }
        serde_json::Value::Array(items) => {
            CstInputValue::Array(items.iter().map(cst_value).collect())
        }
        serde_json::Value::String(text) => text.as_str().into(),
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0).into(),
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Null => CstInputValue::Null,
    }
}

/// Whether a command hook is one of ours: it runs `herdr-projects … hook`.
fn is_our_entry(value: &serde_json::Value) -> bool {
    value["command"].as_str().is_some_and(|command| {
        #[cfg(windows)]
        let decoded = powershell_script(command);
        #[cfg(windows)]
        let command = decoded.as_deref().unwrap_or(command);
        command.contains("herdr-projects") && command.contains(" hook --agent ")
    })
}

/// Adds (or removes) the plugin's hook entry under each event, keeping
/// everything else, including comments and foreign commands in mixed matcher
/// groups. Any earlier entry of ours (a moved binary) is replaced on add. Idempotent.
pub fn hooks(input: &str, command: &str, remove: bool) -> Result<String> {
    let harness = harness_of(command);
    let root =
        CstRootNode::parse(input, &Default::default()).context("hook file does not parse")?;
    let obj = root
        .object_value()
        .context("hook configuration must be a JSON object")?;
    if harness.flat && !remove && obj.get("version").is_none() {
        obj.append("version", 1u64.into());
    }
    let hooks = match obj.get("hooks") {
        Some(p) => p.object_value().context("`hooks` must be an object")?,
        None if remove => return Ok(input.into()),
        None => obj
            .append("hooks", CstInputValue::Object(vec![]))
            .object_value()
            .unwrap(),
    };
    let expected = hook_entry(harness, command);
    for event in harness.events {
        let entries = match hooks.get(event) {
            Some(p) => p
                .array_value()
                .with_context(|| format!("`hooks.{event}` must be an array"))?,
            None if remove => continue,
            None => hooks
                .append(event, CstInputValue::Array(vec![]))
                .array_value()
                .unwrap(),
        };
        let mut found = false;
        for entry in entries.elements() {
            let value = entry.to_serde_value();
            if value.as_ref() == Some(&expected) {
                if remove || found {
                    entry.remove();
                } else {
                    found = true;
                }
            } else if value.as_ref().is_some_and(is_our_entry) {
                // Ours, but with another command (the binary moved): replaced.
                entry.remove();
            } else if let (Some(nested), Some(values)) = (
                entry
                    .as_object()
                    .and_then(|group| group.array_value("hooks")),
                value.as_ref().and_then(|group| group["hooks"].as_array()),
            ) {
                let mut remaining = values.len();
                for (hook, value) in nested.elements().into_iter().zip(values) {
                    if is_our_entry(value) {
                        hook.remove();
                        remaining -= 1;
                    }
                }
                if remaining == 0 && !values.is_empty() {
                    entry.remove();
                }
            }
        }
        if !remove && !found {
            entries.append(cst_value(&expected));
        }
    }
    Ok(root.to_string())
}

/// The hook command for a harness: the absolute binary path and the root,
/// because hooks run outside the plugin environment.
/// It always exits 0 and never writes to standard error: harnesses treat a
/// failing UserPromptSubmit hook (exit 2) as "block this prompt", in every
/// session on the machine, so a missing or older binary must not do that.
pub fn hook_command(binary: &Path, root: &Path, agent: &str) -> String {
    let command = format!(
        "{} hook --agent {}",
        crate::coordinator::command_prefix(binary, root),
        quote_local(agent)
    );
    #[cfg(windows)]
    {
        powershell_command(&format!("try {{ {command} 2>$null }} catch {{}}; exit 0"))
    }
    #[cfg(not(windows))]
    {
        format!("{command} 2>/dev/null || true")
    }
}

/// Where each harness keeps its hooks; `--claude-home`/`--codex-home`
/// override those two.
pub fn hook_file(
    env: &Env,
    agent: &str,
    claude_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> PathBuf {
    let harness = harness(agent).unwrap_or(&HARNESSES[0]);
    let flag = match agent {
        "claude" => claude_home,
        "codex" => codex_home,
        _ => None,
    };
    flag.map(Path::to_path_buf)
        .or_else(|| {
            harness
                .home_env
                .and_then(|name| env.var(name))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| env.home.join(harness.home))
        .join(harness.file)
}

/// The harness's config directory: `configure` without `--clients` picks the
/// harnesses whose directory exists.
pub fn harness_installed(
    env: &Env,
    agent: &str,
    claude_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> bool {
    let file = hook_file(env, agent, claude_home, codex_home);
    let depth = harness(agent).map_or(1, |h| h.file.split('/').count());
    file.ancestors().nth(depth).is_some_and(Path::is_dir)
}

/// The skill bundled with the plugin, linked into each harness by `configure`.
pub const SKILL: &str = "autoproject";

/// The bundled skill in the plugin checkout this binary was built in, so the
/// link follows the installed plugin, not the directory `configure` ran in.
pub fn skill_source() -> Option<PathBuf> {
    crate::update::own_root().map(|root| root.join("skill").join(SKILL))
}

/// Where a harness looks for user skills: Claude Code's `<config dir>/skills`,
/// Codex's user scope `~/.agents/skills` (not under `CODEX_HOME`). A skills
/// directory that is itself a link is resolved, so a shared directory gets
/// one link and one journal key; a missing one is resolved through its
/// parent, so the key stays the same once it exists.
pub fn skill_link(env: &Env, agent: &str, claude_home: Option<&Path>) -> PathBuf {
    let dir = match agent {
        "claude" => claude_home
            .map(Path::to_path_buf)
            .or_else(|| env.var("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .unwrap_or_else(|| env.home.join(".claude"))
            .join("skills"),
        _ => env.home.join(".agents/skills"),
    };
    let resolved = crate::paths::canonicalize(&dir).or_else(|_| {
        crate::paths::canonicalize(dir.parent().unwrap_or(&dir)).map(|p| p.join("skills"))
    });
    resolved.unwrap_or(dir).join(SKILL)
}

#[derive(Debug, PartialEq)]
pub enum SkillState {
    /// A link to `source`.
    Ours,
    Missing,
    /// A link to somewhere else: ours from an older checkout when journaled.
    Elsewhere(PathBuf),
    /// A directory or file: never touched.
    Foreign,
}

pub fn skill_state(link: &Path, source: &Path) -> SkillState {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return SkillState::Missing;
    };
    if !meta.file_type().is_symlink() {
        return SkillState::Foreign;
    }
    match std::fs::read_link(link) {
        Ok(target)
            if target == source
                || crate::paths::canonicalize(&target)
                    .ok()
                    .zip(crate::paths::canonicalize(source).ok())
                    .is_some_and(|(a, b)| a == b) =>
        {
            SkillState::Ours
        }
        Ok(target) => SkillState::Elsewhere(target),
        Err(_) => SkillState::Foreign,
    }
}

fn remove_skill_link(link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::fs::remove_dir(link)
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(link)
    }
}

/// Junctions need neither elevation nor Developer Mode and follow plugin
/// updates, unlike copying a skill directory. Removal deletes only the link.
fn create_skill_link(
    source: &Path,
    link: &Path,
    _runner: &dyn crate::runner::Runner,
) -> Result<()> {
    #[cfg(windows)]
    {
        let source = crate::paths::canonicalize(source)?;
        let script = local_command(
            "New-Item",
            &[
                "-ItemType",
                "Junction",
                "-Path",
                &link.to_string_lossy(),
                "-Target",
                &source.to_string_lossy(),
                "-ErrorAction",
                "Stop",
            ],
        );
        let out = _runner.run(
            &crate::runner::Cmd::new("pwsh.exe", std::time::Duration::from_secs(10)).args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("{script} | Out-Null"),
            ]),
        )?;
        ensure!(
            out.success(),
            "could not link {}: {}",
            link.display(),
            out.error_text()
        );
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(source, link)?;
    Ok(())
}

pub struct ConfigureOptions {
    /// Harnesses from [`AGENTS`]; empty means every one whose config
    /// directory exists.
    pub clients: Vec<String>,
    pub claude_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub dry_run: bool,
    /// Install the progress hooks; `false` links only the skill (`doctor --fix`).
    pub hooks: bool,
    /// Also edit Herdr's config.toml: sidebar rows, popup key, tab-bar entry.
    pub sidebar: bool,
    /// The popup key (default: the one already configured, else `prefix+a`).
    pub key: Option<String>,
    pub herdr_config: Option<PathBuf>,
    /// The skill directory to link (`skill_source()`); `None` links nothing.
    pub skill: Option<PathBuf>,
}

/// Whether the standalone agent-progress plugin's hooks are installed in a
/// hook file: `doctor` tells the user to remove them with that plugin's own
/// `unconfigure`, since this plugin never edits another plugin's entries.
pub fn has_agent_progress_hooks(text: &str) -> bool {
    text.contains("herdr-progress") && text.contains(" hook --agent ")
}

/// Installs the hooks. Every edit is journaled before it is made, so a killed
/// run never leaves hooks `unconfigure` cannot identify as its own.
pub fn configure(ctx: &Ctx, options: &ConfigureOptions) -> Result<Vec<String>> {
    let binary = crate::paths::binary()?;
    let clients: Vec<String> = if options.clients.is_empty() {
        AGENTS
            .into_iter()
            .filter(|c| {
                harness_installed(
                    ctx.env,
                    c,
                    options.claude_home.as_deref(),
                    options.codex_home.as_deref(),
                )
            })
            .map(str::to_owned)
            .collect()
    } else {
        options.clients.clone()
    };
    let mut journal = load_journal(&ctx.config_dir);
    let mut edits: Vec<(PathBuf, Owned)> = Vec::new();
    let mut notes = Vec::new();
    for client in clients
        .iter()
        .filter(|c| options.hooks && harness(c).is_some())
    {
        let file = hook_file(
            ctx.env,
            client,
            options.claude_home.as_deref(),
            options.codex_home.as_deref(),
        );
        let command = hook_command(&binary, &ctx.root, client);
        let before = read(&file)?;
        let after = hooks(before.as_deref().unwrap_or("{}"), &command, false)?;
        if before.as_deref().is_some_and(has_agent_progress_hooks) {
            notes.push(format!("{} also runs the standalone agent-progress hooks; run that plugin's `unconfigure` (see `doctor`) so only one set fires", file.display()));
        }
        if before.as_deref() == Some(after.as_str()) {
            notes.push(format!("{}: hooks already in place", file.display()));
            continue;
        }
        notes.push(format!(
            "{}: {} hook entries for `{command}`",
            file.display(),
            if before.is_some() {
                "adding"
            } else {
                "creating with"
            }
        ));
        edits.push((
            file,
            Owned {
                before,
                after,
                kind: "hooks".into(),
                command: Some(command),
            },
        ));
    }
    let mut links: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    if let Some(source) = &options.skill {
        let mut seen = Vec::new();
        for client in clients
            .iter()
            .filter(|c| matches!(c.as_str(), "claude" | "codex"))
        {
            let link = skill_link(ctx.env, client, options.claude_home.as_deref());
            if seen.contains(&link) {
                continue;
            }
            seen.push(link.clone());
            if !source.join("SKILL.md").is_file() {
                notes.push(format!(
                    "{}: no bundled skill at {}; not linked",
                    link.display(),
                    source.display()
                ));
                break;
            }
            let journaled = journal
                .get(&link.to_string_lossy().into_owned())
                .is_some_and(|o| o.kind == "skill");
            match skill_state(&link, source) {
                SkillState::Ours => {
                    notes.push(format!("{}: skill link already in place", link.display()));
                    if !journaled {
                        links.push((link, None));
                    }
                }
                SkillState::Missing => {
                    notes.push(format!(
                        "{}: linking the `{SKILL}` skill to {}",
                        link.display(),
                        source.display()
                    ));
                    links.push((link, Some(source.clone())));
                }
                SkillState::Elsewhere(old) if journaled => {
                    notes.push(format!(
                        "{}: relinking the `{SKILL}` skill from {} to {}",
                        link.display(),
                        old.display(),
                        source.display()
                    ));
                    links.push((link, Some(source.clone())));
                }
                SkillState::Elsewhere(_) | SkillState::Foreign => {
                    notes.push(format!("{}: left alone, it is not this plugin's link; move it away and run `configure` again to install the bundled `{SKILL}` skill", link.display()));
                }
            }
        }
    }
    if options.sidebar {
        let file = options
            .herdr_config
            .clone()
            .unwrap_or_else(|| herdr_config_path(ctx.env));
        let before = read(&file)?;
        let text = before.clone().unwrap_or_default();
        let current_key = text.parse::<toml_edit::DocumentMut>().ok().and_then(|doc| {
            doc.get("keys")?
                .get("command")?
                .as_array_of_tables()?
                .iter()
                .find(|t| {
                    t.get("command").and_then(|c| c.as_str()) == Some(crate::sidebar::POPUP_ACTION)
                })?
                .get("key")?
                .as_str()
                .map(str::to_string)
        });
        let key = options
            .key
            .clone()
            .or(current_key)
            .unwrap_or_else(|| crate::sidebar::DEFAULT_KEY.to_string());
        let defaults = ctx
            .runner
            .run(
                &crate::runner::Cmd::new(ctx.env.herdr_bin(), crate::herdr::CALL_TIMEOUT)
                    .arg("--default-config"),
            )
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout)
            .unwrap_or_default();
        let builtin = crate::sidebar::builtin_keys(&defaults);
        if builtin.is_empty() {
            notes.push("could not read Herdr's built-in key map (`herdr --default-config`); the popup key was checked against your config only".into());
        }
        if let Some(conflict) = crate::sidebar::key_conflict(&text, &key, &builtin) {
            bail!("{conflict}; pick another popup key with `configure --key <key>`");
        }
        let command = tab_command(&binary, &ctx.root);
        let after = crate::sidebar::config_edit(
            &text,
            &crate::sidebar::Spec {
                key: key.clone(),
                tab_command: command.clone(),
            },
            false,
        )?;
        if before.as_deref() == Some(after.as_str()) {
            notes.push(format!(
                "{}: sidebar rows, popup key `{key}` and tab-bar entry already in place",
                file.display()
            ));
        } else {
            crate::sidebar::check_config(
                &ctx.env.herdr_bin(),
                ctx.runner,
                &after,
                &ctx.config_dir,
            )?;
            notes.push(format!(
                "{}: adding the project grouping rows, the popup key `{key}` and the tab-bar entry",
                file.display()
            ));
            edits.push((
                file,
                Owned {
                    before,
                    after,
                    kind: "config".into(),
                    command: Some(command),
                },
            ));
        }
    }
    if options.dry_run {
        return Ok(notes);
    }
    for (path, edit) in &edits {
        let key = path.to_string_lossy().into_owned();
        let mut owned = edit.clone();
        if let Some(previous) = journal.get(&key) {
            owned.before = removal_baseline(previous, &owned)?;
        }
        journal.insert(key, owned);
    }
    let source = options
        .skill
        .as_ref()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for (link, _) in &links {
        journal.insert(
            link.to_string_lossy().into_owned(),
            Owned {
                before: None,
                after: source.clone(),
                kind: "skill".into(),
                command: None,
            },
        );
    }
    save_journal(&ctx.config_dir, &journal)?;
    for (index, (path, edit)) in edits.iter().enumerate() {
        if let Err(error) = replace(path, &edit.before, &edit.after) {
            for (path, edit) in edits[..index].iter().rev() {
                if read(path)?.as_deref() == Some(edit.after.as_str()) {
                    match &edit.before {
                        Some(text) => replace(path, &Some(edit.after.clone()), text)?,
                        None => std::fs::remove_file(path)?,
                    }
                }
            }
            return Err(error);
        }
    }
    for (link, source) in &links {
        let Some(source) = source else { continue };
        if std::fs::symlink_metadata(link).is_ok() {
            remove_skill_link(link)?;
        }
        std::fs::create_dir_all(link.parent().context("skill link has no parent")?)?;
        create_skill_link(source, link, ctx.runner)
            .with_context(|| format!("could not link {}", link.display()))?;
    }
    Ok(notes)
}

/// Removes exactly what `configure` added: the file goes back to its journaled
/// text when nothing else changed, else only our entries are taken out.
pub fn unconfigure(ctx: &Ctx) -> Result<Vec<String>> {
    let journal = load_journal(&ctx.config_dir);
    let mut notes = Vec::new();
    let mut remaining = journal.clone();
    for (key, owned) in &journal {
        let path = Path::new(key);
        if owned.kind == "skill" {
            match skill_state(path, Path::new(&owned.after)) {
                SkillState::Ours => {
                    remove_skill_link(path)?;
                    notes.push(format!("{key}: skill link removed"));
                }
                SkillState::Missing => notes.push(format!("{key}: already gone")),
                _ => notes.push(format!("{key}: no longer this plugin's link; left alone")),
            }
            remaining.remove(key);
            continue;
        }
        let current = read(path)?;
        if current.as_deref() == Some(owned.after.as_str()) {
            match &owned.before {
                Some(text) => replace(path, &current, text)?,
                None => std::fs::remove_file(path)?,
            }
            notes.push(format!("{key}: restored"));
        } else if let Some(text) = &current {
            let cleaned = remove_ours(&owned.kind, text, owned.command.as_deref())?;
            if cleaned != *text {
                replace(path, &current, &cleaned)?;
            }
            notes.push(format!(
                "{key}: edited since configure; only the plugin's entries were removed"
            ));
        } else {
            notes.push(format!("{key}: already gone"));
        }
        remaining.remove(key);
    }
    save_journal(&ctx.config_dir, &remaining)?;
    if journal.is_empty() {
        notes.push("nothing was configured".into());
    }
    Ok(notes)
}

/// The session the user's shell or the plugin action talks to, if reachable.
fn session_herdr<'a>(ctx: &'a Ctx) -> Option<crate::herdr::Herdr<'a>> {
    let session =
        crate::paths::resolve_session(&crate::paths::SessionFlags::default(), ctx.env, ctx.runner)
            .ok()?;
    let herdr = crate::herdr::Herdr::new(ctx.env.herdr_bin(), &session.socket, ctx.runner);
    herdr.reachable().then_some(herdr)
}

/// `herdr server reload-config`, so server-side settings (the tab-bar entry,
/// keys) apply without a restart.
pub fn reload_config(ctx: &Ctx) {
    if let Some(herdr) = session_herdr(ctx) {
        match herdr.call(&["server", "reload-config"], crate::herdr::CALL_TIMEOUT) {
            Ok(_) => println!("reloaded the Herdr server's config"),
            Err(error) => println!(
                "could not reload the Herdr config ({error}); run `herdr server reload-config`"
            ),
        }
    }
}

/// After `configure`: reload, then the default by-need agent order.
pub fn apply_live(ctx: &Ctx) {
    reload_config(ctx);
    apply_view(ctx);
    println!(
        "Sidebar rows are drawn by your Herdr client: if they are not visible yet, run `reload config` in Herdr (prefix+shift+r)."
    );
}

/// The default agent view, once the sidebar is configured. Herdr holds one
/// view and has no way to read it, so this replaces another tool's view; it
/// is applied at startup, after `configure` and on `unfocus` only.
pub fn apply_view(ctx: &Ctx) {
    let configured = load_journal(&ctx.config_dir)
        .values()
        .any(|o| o.kind == "config");
    if !configured {
        return;
    }
    if let Some(herdr) = session_herdr(ctx) {
        let _ = herdr.agent_view_set(crate::sidebar::default_view());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "'/p/herdr-projects' --root /r hook --agent claude 2>/dev/null || true";

    #[test]
    fn the_hook_command_never_fails_even_with_a_missing_binary() {
        let command = hook_command(
            Path::new("/no/such/herdr-projects"),
            Path::new("/r"),
            "claude",
        );
        #[cfg(windows)]
        let out = std::process::Command::new(crate::paths::windows_cmd())
            .args(["/d", "/c", &command])
            .output()
            .unwrap();
        #[cfg(not(windows))]
        let out = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "status: {}; stdout: {}; stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "stdout: {}; stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn generated_hook_and_tab_commands_execute_in_the_host_shell() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp
            .path()
            .join("it's a %HP_UNKNOWN% $(root) hook --agent droid \u{03bb}");
        std::fs::create_dir(&root).unwrap();
        let binary = temp.path().join(if cfg!(windows) {
            "it's %HP_UNKNOWN% herdr-projects.ps1"
        } else {
            "it's %HP_UNKNOWN% herdr-projects.sh"
        });
        if cfg!(windows) {
            std::fs::write(&binary, "$ErrorActionPreference = 'Stop'\nif ($args[0] -ne '--root') { exit 3 }\nif ($args[2] -eq 'hook') {\n  [IO.File]::WriteAllText([IO.Path]::Combine($args[1], 'hook.txt'), $args[4])\n  [Console]::Out.Write('{\"ok\":true}')\n} elseif ($args[2] -eq 'needs-you') {\n  [IO.File]::WriteAllText([IO.Path]::Combine($args[1], 'tab.txt'), 'ran')\n  [Console]::Out.Write('projects: 1 need you')\n} else { exit 4 }\n").unwrap();
        } else {
            std::fs::write(&binary, "#!/bin/sh\nset -e\ntest \"$1\" = --root\ncase \"$3\" in\nhook) printf %s \"$5\" > \"$2/hook.txt\"; printf '%s' '{\"ok\":true}' ;;\nneeds-you) printf ran > \"$2/tab.txt\"; printf '%s' 'projects: 1 need you' ;;\n*) exit 4 ;;\nesac\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let run = |command: &str| {
            #[cfg(windows)]
            {
                std::process::Command::new(crate::paths::windows_cmd())
                    .args(["/d", "/c", command])
                    .env("PSExecutionPolicyPreference", "Bypass")
                    .output()
                    .unwrap()
            }
            #[cfg(not(windows))]
            {
                std::process::Command::new("sh")
                    .args(["-c", command])
                    .output()
                    .unwrap()
            }
        };
        for agent in ["claude", "copilot", "gemini"] {
            let command = hook_command(&binary, &root, agent);
            let out = run(&command);
            assert!(
                out.status.success(),
                "status: {}; stdout: {}; stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                out.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(out.stdout, b"{\"ok\":true}");
            assert_eq!(
                std::fs::read_to_string(root.join("hook.txt")).unwrap(),
                agent
            );
            let configured = hooks("{}", &command, false).unwrap();
            let value: serde_json::Value = serde_json::from_str(&configured).unwrap();
            for event in harness(agent).unwrap().events {
                let entry = &value["hooks"][event][0];
                assert_eq!(
                    if agent == "copilot" {
                        &entry["command"]
                    } else {
                        &entry["hooks"][0]["command"]
                    },
                    &serde_json::Value::String(command.clone())
                );
            }
            let moved = hook_command(&temp.path().join("moved/herdr-projects"), &root, agent);
            let replaced = hooks(&configured, &moved, false).unwrap();
            let removed = hooks(&replaced, &moved, true).unwrap();
            assert!(!removed.contains(&moved));
        }
        let command = tab_command(&binary, &root);
        assert!(is_tab_command(&command));
        let out = run(&command);
        assert!(
            out.status.success(),
            "status: {}; stdout: {}; stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"projects: 1 need you");
        assert_eq!(
            std::fs::read_to_string(root.join("tab.txt")).unwrap(),
            "ran"
        );
    }

    #[test]
    fn existing_hooks_comments_and_user_edits_survive() {
        let original = "{\n// user's comment\n\"theme\": \"dark\",\"hooks\":{\"SessionStart\":[{\"hooks\":[{\"command\":\"keep\"}]}]}}";
        let added = hooks(original, CMD, false).unwrap();
        assert!(added.contains("// user's comment"));
        assert!(added.contains("keep"));
        assert_eq!(added.matches("hook --agent claude").count(), 3);
        assert_eq!(hooks(&added, CMD, false).unwrap(), added);
        let removed = hooks(&added, CMD, true).unwrap();
        assert!(!removed.contains("herdr-projects"));
        assert!(removed.contains("keep"));
        assert!(hooks("[]", CMD, false).is_err());
    }

    #[test]
    fn a_moved_binary_replaces_the_old_entries_and_other_plugins_are_left_alone() {
        let old = hooks("{}", CMD, false).unwrap();
        let moved = hooks(
            &old,
            "'/new/herdr-projects' --root /r hook --agent claude",
            false,
        )
        .unwrap();
        assert!(!moved.contains("/p/herdr-projects"));
        assert_eq!(moved.matches("/new/herdr-projects").count(), 3);
        let with_other = "{\"hooks\":{\"PostToolUse\":[{\"matcher\":\"*\",\"hooks\":[{\"type\":\"command\",\"command\":\"'/x/herdr-progress' hook --agent claude\",\"timeout\":10}]}]}}";
        let added = hooks(with_other, CMD, false).unwrap();
        assert!(added.contains("herdr-progress"));
        assert!(has_agent_progress_hooks(&added));
        let removed = hooks(&added, CMD, true).unwrap();
        assert!(removed.contains("herdr-progress") && !removed.contains("herdr-projects"));
    }

    #[test]
    fn each_harness_gets_its_own_file_event_names_and_entry_shape() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[("COPILOT_HOME", "/c")]);
        assert_eq!(
            hook_file(&env, "droid", None, None),
            home.path().join(".factory/settings.json")
        );
        assert_eq!(
            hook_file(&env, "gemini", None, None),
            home.path().join(".gemini/settings.json")
        );
        assert_eq!(
            hook_file(&env, "copilot", None, None),
            Path::new("/c/hooks/herdr-projects.json")
        );
        // Copilot's hooks/ folder may not exist yet: its config directory counts.
        std::fs::create_dir_all(home.path().join(".copilot")).unwrap();
        let env = Env::for_test(home.path(), &[]);
        assert!(harness_installed(&env, "copilot", None, None));
        assert!(!harness_installed(&env, "gemini", None, None));

        let gemini = hooks(
            "{\"theme\":\"x\"}",
            "/b/herdr-projects --root /r hook --agent gemini",
            false,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&gemini).unwrap();
        for event in ["SessionStart", "BeforeAgent", "AfterTool"] {
            assert_eq!(
                value["hooks"][event][0]["hooks"][0]["timeout"], 10_000,
                "{gemini}"
            );
        }
        assert!(value["hooks"]["PostToolUse"].is_null());

        let command = "/b/herdr-projects --root /r hook --agent copilot";
        let copilot = hooks("{}", command, false).unwrap();
        let value: serde_json::Value = serde_json::from_str(&copilot).unwrap();
        assert_eq!(value["version"], 1);
        for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
            assert_eq!(
                value["hooks"][event],
                serde_json::json!([{"type":"command","command":command,"timeoutSec":10}]),
                "{copilot}"
            );
        }
        assert_eq!(hooks(&copilot, command, false).unwrap(), copilot);
        let moved = hooks(
            &copilot,
            "/new/herdr-projects --root /r hook --agent copilot",
            false,
        )
        .unwrap();
        assert_eq!(moved.matches("herdr-projects").count(), 3);
        assert!(
            !hooks(&copilot, command, true)
                .unwrap()
                .contains("herdr-projects")
        );
    }

    #[test]
    fn removal_baseline_keeps_the_original_or_the_users_later_edits() {
        let previous = Owned {
            before: Some("original".into()),
            after: "configured".into(),
            kind: "hooks".into(),
            command: Some(CMD.into()),
        };
        let unchanged = Owned {
            before: Some("configured".into()),
            after: "configured2".into(),
            kind: "hooks".into(),
            command: Some(CMD.into()),
        };
        assert_eq!(
            removal_baseline(&previous, &unchanged).unwrap().as_deref(),
            Some("original")
        );
        let edited_text = format!("{}\n", hooks("{\"theme\":\"dark\"}", CMD, false).unwrap());
        let edited = Owned {
            before: Some(edited_text.clone()),
            after: "x".into(),
            kind: "hooks".into(),
            command: Some(CMD.into()),
        };
        let baseline = removal_baseline(&previous, &edited).unwrap().unwrap();
        assert!(baseline.contains("dark") && !baseline.contains("herdr-projects"));
    }

    #[test]
    fn configure_and_unconfigure_round_trip_byte_for_byte_and_keep_user_additions() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let claude = home.path().join("claude");
        let codex = home.path().join("codex");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        let original = "{\n  // mine\n  \"permissions\": {\"allow\": [\"Bash(ls:*)\"]},\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"say done\"}]}]}\n}\n";
        std::fs::write(claude.join("settings.json"), original).unwrap();
        let runner = crate::runner::fake::FakeRunner::new();
        let ctx = Ctx {
            env: &env,
            root: home.path().join("root"),
            config_dir: home.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        let options = ConfigureOptions {
            clients: vec![],
            claude_home: Some(claude.clone()),
            codex_home: Some(codex.clone()),
            dry_run: true,
            hooks: true,
            sidebar: false,
            key: None,
            herdr_config: None,
            skill: None,
        };
        let notes = configure(&ctx, &options).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            original,
            "dry run changed a file"
        );

        let options = ConfigureOptions {
            dry_run: false,
            ..options
        };
        configure(&ctx, &options).unwrap();
        let configured = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        assert!(configured.contains("// mine") && configured.contains("say done"));
        let configured_json = CstRootNode::parse(&configured, &Default::default())
            .unwrap()
            .to_serde_value()
            .unwrap();
        assert_eq!(
            configured_json["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            hook_command(&crate::paths::binary().unwrap(), &ctx.root, "claude")
        );
        let codex_text = std::fs::read_to_string(codex.join("hooks.json")).unwrap();
        let codex_json: serde_json::Value = serde_json::from_str(&codex_text).unwrap();
        assert_eq!(
            codex_json["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            hook_command(&crate::paths::binary().unwrap(), &ctx.root, "codex")
        );
        assert_eq!(load_journal(&ctx.config_dir).len(), 2);
        // Idempotent.
        configure(&ctx, &options).unwrap();
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            configured
        );

        // Unconfigure: byte-identical when nothing else changed; the created file is removed.
        unconfigure(&ctx).unwrap();
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            original
        );
        assert!(!codex.join("hooks.json").exists());
        assert!(load_journal(&ctx.config_dir).is_empty());

        // A user edit made after configure survives unconfigure.
        configure(&ctx, &options).unwrap();
        let text = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        std::fs::write(
            claude.join("settings.json"),
            text.replacen("{\n", "{\n  \"model\": \"opus\",\n", 1),
        )
        .unwrap();
        unconfigure(&ctx).unwrap();
        let after = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        assert!(
            after.contains("\"model\": \"opus\"")
                && after.contains("say done")
                && !after.contains("herdr-projects")
        );
    }

    #[test]
    fn configure_refresh_and_unconfigure_preserve_mixed_hook_groups() {
        for agent in ["claude", "codex", "droid", "gemini"] {
            let home = tempfile::tempdir().unwrap();
            let env = Env::for_test(home.path(), &[]);
            let runner = crate::runner::fake::FakeRunner::new();
            let ctx = Ctx {
                env: &env,
                root: home.path().join("root"),
                config_dir: home.path().join("cfg"),
                runner: &runner,
                detached_ticker: false,
            };
            let options = ConfigureOptions {
                clients: vec![agent.into()],
                claude_home: None,
                codex_home: None,
                dry_run: false,
                hooks: true,
                sidebar: false,
                key: None,
                herdr_config: None,
                skill: None,
            };
            configure(&ctx, &options).unwrap();
            let file = hook_file(&env, agent, None, None);
            let events = harness(agent).unwrap().events;
            let native_hook = serde_json::json!({"type":"command","command":hook_command(&home.path().join("herdr-projects"), &ctx.root, agent),"timeout":harness(agent).unwrap().timeout});
            let plain_hook = serde_json::json!({"type":"command","command":format!("/old/herdr-projects --root /r hook --agent {agent}"),"timeout":10});
            let foreign_before = serde_json::json!({"type":"command","command":"notify-before","timeout":42,"userField":true});
            let foreign_after =
                serde_json::json!({"type":"command","command":"notify-after","timeout":17});
            let event_groups = events.map(|event| {
                format!(
                    r#""{event}": [
                    {{
                        // user's matcher
                        "matcher": "Edit",
                        "userField": {{"keep": true}},
                        "hooks": [
                            {foreign_before}, // user's first command
                            {native_hook},
                            {plain_hook},
                            // user's second command
                            {foreign_after}
                        ]
                    }},
                    {{"matcher":"*","hooks":[{native_hook}]}}
                ]"#
                )
            });
            let edited = format!(
                "{{\n// user's preferences\n\"theme\":\"dark\",\"hooks\":{{{},\"Stop\":[{{\"command\":\"notify-stop\"}}]}}\n}}\n",
                event_groups.join(",\n")
            );
            std::fs::write(&file, &edited).unwrap();
            let refreshed_ctx = Ctx {
                root: home.path().join("moved-root"),
                ..ctx
            };
            let command =
                hook_command(&crate::paths::binary().unwrap(), &refreshed_ctx.root, agent);
            let foreign_group = serde_json::json!({"matcher":"Edit","userField":{"keep":true},"hooks":[foreign_before,foreign_after]});
            let managed_group = serde_json::json!({"matcher":"*","hooks":[{"type":"command","command":command,"timeout":if agent == "gemini" { 10_000 } else { 10 }}]});
            let assert_config = |text: &str, managed: bool| {
                let value = CstRootNode::parse(text, &Default::default())
                    .unwrap()
                    .to_serde_value()
                    .unwrap();
                assert_eq!(value["theme"], "dark", "{agent}");
                assert_eq!(
                    value["hooks"]["Stop"],
                    serde_json::json!([{"command":"notify-stop"}]),
                    "{agent}"
                );
                for event in events {
                    let expected = if managed {
                        serde_json::json!([foreign_group, managed_group])
                    } else {
                        serde_json::json!([foreign_group])
                    };
                    assert_eq!(value["hooks"][event], expected, "{agent}: {event}");
                }
                for comment in [
                    "// user's preferences",
                    "// user's matcher",
                    "// user's first command",
                    "// user's second command",
                ] {
                    assert!(text.contains(comment), "{agent}: lost {comment}");
                }
            };
            configure(&refreshed_ctx, &options).unwrap();
            let refreshed = std::fs::read_to_string(&file).unwrap();
            assert_config(&refreshed, true);
            configure(&refreshed_ctx, &options).unwrap();
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                refreshed,
                "{agent}: refresh duplicated hooks"
            );
            unconfigure(&refreshed_ctx).unwrap();
            assert_config(&std::fs::read_to_string(&file).unwrap(), false);

            // A later mixed-group edit survives unconfigure without refreshing first.
            configure(&refreshed_ctx, &options).unwrap();
            let installed = std::fs::read_to_string(&file).unwrap();
            let root = CstRootNode::parse(&installed, &Default::default()).unwrap();
            let late_hook = serde_json::json!({"type":"command","command":"notify-later","timeout":23,"userField":"keep"});
            for event in events {
                let groups = root
                    .object_value()
                    .unwrap()
                    .object_value("hooks")
                    .unwrap()
                    .array_value(event)
                    .unwrap();
                let group = groups.elements()[1].as_object().unwrap();
                group.append("userField", "later".into());
                let commands = group.array_value("hooks").unwrap();
                commands.elements().into_iter().next().unwrap().remove();
                commands.append(cst_value(&native_hook));
                commands.append(cst_value(&late_hook));
            }
            std::fs::write(&file, root.to_string()).unwrap();
            unconfigure(&refreshed_ctx).unwrap();
            let removed = std::fs::read_to_string(&file).unwrap();
            let value = CstRootNode::parse(&removed, &Default::default())
                .unwrap()
                .to_serde_value()
                .unwrap();
            for event in events {
                assert_eq!(
                    value["hooks"][event],
                    serde_json::json!([foreign_group, {"matcher":"*","userField":"later","hooks":[late_hook]}]),
                    "{agent}: {event}"
                );
            }
            assert_eq!(value["theme"], "dark", "{agent}");
            assert_eq!(
                value["hooks"]["Stop"],
                serde_json::json!([{"command":"notify-stop"}]),
                "{agent}"
            );
            for comment in [
                "// user's preferences",
                "// user's matcher",
                "// user's first command",
                "// user's second command",
            ] {
                assert!(removed.contains(comment), "{agent}: lost {comment}");
            }
            assert!(
                load_journal(&refreshed_ctx.config_dir).is_empty(),
                "{agent}"
            );
        }
    }

    #[test]
    fn flat_hook_refresh_preserves_foreign_commands_and_removes_owned_duplicates() {
        let home = tempfile::tempdir().unwrap();
        let command = hook_command(&home.path().join("herdr-projects"), home.path(), "copilot");
        let old_command = hook_command(
            &home.path().join("old/herdr-projects"),
            home.path(),
            "copilot",
        );
        let plain_command = "/old/herdr-projects --root /r hook --agent copilot";
        let foreign = serde_json::json!({"type":"command","command":"notify-user","timeoutSec":25,"userField":true});
        let managed = serde_json::json!({"type":"command","command":command,"timeoutSec":10});
        let mut input = serde_json::json!({"version":1,"userField":{"keep":true},"hooks":{}});
        let mut expected = input.clone();
        for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
            input["hooks"][event] = serde_json::json!([
                foreign,
                {"type":"command","command":old_command,"timeoutSec":10},
                {"type":"command","command":plain_command,"timeoutSec":10},
                managed,
                managed
            ]);
            expected["hooks"][event] = serde_json::json!([foreign, managed]);
        }
        let refreshed = hooks(&input.to_string(), &command, false).unwrap();
        let value: serde_json::Value = serde_json::from_str(&refreshed).unwrap();
        assert_eq!(value, expected);
        assert_eq!(hooks(&refreshed, &command, false).unwrap(), refreshed);
        let removed = hooks(&refreshed, &command, true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&removed).unwrap();
        for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
            expected["hooks"][event] = serde_json::json!([foreign]);
        }
        assert_eq!(value, expected);
    }

    #[test]
    fn the_skill_is_linked_once_into_a_shared_skills_dir_and_foreign_ones_are_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let claude = home.path().join("claude");
        let shared = home.path().join(".agents/skills");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(home.path().join("codex")).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        // Like this Mac: Claude's skills dir is itself a link to ~/.agents/skills.
        create_skill_link(&shared, &claude.join("skills"), &crate::runner::RealRunner).unwrap();
        let source = home.path().join("plugin/skill/autoproject");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "---\nname: autoproject\n---\n").unwrap();
        let runner = crate::runner::RealRunner;
        let ctx = Ctx {
            env: &env,
            root: home.path().join("root"),
            config_dir: home.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        let options = |dry_run: bool, skill: &Path| ConfigureOptions {
            clients: vec![],
            claude_home: Some(claude.clone()),
            codex_home: Some(home.path().join("codex")),
            dry_run,
            hooks: true,
            sidebar: false,
            key: None,
            herdr_config: None,
            skill: Some(skill.to_path_buf()),
        };
        let link = crate::paths::canonicalize(&shared).unwrap().join(SKILL);

        // A plain directory already there (the old personal copy) is never touched.
        std::fs::create_dir_all(shared.join(SKILL)).unwrap();
        let notes = configure(&ctx, &options(false, &source)).unwrap();
        assert!(notes.iter().any(|n| n.contains("left alone")), "{notes:?}");
        assert_eq!(skill_state(&link, &source), SkillState::Foreign);
        std::fs::remove_dir(shared.join(SKILL)).unwrap();
        unconfigure(&ctx).unwrap();

        // Dry run: nothing linked.
        configure(&ctx, &options(true, &source)).unwrap();
        assert_eq!(skill_state(&link, &source), SkillState::Missing);

        let notes = configure(&ctx, &options(false, &source)).unwrap();
        assert_eq!(
            notes.iter().filter(|n| n.contains("linking")).count(),
            1,
            "{notes:?}"
        );
        assert_eq!(skill_state(&link, &source), SkillState::Ours);
        assert!(claude.join("skills").join(SKILL).join("SKILL.md").is_file());
        assert!(
            claude.join("skills").is_symlink(),
            "the shared dir link was replaced"
        );

        // A moved plugin checkout relinks our own link.
        let moved = home.path().join("moved/skill/autoproject");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(moved.join("SKILL.md"), "x").unwrap();
        configure(&ctx, &options(false, &moved)).unwrap();
        assert_eq!(skill_state(&link, &moved), SkillState::Ours);

        // Unconfigure removes only our link; a foreign link in its place survives.
        unconfigure(&ctx).unwrap();
        assert_eq!(skill_state(&link, &moved), SkillState::Missing);
        assert!(shared.is_dir());
        configure(&ctx, &options(false, &moved)).unwrap();
        remove_skill_link(&link).unwrap();
        create_skill_link(home.path(), &link, &crate::runner::RealRunner).unwrap();
        let notes = unconfigure(&ctx).unwrap();
        assert!(notes.iter().any(|n| n.contains("left alone")), "{notes:?}");
        assert!(link.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_is_refused_without_touching_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.json");
        let link = dir.path().join("settings.json");
        std::fs::write(&target, "{\"user\":true}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read(&link).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"user\":true}");
    }

    #[test]
    fn configure_edits_herdrs_config_checks_the_key_and_unconfigure_restores_it() {
        use crate::runner::fake::{FakeRunner, fail, ok};
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let config = home.path().join("herdr.toml");
        let original = "# my theme\n[theme]\nname = \"catppuccin\"\n";
        std::fs::write(&config, original).unwrap();
        let runner = FakeRunner::new();
        runner.on(
            "--default-config",
            ok("[keys]\n# previous_tab = \"prefix+p\"\n"),
        );
        runner.on("config check", ok(""));
        let ctx = Ctx {
            env: &env,
            root: home.path().join("root"),
            config_dir: home.path().join("cfg"),
            runner: &runner,
            detached_ticker: false,
        };
        let options = |key: Option<&str>| ConfigureOptions {
            clients: vec!["claude".into()],
            claude_home: Some(home.path().join("claude")),
            codex_home: None,
            dry_run: false,
            hooks: true,
            sidebar: true,
            key: key.map(str::to_string),
            herdr_config: Some(config.clone()),
            skill: None,
        };
        std::fs::create_dir_all(home.path().join("claude")).unwrap();

        // A key Herdr already uses is refused before anything is written.
        assert!(
            configure(&ctx, &options(Some("prefix+p")))
                .unwrap_err()
                .to_string()
                .contains("previous_tab")
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);

        configure(&ctx, &options(None)).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert!(
            text.contains("# my theme") && text.contains("prefix+a") && text.contains("$hp_sub")
        );
        assert!(
            parsed["ui"]["tab_bar_right"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["command"].as_str().is_some_and(is_tab_command))
        );
        assert_eq!(runner.count("config check"), 1);
        // A second run keeps the configured key.
        configure(&ctx, &options(None)).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);

        unconfigure(&ctx).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);

        // Herdr rejecting the candidate changes nothing.
        let rejecting = FakeRunner::new();
        rejecting.on("--default-config", ok(""));
        rejecting.on("config check", fail(1, "bad row"));
        let ctx = Ctx {
            runner: &rejecting,
            ..ctx
        };
        assert!(configure(&ctx, &options(None)).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    }
}

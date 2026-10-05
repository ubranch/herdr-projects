//! Built-in progress self-reporting: the agent runs `report` in its own pane,
//! the harness hooks (`hook`) inject the instructions and a reminder, one JSON
//! record per pane under `<root>/.progress/` feeds the ticker. The binding is
//! the pane id Herdr hands every pane shell; there is nothing to mint and no
//! daemon. Outside a Herdr pane both commands do nothing.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::herdr::{CALL_TIMEOUT, Herdr};
use crate::paths::{self, Ctx, Env};
use crate::runner::Runner;

pub const ACTIVITY_COLUMNS: usize = 40;
pub const ACTIVITY_TTL_MS: u64 = 300_000;
/// Reminders go out at most about once a minute.
pub const REMIND_SECS: i64 = 60;
pub const WAITING: &str = "Waiting for you";
pub const DONE: &str = "Done";

/// One pane's self-report.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Record {
    pub socket: String,
    pub pane_id: String,
    /// Pane ids restart from `w1` after a server restart; the terminal id tells
    /// a stale record from a live pane that reused the id.
    pub terminal_id: String,
    pub agent: String,
    pub activity: String,
    pub percent: Option<u8>,
    /// Unix seconds of the last `report`; 0 when none since the session started.
    pub reported_at: i64,
    /// Unix seconds of the last reminder the hook injected.
    pub reminded_at: i64,
    pub session_started_at: i64,
    /// The harness's own session id from the last SessionStart: Copilot CLI
    /// runs a subagent's hooks under another id with no other marker.
    pub session_id: String,
}

impl Record {
    pub fn waiting(&self) -> bool {
        self.reported_at > 0 && self.activity == WAITING
    }

    pub fn done(&self) -> bool {
        self.reported_at > 0 && self.percent == Some(100)
    }

    pub fn reported(&self) -> bool {
        self.reported_at > 0
    }
}

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// A report recent enough for its activity to show in the sidebar: silence
/// clears it after `ACTIVITY_TTL_MS`.
pub fn fresh(record: &Record) -> bool {
    record.reported() && now() - record.reported_at < (ACTIVITY_TTL_MS / 1000) as i64
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(".progress")
}

/// `<pane id>-<short hash of the socket path>.json`: pane ids repeat across sessions.
pub fn path(root: &Path, socket: &str, pane_id: &str) -> PathBuf {
    let mut hash = Sha256::new();
    #[cfg(not(windows))]
    hash.update(socket.as_bytes());
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut separator = false;
        // Match Path identity without changing calls or existing native backslash hashes.
        for component in paths::socket_ref(socket).components() {
            match component {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(drive) => hash.update([drive, b':']),
                    Prefix::VerbatimDisk(drive) => {
                        hash.update(b"\\\\?\\");
                        hash.update([drive, b':']);
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        hash.update(if prefix.kind().is_verbatim() {
                            &b"\\\\?\\UNC\\"[..]
                        } else {
                            &b"\\\\"[..]
                        });
                        hash.update(server.as_encoded_bytes());
                        hash.update(b"\\");
                        hash.update(share.as_encoded_bytes());
                    }
                    Prefix::DeviceNS(name) | Prefix::Verbatim(name) => {
                        hash.update(if prefix.kind().is_verbatim() {
                            &b"\\\\?\\"[..]
                        } else {
                            &b"\\\\.\\"[..]
                        });
                        hash.update(name.as_encoded_bytes());
                    }
                },
                Component::RootDir => {
                    hash.update(b"\\");
                    separator = false;
                }
                Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                    if separator {
                        hash.update(b"\\");
                    }
                    hash.update(component.as_os_str().as_encoded_bytes());
                    separator = true;
                }
            }
        }
    }
    hashed_path(root, pane_id, &hash.finalize())
}

fn hashed_path(root: &Path, pane_id: &str, hash: &[u8]) -> PathBuf {
    dir(root).join(format!(
        "{}-{:02x}{:02x}{:02x}{:02x}.json",
        pane_id.replace(':', "_"),
        hash[0],
        hash[1],
        hash[2],
        hash[3]
    ))
}

pub fn load(root: &Path, socket: &str, pane_id: &str) -> Option<Record> {
    crate::project::read_json::<Record>(&path(root, socket, pane_id)).filter(|record| {
        record.pane_id == pane_id && paths::socket_ref(&record.socket) == paths::socket_ref(socket)
    })
}

pub fn save(root: &Path, record: &Record) -> Result<()> {
    std::fs::create_dir_all(dir(root))?;
    #[cfg(windows)]
    let _lock = lock(root)?;
    crate::project::write_json(&path(root, &record.socket, &record.pane_id), record)
}

/// Upgrade each owned legacy filename once, before any command reads progress.
/// Unix socket strings and their existing filenames have not changed.
#[cfg(windows)]
pub(crate) fn migrate(root: &Path) -> Result<()> {
    let progress = dir(root);
    if !plain_metadata(&progress).is_some_and(|meta| meta.is_dir()) {
        return Ok(());
    }
    if migration_done(&progress)? {
        return Ok(());
    }
    if !plain_metadata(root).is_some_and(|meta| meta.is_dir())
        || !paths::within_dir(&progress, root)
    {
        return Ok(());
    }
    let _lock = lock(root)?;
    if !plain_metadata(&progress).is_some_and(|meta| meta.is_dir()) {
        return Ok(());
    }
    if migration_done(&progress)? {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(&progress) else {
        return Ok(());
    };
    let mut legacy = Vec::new();
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let source = entry.path();
        match owned_record(root, &source) {
            Ok(Some((record, destination))) if source != destination => {
                legacy.push((source, destination, record));
            }
            Err(_) => complete = false,
            _ => {}
        }
    }
    // If only aliases exist, the newest complete record becomes canonical.
    // Existing canonical terminal/session ownership always takes precedence.
    legacy.sort_by(|left, right| {
        revision(&right.2)
            .cmp(&revision(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    for (source, destination, record) in legacy {
        match std::fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(&source, &destination)?;
            }
            Ok(_) => {
                let Ok(Some((canonical, _))) = owned_record(root, &destination) else {
                    complete = false;
                    continue;
                };
                if paths::socket_ref(&canonical.socket) != paths::socket_ref(&record.socket)
                    || canonical.pane_id != record.pane_id
                {
                    complete = false;
                    continue;
                }
                if canonical.terminal_id != record.terminal_id
                    || canonical.session_id != record.session_id
                {
                    continue;
                }
                if revision(&record) > revision(&canonical) {
                    crate::project::write_json(&destination, &record)?;
                }
                std::fs::remove_file(&source)?;
            }
            Err(_) => complete = false,
        }
    }
    if complete {
        crate::project::write_atomic(&progress.join(MIGRATION_MARKER), MIGRATION_DONE)?;
    }
    Ok(())
}

#[cfg(windows)]
const MIGRATION_MARKER: &str = ".canonical-sockets-v1";
#[cfg(windows)]
const MIGRATION_DONE: &[u8] = b"herdr-projects progress canonical sockets 1\n";

#[cfg(windows)]
fn migration_done(progress: &Path) -> Result<bool> {
    let marker = progress.join(MIGRATION_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(_) => {
            if plain_metadata(&marker)
                .is_some_and(|meta| meta.is_file() && meta.len() == MIGRATION_DONE.len() as u64)
                && std::fs::read(&marker)? == MIGRATION_DONE
            {
                Ok(true)
            } else {
                bail!(
                    "refusing foreign progress migration marker {}",
                    marker.display()
                );
            }
        }
    }
}

#[cfg(windows)]
fn revision(record: &Record) -> (i64, i64, i64) {
    (
        record.session_started_at,
        record.reported_at,
        record.reminded_at,
    )
}

#[cfg(windows)]
fn plain_metadata(path: &Path) -> Option<std::fs::Metadata> {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_attributes() & 0x400 == 0)
}

#[cfg(windows)]
fn lock(root: &Path) -> Result<std::fs::File> {
    let token = root.join(".progress.lock");
    match std::fs::symlink_metadata(&token) {
        Ok(_) if !plain_metadata(&token).is_some_and(|meta| meta.is_file() && meta.len() == 0) => {
            bail!("refusing foreign progress lock {}", token.display());
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(token)?;
    file.lock()?;
    Ok(file)
}

#[cfg(windows)]
fn owned_record(root: &Path, file: &Path) -> std::io::Result<Option<(Record, PathBuf)>> {
    use std::os::windows::fs::MetadataExt;
    let progress = dir(root);
    let named = file
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once('-'))
        .is_some_and(|(pane, hash)| {
            !pane.is_empty() && hash.len() == 8 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    if !named
        || file.extension() != Some(std::ffi::OsStr::new("json"))
        || file.parent() != Some(progress.as_path())
    {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(file)?;
    if !metadata.is_file()
        || metadata.file_attributes() & 0x400 != 0
        || !paths::within_dir(file, &progress)
    {
        return Ok(None);
    }
    let contents = std::fs::read(file)?;
    let Ok(record) = serde_json::from_slice::<Record>(&contents) else {
        return Ok(None);
    };
    if record.socket.is_empty()
        || record.pane_id.is_empty()
        || record.percent.is_some_and(|percent| percent > 100)
    {
        return Ok(None);
    }
    let destination = path(root, &record.socket, &record.pane_id);
    if destination.parent() != Some(progress.as_path()) {
        return Ok(None);
    }
    if file != destination
        && file
            != hashed_path(
                root,
                &record.pane_id,
                &Sha256::digest(record.socket.as_bytes()),
            )
    {
        return Ok(None);
    }
    Ok(Some((record, destination)))
}

/// At most 40 columns, no control or bidi characters, trimmed. 100% is "Done".
pub fn clean(input: &str, columns: usize) -> String {
    let mut width = 0;
    input
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(120)
        .take_while(|c| {
            // East Asian wide characters take two columns; everything else one.
            width += if ('\u{1100}'..='\u{115f}').contains(c)
                || ('\u{2e80}'..='\u{a4cf}').contains(c)
                || ('\u{ac00}'..='\u{d7a3}').contains(c)
                || ('\u{f900}'..='\u{faff}').contains(c)
                || ('\u{fe30}'..='\u{fe4f}').contains(c)
                || ('\u{ff00}'..='\u{ff60}').contains(c)
                || ('\u{ffe0}'..='\u{ffe6}').contains(c)
                || ('\u{1f300}'..='\u{1f64f}').contains(c)
                || ('\u{1f900}'..='\u{1f9ff}').contains(c)
                || ('\u{20000}'..='\u{3fffd}').contains(c)
            {
                2
            } else {
                1
            };
            width <= columns
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The calling pane, when this process runs inside a Herdr pane: the id
/// Herdr shows for it (a moved pane keeps its launch-time `HERDR_PANE_ID`),
/// its terminal id and the agent Herdr detects. `None` outside Herdr.
pub struct Current {
    pub socket: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub agent: String,
}

pub fn current(env: &Env, runner: &dyn Runner) -> Option<Current> {
    current_within(env, runner, CALL_TIMEOUT)
}

/// The hook uses a short timeout: a slow server must not stall every tool call.
pub fn current_within(
    env: &Env,
    runner: &dyn Runner,
    timeout: std::time::Duration,
) -> Option<Current> {
    if env.var("HERDR_ENV") != Some("1") {
        return None;
    }
    env.var("HERDR_PANE_ID")?;
    let socket = env.var("HERDR_SOCKET_PATH")?.to_string();
    let herdr = Herdr::new(env.herdr_bin(), &socket, runner);
    let result = herdr
        .call(&["pane", "current", "--current"], timeout)
        .ok()?;
    let pane = &result["pane"];
    let pane_id = pane["pane_id"].as_str()?.to_string();
    Some(Current {
        socket,
        pane_id,
        terminal_id: pane["terminal_id"].as_str().unwrap_or("").to_string(),
        agent: pane["agent"].as_str().unwrap_or("").to_string(),
    })
}

/// `report --percent N|--unknown --activity "..."`, run by the agent in its
/// pane. Writes the record; the ticker shows its activity on the pane's sub-line
/// while it is [`fresh`].
pub fn report(ctx: &Ctx, percent: Option<u8>, activity: &str) -> Result<()> {
    let Some(pane) = current(ctx.env, ctx.runner) else {
        println!("not inside a Herdr pane; nothing reported");
        return Ok(());
    };
    if percent.is_some_and(|p| p > 100) {
        bail!("--percent must be 0 to 100");
    }
    let activity = if percent == Some(100) {
        DONE.to_string()
    } else {
        clean(activity, ACTIVITY_COLUMNS)
    };
    if activity.is_empty() {
        bail!("--activity is empty");
    }
    let mut record = load(&ctx.root, &pane.socket, &pane.pane_id).unwrap_or_default();
    record.socket = pane.socket.clone();
    record.pane_id = pane.pane_id.clone();
    record.terminal_id = pane.terminal_id.clone();
    record.agent = pane.agent.clone();
    record.activity = activity.clone();
    record.percent = percent;
    record.reported_at = now();
    save(&ctx.root, &record)?;
    println!(
        "recorded: {}{activity}",
        percent.map(|p| format!("{p}% · ")).unwrap_or_default()
    );
    Ok(())
}

/// `progress [--pane ID]`: the record for the calling pane, or the given one.
pub fn print(ctx: &Ctx, pane: Option<&str>) -> Result<()> {
    let (socket, pane_id) = match pane {
        Some(id) => (
            ctx.env.var("HERDR_SOCKET_PATH").unwrap_or("").to_string(),
            id.to_string(),
        ),
        None => match current(ctx.env, ctx.runner) {
            Some(c) => (c.socket, c.pane_id),
            None => bail!("not inside a Herdr pane; pass --pane ID"),
        },
    };
    match load(&ctx.root, &socket, &pane_id) {
        Some(record) => println!("{}", serde_json::to_string_pretty(&record)?),
        None => println!("null"),
    }
    Ok(())
}

// ---------------------------------------------------------------- hooks

/// The instructions the SessionStart hook injects, with the exact command.
pub fn instructions(prefix: &str, pane_id: &str) -> String {
    format!(
        "# Progress (herdr-projects)\n\n{}",
        guidance(prefix, Some(pane_id))
    )
}

/// The body of the progress instructions. Hooks inject it at session start;
/// every other agent reads it in its thread brief or the coordinator skill,
/// so any harness that can run a shell command reports.
pub fn guidance(prefix: &str, pane_id: Option<&str>) -> String {
    let pane = pane_id
        .map(|id| format!("this pane ({id})"))
        .unwrap_or_else(|| "this pane".into());
    format!(
        "Report the progress of the user's whole current task through `{prefix} report`. This is your estimate, not a timer or a count of tools. Reporting failures must never stop the actual work: give one short diagnostic and continue, without retries.\n\n\
         Report a rough percentage in five-point increments and a two-to-four-word activity, such as `Reading code`, `Testing changes` or `Waiting for you`:\n\n\
         `{prefix} report --percent 25 --activity 'Reading code'`\n\
         `{prefix} report --unknown --activity 'Assessing task'`\n\n\
         Report at the start of new work, after meaningful milestones, when the activity changes, at blockers, and before every substantive reply. Report `--activity 'Waiting for you'` whenever you stop to ask the user something. During active work aim for one report per minute at a natural tool boundary; never invent progress to satisfy a reminder. Revise the estimate downward when you discover more work; use `--unknown` while the scope is unclear.\n\n\
         Use `--percent 100` only when the entire requested outcome and its checks are finished; it displays `Done`. If more work is requested afterwards, report a fresh, lower percentage.\n\n\
         Only the top-level agent in {pane} reports; subagents and helpers do not. Await each report command; do not run it in the background."
    )
}

pub fn reminder(prefix: &str) -> String {
    format!(
        "Progress check-in is due if this is a natural boundary: `{prefix} report --percent N --activity '...'`. Reassess the current task; do not invent progress. Report `Waiting for you` before a question to the user."
    )
}

/// Events the reporter reacts to: the top-level agent's own SessionStart,
/// UserPromptSubmit and PostToolUse, never a subagent's, and never the
/// PostToolUse of the `report` call itself.
pub fn eligible(event: &serde_json::Value) -> bool {
    if ["agent_id", "subagent_id", "agent_transcript_path"]
        .iter()
        .any(|key| event.get(key).is_some_and(|v| !v.is_null()))
    {
        return false;
    }
    if event["transcript_path"]
        .as_str()
        .is_some_and(|p| p.split(['/', '\\']).any(|part| part == "subagents"))
    {
        return false;
    }
    let name = event["hook_event_name"].as_str().unwrap_or("");
    if !matches!(name, "SessionStart" | "PostToolUse" | "UserPromptSubmit") {
        return false;
    }
    if name == "PostToolUse"
        && event["tool_input"].to_string().contains("herdr-projects")
        && event["tool_input"].to_string().contains(" report ")
    {
        return false;
    }
    true
}

/// What the hook answers for one event: the text to inject, and whether the
/// record changed. Pure, so it is testable without a pane.
pub fn respond(record: &mut Record, kind: &str, prefix: &str, now: i64) -> Option<String> {
    match kind {
        "SessionStart" => {
            // A new session in this pane: the old report no longer describes it.
            let keep = (
                record.socket.clone(),
                record.pane_id.clone(),
                record.terminal_id.clone(),
                record.agent.clone(),
            );
            *record = Record {
                socket: keep.0,
                pane_id: keep.1,
                terminal_id: keep.2,
                agent: keep.3,
                session_started_at: now,
                reminded_at: now,
                ..Record::default()
            };
            Some(instructions(prefix, &record.pane_id))
        }
        "UserPromptSubmit" => {
            record.reminded_at = now;
            // The user answered: an old "Waiting for you" no longer holds.
            if record.activity == WAITING {
                record.activity.clear();
                record.reported_at = 0;
            }
            Some(format!(
                "Before task tools or a blocking question, check whether this request starts new work; if so report a fresh estimate. Then report before waiting for the user, for example `{prefix} report --unknown --activity 'Waiting for you'`."
            ))
        }
        "PostToolUse" => {
            let done = record.percent == Some(100);
            let due = now - record.reminded_at >= REMIND_SECS
                && !done
                && (record.reported_at == 0 || now - record.reported_at >= REMIND_SECS);
            if due {
                record.reminded_at = now;
                Some(reminder(prefix))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether an event belongs to another session than the one this pane
/// started: a subagent's, where the harness gives it an id of its own.
pub fn foreign_session(record: &Record, event: &serde_json::Value) -> bool {
    let id = event["session_id"].as_str().unwrap_or("");
    !record.session_id.is_empty() && !id.is_empty() && id != record.session_id
}

/// The event under its Claude Code name, which the rest of the reporter
/// uses: `hook_event_name` rewritten from the harness's own name.
pub fn normalize(
    harness: &crate::setup::Harness,
    mut event: serde_json::Value,
) -> serde_json::Value {
    let native = event["hook_event_name"].as_str().unwrap_or("");
    if let Some(i) = harness.events.iter().position(|e| *e == native) {
        event["hook_event_name"] = ["SessionStart", "UserPromptSubmit", "PostToolUse"][i].into();
    }
    event
}

/// The hook's answer in the shape the harness reads.
pub fn output(harness: &crate::setup::Harness, native: &str, text: &str) -> serde_json::Value {
    if harness.top_level_output {
        serde_json::json!({"additionalContext": text})
    } else {
        serde_json::json!({"hookSpecificOutput": {"hookEventName": native, "additionalContext": text}})
    }
}

/// `hook --agent <harness>`, the entry point the harness hooks run. Silent
/// (exit 0, no output) outside a Herdr pane, so the same hooks may sit in the
/// user's settings for every session on the machine.
pub fn hook(ctx: &Ctx, agent: &str) -> Result<()> {
    if ctx.env.var("HERDR_ENV") != Some("1") || ctx.env.var("HERDR_PANE_ID").is_none() {
        return Ok(());
    }
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin()
        .take(1_048_576)
        .read_to_string(&mut input)?;
    let Ok(event) = serde_json::from_str::<serde_json::Value>(&input) else {
        return Ok(());
    };
    let Some(harness) = crate::setup::harness(agent) else {
        return Ok(());
    };
    let native = event["hook_event_name"].as_str().unwrap_or("").to_string();
    let event = normalize(harness, event);
    if !eligible(&event) {
        return Ok(());
    }
    let kind = event["hook_event_name"].as_str().unwrap_or("").to_string();
    // Codex runs hooks for nested threads too; only the pane's own thread reports.
    if agent == "codex"
        && let Some(native) = event["session_id"].as_str()
        && ctx
            .env
            .var("CODEX_THREAD_ID")
            .is_some_and(|id| id != native)
    {
        return Ok(());
    }
    // At SessionStart Herdr may not have detected the agent yet: wait briefly.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let pane = loop {
        match current_within(ctx.env, ctx.runner, std::time::Duration::from_secs(2)) {
            Some(p) if kind != "SessionStart" || !p.agent.is_empty() => break p,
            Some(p) if std::time::Instant::now() >= deadline => break p,
            None if kind != "SessionStart" || std::time::Instant::now() >= deadline => {
                return Ok(());
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    if !pane.agent.is_empty() && pane.agent != agent {
        return Ok(()); // another harness's hook fired in a pane that is not its own
    }
    let mut record = load(&ctx.root, &pane.socket, &pane.pane_id).unwrap_or_default();
    if kind != "SessionStart" && foreign_session(&record, &event) {
        return Ok(());
    }
    record.socket = pane.socket.clone();
    record.pane_id = pane.pane_id.clone();
    record.terminal_id = pane.terminal_id.clone();
    record.agent = if pane.agent.is_empty() {
        agent.to_string()
    } else {
        pane.agent.clone()
    };
    let prefix = crate::coordinator::current_prefix(&ctx.root)?;
    let before = record.clone();
    let text = respond(&mut record, &kind, &prefix, now());
    if kind == "SessionStart" {
        record.session_id = event["session_id"].as_str().unwrap_or("").to_string();
    }
    if record != before {
        save(&ctx.root, &record)?;
    }
    if let Some(text) = text {
        println!("{}", output(harness, &native, &text));
    }
    Ok(())
}

/// The record's contribution to a thread's live state, or nothing when the
/// record is missing or describes an earlier pane with the same id.
pub fn self_report(root: &Path, socket: &str, pane_id: &str, terminal_id: &str) -> Option<Record> {
    let record = load(root, socket, pane_id)?;
    if !terminal_id.is_empty()
        && !record.terminal_id.is_empty()
        && record.terminal_id != terminal_id
    {
        return None;
    }
    record.reported().then_some(record)
}

/// Drops records whose pane is no longer listed in the session they belong to.
/// Only records of `socket` are judged: other sessions' records are theirs.
pub fn prune(root: &Path, socket: &str, live_pane_ids: &[String]) {
    #[cfg(windows)]
    if !plain_metadata(&dir(root)).is_some_and(|meta| meta.is_dir()) {
        return;
    }
    #[cfg(windows)]
    let Ok(_lock) = lock(root) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        #[cfg(windows)]
        let record = owned_record(root, &path)
            .ok()
            .flatten()
            .map(|(record, _)| record);
        #[cfg(not(windows))]
        let record = crate::project::read_json::<Record>(&path);
        let Some(record) = record else {
            continue;
        };
        if paths::socket_ref(&record.socket) == paths::socket_ref(socket)
            && !live_pane_ids.contains(&record.pane_id)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn initialize(root: &Path) -> Result<()> {
        let env = Env::for_test(root, &[]);
        let runner = crate::runner::fake::FakeRunner::new();
        Ctx {
            env: &env,
            root: root.into(),
            config_dir: root.join("cfg"),
            runner: &runner,
            detached_ticker: false,
        }
        .initialize()
        .map(|_| ())
    }

    #[cfg(windows)]
    fn persist_legacy(root: &Path, record: &Record) -> PathBuf {
        std::fs::create_dir_all(dir(root)).unwrap();
        let file = dir(root).join(format!(
            "{}-{}.json",
            record.pane_id.replace(':', "_"),
            &crate::thread::sha256_hex(record.socket.as_bytes())[..8]
        ));
        crate::project::write_json(&file, record).unwrap();
        file
    }

    #[test]
    fn context_initialization_does_not_create_root_or_progress_for_readers() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("absent");
        initialize(&root).unwrap();
        assert!(!root.exists());
        std::fs::create_dir(&root).unwrap();
        initialize(&root).unwrap();
        assert!(!dir(&root).exists());
        assert!(!root.join(".progress.lock").exists());
    }

    #[cfg(windows)]
    #[test]
    fn context_migrates_alias_only_progress_once_and_consumers_keep_identity() {
        let native = r"C:\config\herdr\sessions\one\herdr.sock";
        let mixed = r"C:/config/herdr\sessions\one\herdr.sock";
        for socket in [
            mixed,
            r"c:\config\herdr\sessions\one\herdr.sock",
            r"c:\config\\herdr\.\sessions\one\herdr.sock",
        ] {
            let root = tempfile::tempdir().unwrap();
            let report = Record {
                socket: socket.into(),
                pane_id: "w3:p1".into(),
                terminal_id: "terminal-one".into(),
                session_id: "harness-one".into(),
                activity: "Testing".into(),
                percent: Some(73),
                reported_at: 10,
                session_started_at: 1,
                ..Record::default()
            };
            let legacy = persist_legacy(root.path(), &report);
            let other = Record {
                socket: r"C:/config/herdr/sessions/two/herdr.sock".into(),
                terminal_id: "terminal-two".into(),
                session_id: "harness-two".into(),
                percent: Some(41),
                ..report.clone()
            };
            let other_legacy = persist_legacy(root.path(), &other);
            assert!(load(root.path(), native, &report.pane_id).is_none());
            initialize(root.path()).unwrap();
            for alias in [native, mixed, socket] {
                assert_eq!(
                    self_report(root.path(), alias, &report.pane_id, "terminal-one"),
                    Some(report.clone())
                );
                assert!(self_report(root.path(), alias, &report.pane_id, "terminal-two").is_none());
            }
            assert_eq!(
                self_report(root.path(), &other.socket, &other.pane_id, "terminal-two"),
                Some(other)
            );
            assert!(!legacy.exists());
            assert!(!other_legacy.exists());
            assert!(migration_done(&dir(root.path())).unwrap());

            // The cutover is persistent, not an alias fallback on every invocation.
            let late = Record {
                percent: Some(99),
                reported_at: 100,
                ..report.clone()
            };
            persist_legacy(root.path(), &late);
            initialize(root.path()).unwrap();
            assert_eq!(load(root.path(), native, &report.pane_id), Some(report));
            assert!(legacy.exists());
        }
    }

    #[cfg(windows)]
    #[test]
    fn migration_prefers_newest_same_identity_and_existing_current_terminal() {
        for case in 0..4 {
            let root = tempfile::tempdir().unwrap();
            let legacy = Record {
                socket: r"C:/config/herdr/sessions/one/herdr.sock".into(),
                pane_id: "w3:p1".into(),
                terminal_id: "terminal-one".into(),
                session_id: "harness-one".into(),
                percent: Some(73),
                reported_at: 10,
                ..Record::default()
            };
            let mut canonical = Record {
                socket: r"C:\config\herdr\sessions\one\herdr.sock".into(),
                percent: Some(41),
                reported_at: if case == 1 { 20 } else { 5 },
                ..legacy.clone()
            };
            if case == 2 {
                canonical.terminal_id = "current-terminal".into();
            }
            if case == 3 {
                canonical.session_id = "current-harness".into();
            }
            save(root.path(), &canonical).unwrap();
            let alias = persist_legacy(root.path(), &legacy);
            initialize(root.path()).unwrap();
            let expected = if case == 0 { &legacy } else { &canonical };
            for socket in [&legacy.socket, &canonical.socket] {
                assert_eq!(
                    self_report(
                        root.path(),
                        socket,
                        &expected.pane_id,
                        &expected.terminal_id
                    ),
                    Some(expected.clone())
                );
            }
            assert!(migration_done(&dir(root.path())).unwrap());
            if case >= 2 {
                // Different terminal/harness records are preserved, never merged.
                assert_eq!(crate::project::read_json::<Record>(&alias), Some(legacy));
            } else {
                assert!(!alias.exists());
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn migration_preserves_foreign_files_and_does_not_mark_blocked_owned_aliases_done() {
        let root = tempfile::tempdir().unwrap();
        let report = Record {
            socket: r"C:/config/herdr/sessions/one/herdr.sock".into(),
            pane_id: "w3:p1".into(),
            terminal_id: "terminal-one".into(),
            session_id: "harness-one".into(),
            percent: Some(73),
            reported_at: 10,
            ..Record::default()
        };
        let legacy = persist_legacy(root.path(), &report);
        let canonical = path(root.path(), &report.socket, &report.pane_id);
        std::fs::write(&canonical, b"foreign canonical").unwrap();
        let unrelated = dir(root.path()).join("unrelated.json");
        crate::project::write_json(&unrelated, &report).unwrap();
        let malformed = dir(root.path()).join("w9_p9-deadbeef.json");
        std::fs::write(&malformed, [0xff]).unwrap();
        let unsafe_pane = dir(root.path()).join("outside-deadbeef.json");
        let unsafe_report = Record {
            pane_id: "../outside".into(),
            ..report.clone()
        };
        crate::project::write_json(&unsafe_pane, &unsafe_report).unwrap();
        initialize(root.path()).unwrap();
        assert!(legacy.exists());
        assert!(!migration_done(&dir(root.path())).unwrap());
        assert_eq!(std::fs::read(&canonical).unwrap(), b"foreign canonical");
        assert_eq!(
            crate::project::read_json::<Record>(&unrelated),
            Some(report.clone())
        );
        assert_eq!(std::fs::read(&malformed).unwrap(), [0xff]);
        assert_eq!(
            crate::project::read_json::<Record>(&unsafe_pane),
            Some(unsafe_report)
        );

        std::fs::remove_file(&canonical).unwrap();
        initialize(root.path()).unwrap();
        assert_eq!(
            self_report(
                root.path(),
                &report.socket,
                &report.pane_id,
                &report.terminal_id
            ),
            Some(report)
        );
        assert!(!legacy.exists());
        assert!(migration_done(&dir(root.path())).unwrap());
        prune(root.path(), r"C:\config\herdr\sessions\one\herdr.sock", &[]);
        assert!(unrelated.exists());
        assert!(malformed.exists());
        assert!(unsafe_pane.exists());
    }

    #[cfg(windows)]
    #[test]
    fn context_does_not_trust_or_replace_a_foreign_migration_marker() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir(root.path())).unwrap();
        let marker = dir(root.path()).join(MIGRATION_MARKER);
        std::fs::write(&marker, b"foreign").unwrap();
        assert!(initialize(root.path()).is_err());
        assert_eq!(std::fs::read(marker).unwrap(), b"foreign");
    }

    #[cfg(windows)]
    #[test]
    fn progress_socket_aliases_load_native_records_without_merging_sessions() {
        let root = tempfile::tempdir().unwrap();
        let native = r"C:\config\herdr\sessions\one\herdr.sock";
        let mixed = r"C:/config/herdr\sessions\one\herdr.sock";
        let other = r"C:\config\herdr\sessions\two\herdr.sock";
        let pane = "w3:p1";
        let report = Record {
            socket: native.into(),
            pane_id: pane.into(),
            terminal_id: "terminal-one".into(),
            percent: Some(73),
            reported_at: 1,
            ..Record::default()
        };
        // Persist the pre-normalization native filename, not the current path helper.
        std::fs::create_dir_all(dir(root.path())).unwrap();
        let native_file = dir(root.path()).join(format!(
            "w3_p1-{}.json",
            &crate::thread::sha256_hex(native.as_bytes())[..8]
        ));
        crate::project::write_json(&native_file, &report).unwrap();
        initialize(root.path()).unwrap();
        for alias in [native, mixed, r"c:\config\\herdr\.\sessions\one\herdr.sock"] {
            assert_eq!(
                self_report(root.path(), alias, pane, "terminal-one"),
                Some(report.clone())
            );
            assert!(self_report(root.path(), alias, pane, "different-terminal").is_none());
        }
        assert!(self_report(root.path(), other, pane, "terminal-one").is_none());
        let legacy = dir(root.path()).join(format!(
            "w3_p1-{}.json",
            &crate::thread::sha256_hex(mixed.as_bytes())[..8]
        ));
        crate::project::write_json(
            &legacy,
            &Record {
                socket: mixed.into(),
                ..report.clone()
            },
        )
        .unwrap();
        let other_report = Record {
            socket: other.into(),
            percent: Some(41),
            ..report.clone()
        };
        save(root.path(), &other_report).unwrap();
        prune(root.path(), native, &[pane.into()]);
        assert_eq!(load(root.path(), mixed, pane), Some(report));
        assert!(legacy.exists());
        prune(root.path(), native, &[]);
        assert!(load(root.path(), mixed, pane).is_none());
        assert!(!legacy.exists());
        assert_eq!(load(root.path(), other, pane), Some(other_report));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_progress_keeps_distinct_opaque_socket_records() {
        let root = tempfile::tempdir().unwrap();
        let report = Record {
            socket: "/config/herdr.sock".into(),
            pane_id: "w3:p1".into(),
            percent: Some(73),
            reported_at: 1,
            ..Record::default()
        };
        save(root.path(), &report).unwrap();
        initialize(root.path()).unwrap();
        assert_eq!(
            load(root.path(), "/config/herdr.sock", "w3:p1"),
            Some(report)
        );
        for socket in [
            "/config/./herdr.sock",
            "/config//herdr.sock",
            r"/config\herdr.sock",
            "/config/HERDR.sock",
        ] {
            assert!(load(root.path(), socket, "w3:p1").is_none());
        }
    }

    #[test]
    fn activity_is_cleaned_and_capped_at_forty_columns() {
        assert_eq!(clean("  Reading code\n", 40), "Reading code");
        assert_eq!(clean(&"x".repeat(60), 40).len(), 40);
        assert_eq!(clean("a\u{202e}b\u{7}c", 40), "abc");
        assert_eq!(clean("日本語テキスト", 6), "日本語");
    }

    #[test]
    fn helpers_and_the_reporters_own_call_do_not_trigger() {
        assert!(!eligible(
            &json!({"hook_event_name":"PostToolUse","agent_id":"child"})
        ));
        assert!(!eligible(
            &json!({"hook_event_name":"PostToolUse","transcript_path":"/x/subagents/y.jsonl"})
        ));
        assert!(!eligible(
            &json!({"hook_event_name":"PostToolUse","transcript_path":r"C:\x\subagents\y.jsonl"})
        ));
        assert!(!eligible(
            &json!({"hook_event_name":"PostToolUse","tool_input":{"command":"/p/herdr-projects --root /r report --percent 5 --activity x"}})
        ));
        assert!(eligible(
            &json!({"hook_event_name":"PostToolUse","tool_input":{"command":"/p/herdr-projects --root /r context demo"}})
        ));
        assert!(eligible(
            &json!({"hook_event_name":"SessionStart","source":"compact"})
        ));
        assert!(!eligible(&json!({"hook_event_name":"Stop"})));
    }

    #[test]
    fn session_start_injects_instructions_and_resets_the_record() {
        let mut record = Record {
            pane_id: "w1:p1".into(),
            activity: "Old".into(),
            percent: Some(50),
            reported_at: 5,
            ..Record::default()
        };
        let text = respond(&mut record, "SessionStart", "/p/hp --root /r", 1000).unwrap();
        assert!(text.contains("/p/hp --root /r report --percent 25"));
        assert!(text.contains("(w1:p1)"));
        assert_eq!(record.activity, "");
        assert_eq!(record.percent, None);
        assert_eq!(record.reported_at, 0);
        assert_eq!(record.session_started_at, 1000);
    }

    #[test]
    fn reminders_are_throttled_to_once_a_minute_and_stop_at_done() {
        let mut record = Record {
            reminded_at: 1000,
            ..Record::default()
        };
        assert!(respond(&mut record, "PostToolUse", "hp", 1030).is_none());
        assert!(respond(&mut record, "PostToolUse", "hp", 1061).is_some());
        assert_eq!(record.reminded_at, 1061);
        // A fresh report also quiets the reminder for a minute.
        record.reported_at = 1100;
        assert!(respond(&mut record, "PostToolUse", "hp", 1130).is_none());
        assert!(respond(&mut record, "PostToolUse", "hp", 1200).is_some());
        record.percent = Some(100);
        assert!(respond(&mut record, "PostToolUse", "hp", 9000).is_none());
        assert!(
            respond(&mut record, "UserPromptSubmit", "hp", 9001)
                .unwrap()
                .contains("Waiting for you")
        );
        let mut asked = Record {
            activity: WAITING.into(),
            percent: Some(40),
            reported_at: 5,
            ..Record::default()
        };
        respond(&mut asked, "UserPromptSubmit", "hp", 10);
        assert!(!asked.waiting(), "an answer clears the old question");
        assert!(respond(&mut record, "Stop", "hp", 9002).is_none());
    }

    #[test]
    fn native_events_are_normalized_and_answered_in_each_harness_shape() {
        let gemini = crate::setup::harness("gemini").unwrap();
        let event = normalize(
            gemini,
            json!({"hook_event_name":"AfterTool","tool_input":{"command":"ls"}}),
        );
        assert_eq!(event["hook_event_name"], "PostToolUse");
        assert!(eligible(&event));
        assert_eq!(
            normalize(gemini, json!({"hook_event_name":"BeforeAgent"}))["hook_event_name"],
            "UserPromptSubmit"
        );
        assert_eq!(
            output(gemini, "AfterTool", "hi"),
            json!({"hookSpecificOutput":{"hookEventName":"AfterTool","additionalContext":"hi"}})
        );
        let copilot = crate::setup::harness("copilot").unwrap();
        assert_eq!(
            output(copilot, "SessionStart", "hi"),
            json!({"additionalContext":"hi"})
        );
    }

    #[test]
    fn events_from_another_session_in_the_pane_are_a_subagents() {
        let record = Record {
            session_id: "main".into(),
            ..Record::default()
        };
        assert!(foreign_session(&record, &json!({"session_id":"sub"})));
        assert!(!foreign_session(&record, &json!({"session_id":"main"})));
        assert!(!foreign_session(&record, &json!({})));
        assert!(
            !foreign_session(&Record::default(), &json!({"session_id":"sub"})),
            "no SessionStart seen: accept"
        );
    }

    #[test]
    fn guidance_names_the_command_and_the_pane_when_known() {
        assert!(guidance("/p/hp --root /r", None).contains("top-level agent in this pane reports"));
        assert!(guidance("/p/hp --root /r", Some("w2:p3")).contains("this pane (w2:p3)"));
        assert!(
            instructions("hp", "w1:p1")
                .starts_with("# Progress (herdr-projects)\n\nReport the progress")
        );
    }

    #[test]
    fn records_round_trip_per_session_and_stale_terminals_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let record = Record {
            socket: "/a.sock".into(),
            pane_id: "w1:p1".into(),
            terminal_id: "term_1".into(),
            activity: WAITING.into(),
            reported_at: 7,
            ..Record::default()
        };
        save(root.path(), &record).unwrap();
        let other = Record {
            socket: "/b.sock".into(),
            pane_id: "w1:p1".into(),
            terminal_id: "term_9".into(),
            activity: "Testing".into(),
            reported_at: 8,
            ..Record::default()
        };
        save(root.path(), &other).unwrap();
        assert_eq!(load(root.path(), "/a.sock", "w1:p1").unwrap(), record);
        assert!(
            self_report(root.path(), "/a.sock", "w1:p1", "term_1")
                .unwrap()
                .waiting()
        );
        assert!(self_report(root.path(), "/a.sock", "w1:p1", "term_2").is_none());
        assert!(self_report(root.path(), "/a.sock", "w1:p1", "").is_some());
        prune(root.path(), "/a.sock", &["w1:p2".into()]);
        assert!(load(root.path(), "/a.sock", "w1:p1").is_none());
        assert!(load(root.path(), "/b.sock", "w1:p1").is_some());
    }
}

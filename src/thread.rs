//! Thread records, ids, briefs, groups and the copy home.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::herdr::{Agent, Pane, ready_state};
use crate::project::{self, Project, slugify, write_atomic};
use crate::runner::Runner;

pub const STARTING_TIMEOUT_SECS: i64 = 300;
pub const BLOCKED_DEBOUNCE_SECS: i64 = 30;
pub const NOT_READY_SECS: i64 = 60;
pub const MEMORY_CAP_CHARS: usize = 32_000;
pub const LIBRARY_CAP_KB: u64 = 50 * 1024;
pub const MAX_LAUNCH_ATTEMPTS: u32 = 3;
const LOCAL_COPY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Starting,
    Open,
    Failed,
    Resolved,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Worktree,
    Tab,
    /// A tab in the project workspace whose working directory is the repo's
    /// main checkout, asked for explicitly with `thread start --kind checkout`.
    Checkout,
    Adopted,
}

impl Kind {
    pub fn parse(text: &str) -> Result<Kind> {
        match text {
            "worktree" => Ok(Kind::Worktree),
            "tab" => Ok(Kind::Tab),
            "checkout" => Ok(Kind::Checkout),
            other => bail!("`{other}` is not a thread kind (worktree, tab or checkout)"),
        }
    }
}

/// `threads/<id>.toml`. An empty string means "not set". Paths are stored as
/// they are on the thread's own machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub error: String,
    pub prompt_pending: bool,
    pub launch_attempts: u32,
    /// When the ticker last ran `agent start` for this thread.
    pub launched_at: String,
    /// Times the brief was typed or its line submitted with Enter. Above zero,
    /// a copy may sit in the input box, so the screen is read before any retry.
    pub brief_attempts: u32,
    /// A sender is at work on the brief since this time (a lease: a sender
    /// that died leaves it to expire).
    pub brief_claimed: String,
    /// The agent's state sequence and screen when first seen ready, and when:
    /// the brief waits until both stayed the same for a moment.
    pub brief_seen: String,
    pub brief_seen_at: String,
    /// The brief did not get through after its tries: the ticker stopped and
    /// an inbox item says so; `thread brief` still sends it.
    pub brief_stuck: bool,
    pub kind: Kind,
    pub repo: String,
    pub origin: String,
    pub branch: String,
    pub base: String,
    pub machine: String,
    pub worktree_path: String,
    pub thread_dir: String,
    pub workspace_id: String,
    /// The repository's primary Space herdr grouped this worktree under
    /// (made or reused by `worktree create`); closed once nothing uses it.
    pub repo_workspace: String,
    pub tab_id: String,
    pub pane_id: String,
    /// The Herdr agent kind: the profile's harness.
    pub agent: String,
    /// The profile the ticker launches the agent with, checked against the
    /// project's allow-list again at every launch. Empty on a thread started
    /// before profiles: it launches as the built-in `agent` plus `agent_args`.
    pub profile: String,
    /// Before profiles: a model flag for the agent CLI (checked again by the
    /// ticker). New threads leave it empty.
    pub agent_args: Vec<String>,
    /// A remote thread's profile as its own machine defines it: looked up
    /// there at start or restart (`profile resolve`), launched with
    /// `agent` plus these arguments. The name is still checked against this
    /// project's allow-list at every launch.
    pub remote_profile: bool,
    pub profile_args: Vec<String>,
    pub agent_name: String,
    pub cwd: String,
    pub created: String,
    pub updated: String,
    pub last_state: String,
    pub last_state_change: String,
    pub last_group: String,
    pub report_hash: String,
    pub last_report_change: String,
    pub last_review_item_hash: String,
    pub acked_report_hash: String,
    pub pr: String,
    pub pr_state: String,
    pub pr_review: String,
    pub resolved_reason: String,
    /// The sidebar's line 3 as the ticker last computed it (`needs you · ~55%`).
    pub state_line: String,
    /// Resolved with `--keep-worktree`: `sweep` leaves the worktree alone.
    pub kept_worktree: bool,
    /// The agent's own last activity and percent (local threads).
    pub activity: String,
    pub percent: Option<u8>,
}

impl Thread {
    pub fn is_remote(&self) -> bool {
        !self.machine.is_empty()
    }

    pub fn report_path(&self) -> String {
        thread_path(&self.thread_dir, &["report.md"])
    }

    pub fn library_path(&self) -> String {
        thread_path(&self.thread_dir, &["library"])
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    let digits = id.strip_prefix("t-").unwrap_or("");
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        bail!("`{id}` is not a thread id (expected the form t-0001)");
    }
    Ok(())
}

fn threads_dir(project: &Project) -> PathBuf {
    project.dir().join("threads")
}

pub fn record_path(project: &Project, id: &str) -> PathBuf {
    threads_dir(project).join(format!("{id}.toml"))
}

pub fn task_path(project: &Project, id: &str) -> PathBuf {
    threads_dir(project).join(format!("{id}.task.md"))
}

pub fn home_report_path(project: &Project, id: &str) -> PathBuf {
    threads_dir(project).join(format!("{id}.md"))
}

pub fn load(project: &Project, id: &str) -> Result<Thread> {
    validate_id(id)?;
    let path = record_path(project, id);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no thread `{id}` in `{}`", project.slug))?;
    toml::from_str(&text).with_context(|| format!("{} does not parse", path.display()))
}

pub fn list(project: &Project) -> Vec<Thread> {
    let Ok(entries) = std::fs::read_dir(threads_dir(project)) else {
        return Vec::new();
    };
    let mut threads: Vec<Thread> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".toml").map(str::to_string))
        .filter_map(|id| load(project, &id).ok())
        .collect();
    threads.sort_by(|a, b| a.id.cmp(&b.id));
    threads
}

fn write_record(project: &Project, thread: &Thread) -> Result<()> {
    write_atomic(
        &record_path(project, &thread.id),
        toml::to_string(thread)?.as_bytes(),
    )
}

/// Read-modify-write under the project lock: re-reads the record, lets `change`
/// touch only the fields its step owns, writes.
pub fn update(project: &Project, id: &str, change: impl FnOnce(&mut Thread)) -> Result<Thread> {
    let lock = project.lock()?;
    update_locked(project, id, &lock, change)
}

/// The caller already holds this project's lock across its cleanup effect.
pub(crate) fn update_locked(
    project: &Project,
    id: &str,
    lock: &project::ProjectLock,
    change: impl FnOnce(&mut Thread),
) -> Result<Thread> {
    ensure!(lock.guards(project), "the lock belongs to another project");
    let mut thread = load(project, id)?;
    change(&mut thread);
    thread.updated = project::now();
    write_record(project, &thread)?;
    Ok(thread)
}

/// Allocates the next id under the project lock and writes the first record.
pub fn allocate(project: &Project, fill: impl FnOnce(&mut Thread)) -> Result<Thread> {
    let _lock = project.lock()?;
    let next = list(project)
        .iter()
        .filter_map(|t| t.id.strip_prefix("t-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut thread = Thread {
        id: format!("t-{next:04}"),
        status: Status::Starting,
        created: project::now(),
        ..Thread::default()
    };
    fill(&mut thread);
    thread.updated = thread.created.clone();
    let path = record_path(project, &thread.id);
    if path.exists() {
        bail!("thread id {} is already taken", thread.id);
    }
    write_record(project, &thread)?;
    Ok(thread)
}

pub fn branch_name(slug: &str, id: &str, title: &str) -> String {
    let title = slugify(title);
    if title.is_empty() {
        format!("hp/{slug}/{id}")
    } else {
        format!("hp/{slug}/{id}-{title}")
    }
}

pub fn agent_name(slug: &str, id: &str) -> String {
    crate::names::thread(slug, id)
}

/// Appends a forwarded prompt to `threads/<id>.task.md` under `## Follow-ups`
/// with a timestamp, so a restarted thread re-reads it with its task.
pub fn append_follow_up(project: &Project, id: &str, text: &str) -> Result<()> {
    let _lock = project.lock()?;
    let path = task_path(project, id);
    let mut task = std::fs::read_to_string(&path).unwrap_or_default();
    if !task.ends_with('\n') && !task.is_empty() {
        task.push('\n');
    }
    if !task.lines().any(|l| l.trim() == "## Follow-ups") {
        task.push_str("\n## Follow-ups\n");
    }
    task.push_str(&format!("\n### {}\n\n{}\n", project::now(), text.trim()));
    write_atomic(&path, task.as_bytes())
}

/// The lines of a report's `## Next` section: one recommended action per
/// line, list markers removed, empty lines dropped.
pub fn next_lines(report: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in report.lines() {
        if line.starts_with("## ") {
            inside = line.trim() == "## Next";
            continue;
        }
        if !inside || line.starts_with('#') {
            if line.starts_with('#') {
                inside = false;
            }
            continue;
        }
        let text = line.trim();
        let text = text
            .strip_prefix("- ")
            .or_else(|| text.strip_prefix("* "))
            .or_else(|| {
                text.split_once(". ")
                    .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
                    .map(|(_, rest)| rest)
            })
            .unwrap_or(text)
            .trim();
        if !text.is_empty() {
            lines.push(text.to_string());
        }
    }
    lines
}

/// Lines the coordinator added to a thread's Next list (`threads/<id>.next.md`).
pub fn extra_next_path(project: &Project, id: &str) -> PathBuf {
    threads_dir(project).join(format!("{id}.next.md"))
}

/// The thread's Next list: the report's `## Next` lines, then the coordinator's.
pub fn all_next(project: &Project, id: &str) -> Vec<String> {
    let report = std::fs::read_to_string(home_report_path(project, id)).unwrap_or_default();
    let mut lines = next_lines(&report);
    let extra = std::fs::read_to_string(extra_next_path(project, id)).unwrap_or_default();
    lines.extend(
        extra
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| l.trim_start_matches("- ").to_string()),
    );
    lines
}

/// `<agent working directory>/.herdr-project/<slug>-<id>`, for every kind.
pub fn thread_dir(cwd: &str, slug: &str, id: &str) -> String {
    thread_path(
        cwd.trim_end_matches('/'),
        &[".herdr-project/", slug, "-", id],
    )
}

/// POSIX paths belong to remote machines even on a Windows host. Only a
/// native absolute Windows base uses backslashes, required by verbatim paths.
/// Leading `/` stays POSIX, including a remote `//` path that Windows would
/// otherwise interpret as UNC; canonical native UNC paths start with `\\`.
fn thread_path(base: &str, suffix: &[&str]) -> String {
    let native = cfg!(windows) && !base.starts_with('/') && Path::new(base).is_absolute();
    let base = if native {
        base.trim_end_matches(['/', '\\'])
    } else {
        base
    };
    let separator = if native { '\\' } else { '/' };
    let mut path =
        String::with_capacity(base.len() + 1 + suffix.iter().map(|part| part.len()).sum::<usize>());
    path.push_str(base);
    path.push(separator);
    for part in suffix {
        if native {
            for ch in part.chars() {
                path.push(if ch == '/' { separator } else { ch });
            }
        } else {
            path.push_str(part);
        }
    }
    path
}

/// The one line the agent is prompted with; the relative path is the same for
/// every kind. Nothing from outside is ever placed in a prompt.
pub fn launch_prompt(slug: &str, id: &str) -> String {
    format!("Read .herdr-project/{slug}-{id}/brief.md and do what it says.")
}

// ---------------------------------------------------------------- briefs

pub struct BriefInput<'a> {
    pub project_name: &'a str,
    pub slug: &'a str,
    pub goal: &'a str,
    pub repos: &'a [project::Repo],
    /// The project's `uploads/` folder on the home machine.
    pub uploads_path: &'a str,
    pub remote: bool,
    pub instructions: &'a str,
    pub memory_index: &'a str,
    /// (file name, contents), in the order they should be inlined.
    pub memory_files: &'a [(String, String)],
    pub task: &'a str,
    pub restart: bool,
    pub report_path: &'a str,
    pub library_path: &'a str,
    /// `<binary> --root <root>` for `report`; empty for a remote thread,
    /// whose machine has its own binary (or none).
    pub report_prefix: &'a str,
}

/// The header block every brief opens with: what the worker acts on, never
/// the coordinator's or the ticker's settings.
fn brief_header(input: &BriefInput) -> String {
    let mut out = String::from("# Project\n\n");
    out.push_str(&format!(
        "- Project: {} (`{}`)\n",
        input.project_name, input.slug
    ));
    out.push_str(&format!(
        "- Goal: {}\n",
        if input.goal.trim().is_empty() {
            "(none set)"
        } else {
            input.goal.trim()
        }
    ));
    if input.repos.is_empty() {
        out.push_str("- Repos: (none)\n");
    } else {
        out.push_str("- Repos:\n");
        for repo in input.repos {
            match &repo.machine {
                Some(machine) => {
                    out.push_str(&format!("  - {} on machine `{machine}`\n", repo.path))
                }
                None => out.push_str(&format!("  - {} (local)\n", repo.path)),
            }
        }
    }
    let uploads_note = if input.remote {
        " (on the home machine; not copied to yours)"
    } else {
        ""
    };
    out.push_str(&format!(
        "- Uploads, files from the user: `{}`{uploads_note}\n",
        input.uploads_path
    ));
    out.push_str(&format!(
        "- Library, files for the user: `{}`\n",
        input.library_path
    ));
    out.push_str(&format!("- Report: `{}`\n", input.report_path));
    out
}

pub fn compose_brief(input: &BriefInput) -> String {
    let mut brief = brief_header(input);
    brief.push('\n');
    brief.push_str(include_str!("../skill/THREAD.md").trim_end());
    brief.push_str("\n\n");
    if input.restart {
        brief.push_str(
            "**A previous attempt at this task exists on this branch.** Read its report at the report path below first, look at what is already on the branch, and continue from there.\n\n",
        );
    }
    brief.push_str("# Project instructions\n\n");
    brief.push_str(input.instructions.trim());
    brief.push_str("\n\n# Project memory\n\n");
    brief.push_str(input.memory_index.trim());
    brief.push('\n');

    let mut used = input.memory_index.chars().count();
    let mut left_out = Vec::new();
    for (name, text) in input.memory_files {
        let size = text.chars().count();
        if used + size <= MEMORY_CAP_CHARS {
            used += size;
            brief.push_str(&format!("\n## memory/{name}\n\n{}\n", text.trim()));
        } else {
            left_out.push(format!("memory/{name}"));
        }
    }
    if !left_out.is_empty() {
        brief.push_str(&format!(
            "\nNot inlined because project memory is over {MEMORY_CAP_CHARS} characters: {}.\n",
            left_out.join(", ")
        ));
    }

    brief.push_str("\n# Progress\n\n");
    if cfg!(windows) && !input.remote {
        brief.push_str("Run these local commands in PowerShell (`pwsh.exe`); keep the leading `&` when the command includes it.\n\n");
    }
    if input.report_prefix.is_empty() {
        brief.push_str("Report progress with `herdr-projects report --percent N --activity '...'` if that command exists on this machine (use `--activity 'Waiting for you'` before asking the user something, and `--percent 100` when done); otherwise skip it.\n");
    } else {
        brief.push_str(&crate::progress::guidance(input.report_prefix, None));
        brief.push('\n');
    }
    brief.push_str("\n# Task\n\n");
    brief.push_str(input.task.trim());
    brief.push_str(&format!(
        "\n\n# Paths\n\n- Report: `{}`\n- Library folder for files meant for the user: `{}`\n- Uploads from the user: `{}`\n",
        input.report_path, input.library_path, input.uploads_path
    ));
    brief
}

/// Reads the project's instructions and memory and composes the brief.
pub fn brief_for(project: &Project, thread: &Thread, task: &str, restart: bool) -> Result<String> {
    let (settings, instructions) = project.read_project_md()?;
    let project_name = project::display_name(&settings.name, &project.slug);
    let uploads = project.dir().join("uploads").to_string_lossy().into_owned();
    let prefix = if thread.is_remote() {
        String::new()
    } else {
        crate::coordinator::current_prefix(&project.root).unwrap_or_default()
    };
    let memory_index = std::fs::read_to_string(project.dir().join("MEMORY.md")).unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir(project.dir().join("memory"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let memory_files: Vec<(String, String)> = names
        .into_iter()
        .filter_map(|name| {
            let path = project.dir().join("memory").join(&name);
            // Regular files only: a symbolic link in memory/ is never followed.
            let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
            regular
                .then(|| std::fs::read_to_string(&path).ok())
                .flatten()
                .map(|text| (name, text))
        })
        .collect();
    Ok(compose_brief(&BriefInput {
        project_name: &project_name,
        slug: &project.slug,
        goal: &settings.goal,
        repos: &settings.repos,
        uploads_path: &uploads,
        remote: thread.is_remote(),
        instructions: &instructions,
        memory_index: &memory_index,
        memory_files: &memory_files,
        task,
        restart,
        report_path: &thread.report_path(),
        library_path: &thread.library_path(),
        report_prefix: &prefix,
    }))
}

// ---------------------------------------------------------------- groups

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    ReadyForReview,
    WaitingOnYou,
    Working,
    Landing,
    Idle,
    Resolved,
}

impl Group {
    /// Display order, shared by the sidebar `rank` token and the overview:
    /// separate from the precedence in `group()`.
    pub fn rank(self) -> u8 {
        match self {
            Group::WaitingOnYou => 1,
            Group::ReadyForReview => 2,
            Group::Landing => 3,
            Group::Working => 4,
            Group::Idle => 5,
            Group::Resolved => 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Group::ReadyForReview => "Ready for review",
            Group::WaitingOnYou => "Waiting on you",
            Group::Working => "Working",
            Group::Landing => "Landing",
            Group::Idle => "Idle",
            Group::Resolved => "Resolved",
        }
    }

    /// Lower-case hyphenated form, used in the `review` token and `last_group`.
    pub fn token(self) -> &'static str {
        match self {
            Group::ReadyForReview => "ready-for-review",
            Group::WaitingOnYou => "waiting-on-you",
            Group::Working => "working",
            Group::Landing => "landing",
            Group::Idle => "idle",
            Group::Resolved => "resolved",
        }
    }

    pub fn from_token(token: &str) -> Option<Group> {
        [
            Group::ReadyForReview,
            Group::WaitingOnYou,
            Group::Working,
            Group::Landing,
            Group::Idle,
            Group::Resolved,
        ]
        .into_iter()
        .find(|g| g.token() == token)
    }

    /// Needs-you first: the sidebar sort, the popup and the overview agree.
    pub const DISPLAY_ORDER: [Group; 6] = [
        Group::WaitingOnYou,
        Group::ReadyForReview,
        Group::Landing,
        Group::Working,
        Group::Idle,
        Group::Resolved,
    ];
}

/// What herdr shows for a thread's pane right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Live {
    pub pane_exists: bool,
    /// `None` when no agent is detected in the pane.
    pub agent_state: Option<String>,
    /// How long the agent has been in that state.
    pub state_secs: i64,
    /// The agent's own report (built-in progress), for local panes only.
    pub self_report: Option<crate::progress::Record>,
    /// Seconds since that report (0 without one).
    pub report_age_secs: i64,
}

impl Live {
    /// The agent said it is waiting for the user and has not started working since.
    pub fn self_waiting(&self) -> bool {
        self.self_report.as_ref().is_some_and(|r| r.waiting())
            && self.agent_state.as_deref() != Some("working")
    }

    /// The agent reported progress under 100% within the activity TTL.
    pub fn self_working(&self) -> bool {
        self.self_report
            .as_ref()
            .is_some_and(|r| !r.done() && !r.waiting())
            && self.report_age_secs < (crate::progress::ACTIVITY_TTL_MS / 1000) as i64
    }
}

pub fn seconds_since(timestamp: &str, now: jiff::Timestamp) -> i64 {
    timestamp
        .parse::<jiff::Timestamp>()
        .map(|then| now.as_second() - then.as_second())
        .unwrap_or(0)
}

/// The group of a thread. First matching row wins. One function, so the CLI
/// and the ticker always agree.
pub fn group(thread: &Thread, live: &Live, now: jiff::Timestamp) -> Group {
    let state = live.agent_state.as_deref();
    let has_report = !thread.report_hash.is_empty();
    // 1
    if thread.status == Status::Resolved {
        return Group::Resolved;
    }
    // 2
    if thread.status == Status::Starting {
        return if seconds_since(&thread.created, now) < STARTING_TIMEOUT_SECS {
            Group::Working
        } else {
            Group::WaitingOnYou
        };
    }
    // 3: a failed start, a dead pane, or a launch stuck on a dialog.
    let stuck_launch = thread.prompt_pending
        && (thread.brief_stuck
            || (state.is_some_and(|s| !ready_state(s)) && live.state_secs >= NOT_READY_SECS));
    // A pane closed after the thread wrote its report is finished work, not
    // a thread that needs the user.
    if thread.status == Status::Failed || (!live.pane_exists && !has_report) || stuck_launch {
        return Group::WaitingOnYou;
    }
    // 4: the harness shows a question or permission prompt, or the agent
    // said it is waiting for the user.
    let blocked_long = state == Some("blocked") && live.state_secs >= BLOCKED_DEBOUNCE_SECS;
    if blocked_long || live.self_waiting() {
        return Group::WaitingOnYou;
    }
    // 5: pull request and report facts. The harness showing `working` still
    // wins over an unread report: a report written mid-run is not a result.
    let pr_open = thread.pr_state.eq_ignore_ascii_case("open");
    if pr_open && thread.pr_review.eq_ignore_ascii_case("approved") {
        return Group::Landing;
    }
    let new_report = has_report && thread.report_hash != thread.acked_report_hash;
    if (new_report || (has_report && pr_open)) && !thread.prompt_pending && state != Some("working")
    {
        return Group::ReadyForReview;
    }
    // 6: working by the harness or by its own report.
    if matches!(state, Some("working") | Some("blocked"))
        || thread.prompt_pending
        || live.self_working()
    {
        return Group::Working;
    }
    // 7
    Group::Idle
}

/// A pane is the thread's pane only when its stable id and working directory
/// match the record. Native path aliases count; remote paths stay exact.
/// Ids are compared only among panes listed through the project's own socket.
pub fn pane_matches(thread: &Thread, pane: &Pane) -> bool {
    pane.pane_id == thread.pane_id && cwd_matches(thread, &pane.cwd)
}

/// A thread's agent: same pane id and working directory, and (for threads the
/// binary started) the same agent kind, and either our name or no name.
/// Herdr's native resume after a server restart starts the agent again in the
/// restored pane without a name; that is still ours and gets renamed. A pane
/// with our ids holding another kind, or another name, is someone else's.
pub fn agent_matches(thread: &Thread, agent: &Agent) -> bool {
    let ids = agent.pane_id == thread.pane_id && cwd_matches(thread, &agent.cwd);
    match thread.kind {
        // Not started by the binary: whatever herdr reported at adoption.
        Kind::Adopted => ids,
        _ => {
            ids && (thread.agent.is_empty()
                || agent.agent.is_empty()
                || agent.agent == thread.agent)
                && (agent.name.is_empty() || agent.name == thread.agent_name)
        }
    }
}

fn cwd_matches(thread: &Thread, cwd: &str) -> bool {
    if thread.is_remote() {
        thread.cwd == cwd
    } else {
        crate::paths::same_dir(Path::new(&thread.cwd), Path::new(cwd))
    }
}

/// Our agent, found by `agent_matches`, running without a name: re-apply it.
pub fn needs_rename(thread: &Thread, agent: &Agent) -> bool {
    thread.kind != Kind::Adopted
        && !thread.agent_name.is_empty()
        && agent.name.is_empty()
        && agent_matches(thread, agent)
}

/// Live state from one `agent list` and one `pane list`. `recorded` supplies
/// the duration: the ticker keeps `last_state_change` current; a CLI call uses
/// it when the live state equals the recorded one and zero otherwise.
pub fn live_state(thread: &Thread, agents: &[Agent], panes: &[Pane], now: jiff::Timestamp) -> Live {
    let agent = agents.iter().find(|a| agent_matches(thread, a));
    let pane_exists = agent.is_some() || panes.iter().any(|p| pane_matches(thread, p));
    // A pane whose ids match but which holds someone else's agent is not ours.
    let foreign = agent.is_none()
        && agents.iter().any(|a| a.pane_id == thread.pane_id)
        && thread.kind != Kind::Adopted;
    let agent_state = agent.map(|a| a.agent_status.clone());
    let state_secs = match &agent_state {
        Some(state) if *state == thread.last_state => seconds_since(&thread.last_state_change, now),
        _ => 0,
    };
    Live {
        pane_exists: pane_exists && !foreign,
        agent_state,
        state_secs,
        self_report: None,
        report_age_secs: 0,
    }
}

/// `live_state` plus the agent's own report from `<root>/.progress/`, matched
/// by pane id and terminal id. Remote threads have none (the record is written
/// on the machine where the agent runs).
pub fn live_with_report(
    thread: &Thread,
    agents: &[Agent],
    panes: &[Pane],
    now: jiff::Timestamp,
    root: &Path,
    socket: &str,
) -> Live {
    let mut live = live_state(thread, agents, panes, now);
    if thread.is_remote() || !live.pane_exists {
        return live;
    }
    let terminal = agents
        .iter()
        .find(|a| agent_matches(thread, a))
        .map(|a| a.terminal_id.clone())
        .or_else(|| {
            panes
                .iter()
                .find(|p| pane_matches(thread, p))
                .map(|p| p.terminal_id.clone())
        })
        .unwrap_or_default();
    if let Some(record) = crate::progress::self_report(root, socket, &thread.pane_id, &terminal) {
        live.report_age_secs = now.as_second() - record.reported_at;
        live.self_report = Some(record);
    }
    live
}

// ---------------------------------------------------------------- copy home

#[derive(Debug, Clone, PartialEq)]
pub enum CopyOutcome {
    Complete,
    /// The report was copied but something was skipped; each note says what.
    Partial(Vec<String>),
    Failed(String),
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn check_copy_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "the local copy exceeded its 60 second timeout"
    );
    Ok(())
}

fn single_link(_file: &std::fs::File, _metadata: &std::fs::Metadata) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // The handle stays open throughout validation and the subsequent read.
        if unsafe { GetFileInformationByHandle(_file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info.nNumberOfLinks == 1)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(_metadata.nlink() == 1)
    }
}

/// The caller has refused path symlinks. Validate regular-file type and link
/// count on the opened source, not on metadata for a replaceable pathname.
fn opened_private_file(path: &Path) -> std::io::Result<Option<(std::fs::File, std::fs::Metadata)>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || !single_link(&file, &metadata)? {
        return Ok(None);
    }
    Ok(Some((file, metadata)))
}

fn library_size(dir: &Path, deadline: Instant) -> Result<u64> {
    check_copy_deadline(deadline)?;
    let mut bytes = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        check_copy_deadline(deadline)?;
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let size = if meta.file_type().is_symlink() {
            0
        } else if meta.is_dir() {
            library_size(&path, deadline)?
        } else if meta.is_file() {
            opened_private_file(&path)?.map_or(0, |(_, metadata)| metadata.len())
        } else {
            0
        };
        bytes = bytes
            .checked_add(size)
            .context("the library size overflowed")?;
    }
    Ok(bytes)
}

struct CopyBudget {
    remaining: u64,
    deadline: Instant,
}

fn copy_contents(
    input: &mut std::fs::File,
    output: &mut std::fs::File,
    budget: &mut CopyBudget,
    buffer: &mut [u8],
) -> Result<bool> {
    loop {
        check_copy_deadline(budget.deadline)?;
        // Read at most one byte beyond the remaining cap to distinguish EOF.
        // That extra byte is never written, even when a source keeps growing.
        let limit = buffer.len().min((budget.remaining + 1) as usize);
        let read = input.read(&mut buffer[..limit])?;
        check_copy_deadline(budget.deadline)?;
        if read == 0 {
            return Ok(true);
        }
        if read as u64 > budget.remaining {
            return Ok(false);
        }
        output.write_all(&buffer[..read])?;
        budget.remaining -= read as u64;
    }
}

fn make_copy_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "{} is not a real directory; the library was not copied",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn copy_library_tree(
    source: &Path,
    target: &Path,
    budget: &mut CopyBudget,
    buffer: &mut [u8],
    notes: &mut Vec<String>,
) -> Result<bool> {
    check_copy_deadline(budget.deadline)?;
    ensure!(
        is_real_dir(source) && is_real_dir(target),
        "the library source or destination is no longer a real directory"
    );
    for entry in std::fs::read_dir(source)? {
        check_copy_deadline(budget.deadline)?;
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            notes.push(format!(
                "{} is a symbolic link; it was not copied",
                path.display()
            ));
            continue;
        }
        let dest = target.join(entry.file_name());
        if meta.is_dir() {
            make_copy_dir(&dest)?;
            if !copy_library_tree(&path, &dest, budget, buffer, notes)? {
                return Ok(false);
            }
        } else if meta.is_file() {
            let Some((mut input, metadata)) = opened_private_file(&path)? else {
                notes.push(format!(
                    "{} is not a singly linked regular file; it was not copied",
                    path.display()
                ));
                continue;
            };
            if let Ok(meta) = std::fs::symlink_metadata(&dest) {
                ensure!(
                    meta.is_file() && !meta.file_type().is_symlink(),
                    "{} is not a regular file; it was not overwritten",
                    dest.display()
                );
            }
            // Fresh staging prevents writing through a destination hard link.
            let tmp = target.join(format!(".herdr-projects-copy-{}.tmp", std::process::id()));
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            let copied = (|| -> Result<bool> {
                if !copy_contents(&mut input, &mut output, budget, buffer)? {
                    return Ok(false);
                }
                output.set_times(std::fs::FileTimes::new().set_modified(metadata.modified()?))?;
                // Managed Windows copies remain writable. Never clear an
                // existing destination's read-only bit through a hard link.
                #[cfg(unix)]
                output.set_permissions(metadata.permissions())?;
                drop(output);
                check_copy_deadline(budget.deadline)?;
                std::fs::rename(&tmp, &dest)?;
                Ok(true)
            })();
            if !copied.as_ref().is_ok_and(|complete| *complete) {
                let _ = std::fs::remove_file(&tmp);
            }
            if !copied? {
                notes.push(format!("the library grew beyond the {} MB cap during copying; {} and remaining files were not copied", LIBRARY_CAP_KB / 1024, path.display()));
                return Ok(false);
            }
        } else {
            notes.push(format!(
                "{} is not a regular file; it was not copied",
                path.display()
            ));
        }
    }
    Ok(true)
}

/// The hash of a local thread's single-link regular report inside a real
/// thread directory. Streaming keeps every-tick hashing memory bounded.
pub fn local_report_hash(thread: &Thread) -> Option<String> {
    let dir = Path::new(&thread.thread_dir);
    if thread.thread_dir.is_empty() || !is_real_dir(dir) {
        return None;
    }
    let report = dir.join("report.md");
    if !std::fs::symlink_metadata(&report).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    {
        return None;
    }
    let (mut file, _) = opened_private_file(&report).ok()??;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let deadline = Instant::now() + LOCAL_COPY_TIMEOUT;
    loop {
        check_copy_deadline(deadline).ok()?;
        let read = file.read(&mut buffer).ok()?;
        check_copy_deadline(deadline).ok()?;
        if read == 0 {
            return Some(format!("{:x}", hash.finalize()));
        }
        hash.update(&buffer[..read]);
    }
}

pub struct Copied {
    pub outcome: CopyOutcome,
    /// The report's hash, when a regular report file exists.
    pub report_hash: Option<String>,
}

fn read_private_report(path: &Path) -> Result<Option<Vec<u8>>> {
    let Some((mut file, _)) = opened_private_file(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let deadline = Instant::now() + LOCAL_COPY_TIMEOUT;
    loop {
        check_copy_deadline(deadline)?;
        let read = file.read(&mut buffer)?;
        check_copy_deadline(deadline)?;
        if read == 0 {
            return Ok(Some(bytes));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

/// Copies a local thread's report and, when `with_library`, its library home.
/// Symbolic links are not followed and multiply linked source files are not
/// copied. The caller must not hold the project lock: both copies acquire it.
pub fn copy_home_local(project: &Project, thread: &Thread, with_library: bool) -> Copied {
    let dir = Path::new(&thread.thread_dir);
    let mut notes = Vec::new();
    if thread.thread_dir.is_empty() || !dir.exists() {
        // Nothing was ever written, so nothing can be lost.
        return Copied {
            outcome: CopyOutcome::Complete,
            report_hash: None,
        };
    }
    if !is_real_dir(dir) {
        return Copied {
            outcome: CopyOutcome::Partial(vec![format!(
                "{} is a symbolic link; nothing was copied",
                dir.display()
            )]),
            report_hash: None,
        };
    }

    let report = dir.join("report.md");
    let mut report_hash = None;
    match std::fs::symlink_metadata(&report) {
        Err(_) => {}
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            match read_private_report(&report) {
                Ok(Some(bytes)) => {
                    let hash = sha256_hex(&bytes);
                    if hash != thread.report_hash
                        || !home_report_path(project, &thread.id).is_file()
                    {
                        let written = project.lock().and_then(|_lock| {
                            write_atomic(&home_report_path(project, &thread.id), &bytes)
                        });
                        if let Err(error) = written {
                            return Copied {
                                outcome: CopyOutcome::Failed(format!("{error:#}")),
                                report_hash: None,
                            };
                        }
                    }
                    report_hash = Some(hash);
                }
                Ok(None) => notes.push(format!(
                    "{} is not a singly linked regular file; it was not copied",
                    report.display()
                )),
                Err(error) => {
                    return Copied {
                        outcome: CopyOutcome::Failed(format!(
                            "could not read {}: {error}",
                            report.display()
                        )),
                        report_hash: None,
                    };
                }
            }
        }
        Ok(_) => notes.push(format!(
            "{} is not a regular file; it was not copied",
            report.display()
        )),
    }

    if with_library {
        let library = dir.join("library");
        if is_symlink(&library) {
            notes.push(format!(
                "{} is a symbolic link; the library was not copied",
                library.display()
            ));
        } else if is_real_dir(&library) {
            match copy_library_local(project, thread, &library) {
                Ok(mut skipped) => notes.append(&mut skipped),
                Err(error) => {
                    return Copied {
                        outcome: CopyOutcome::Failed(format!("{error:#}")),
                        report_hash,
                    };
                }
            }
        }
    }

    let outcome = if notes.is_empty() {
        CopyOutcome::Complete
    } else {
        CopyOutcome::Partial(notes)
    };
    Copied {
        outcome,
        report_hash,
    }
}

/// The same copy for a thread on a saved machine: the report with `scp`, the
/// library with rsync over ssh, after checking on the machine (without
/// following links) what is a real directory and a regular file.
pub fn copy_home_remote(
    project: &Project,
    thread: &Thread,
    with_library: bool,
    runner: &dyn Runner,
    target: &str,
) -> Copied {
    use crate::remote;
    let failed = |error: String| Copied {
        outcome: CopyOutcome::Failed(error),
        report_hash: None,
    };
    if thread.thread_dir.is_empty() {
        return Copied {
            outcome: CopyOutcome::Complete,
            report_hash: None,
        };
    }
    let found = match remote::layout(runner, target, &thread.thread_dir) {
        Ok(found) => found,
        Err(error) => return failed(format!("{error:#}")),
    };
    if found.absent {
        return Copied {
            outcome: CopyOutcome::Complete,
            report_hash: None,
        };
    }
    if !found.dir_ok {
        return Copied {
            outcome: CopyOutcome::Partial(vec![format!(
                "{} on {target} is a symbolic link; nothing was copied",
                thread.thread_dir
            )]),
            report_hash: None,
        };
    }
    let mut notes = Vec::new();
    let mut report_hash = None;
    if found.report_ok {
        let tmp = project.dir().join("threads").join(format!(
            ".{}.fetch.{}.tmp",
            thread.id,
            std::process::id()
        ));
        let fetched = remote::fetch_file(runner, target, &thread.report_path(), &tmp)
            .and_then(|()| Ok(std::fs::read(&tmp)?));
        let _ = std::fs::remove_file(&tmp);
        match fetched {
            Ok(bytes) => {
                let written = project
                    .lock()
                    .and_then(|_lock| write_atomic(&home_report_path(project, &thread.id), &bytes));
                if let Err(error) = written {
                    return failed(format!("{error:#}"));
                }
                report_hash = Some(sha256_hex(&bytes));
            }
            Err(error) => return failed(format!("{error:#}")),
        }
    } else if found.report_is_other {
        notes.push(format!(
            "{} on {target} is not a regular file; it was not copied",
            thread.report_path()
        ));
    }

    if with_library {
        if found.library_is_link {
            notes.push(format!(
                "{} on {target} is a symbolic link; the library was not copied",
                thread.library_path()
            ));
        } else if found.library_ok && found.library_kb > LIBRARY_CAP_KB {
            notes.push(format!(
                "the library is {} MB, over the {} MB cap; nothing from it was copied",
                found.library_kb / 1024,
                LIBRARY_CAP_KB / 1024
            ));
        } else if found.library_ok {
            notes.extend(
                found
                    .symlinks
                    .iter()
                    .map(|p| format!("{p} is a symbolic link; it was not copied")),
            );
            let target_dir = project.dir().join("library").join(&thread.id);
            let made = project.lock().and_then(|_lock| {
                if !target_dir.is_dir() {
                    std::fs::create_dir(&target_dir)?;
                }
                Ok(())
            });
            if let Err(error) = made {
                return Copied {
                    outcome: CopyOutcome::Failed(format!("{error:#}")),
                    report_hash,
                };
            }
            if let Err(error) =
                remote::fetch_dir(runner, target, &thread.library_path(), &target_dir)
            {
                return Copied {
                    outcome: CopyOutcome::Failed(format!("{error:#}")),
                    report_hash,
                };
            }
        }
    }
    let outcome = if notes.is_empty() {
        CopyOutcome::Complete
    } else {
        CopyOutcome::Partial(notes)
    };
    Copied {
        outcome,
        report_hash,
    }
}

fn copy_library_local(project: &Project, thread: &Thread, library: &Path) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let deadline = Instant::now() + LOCAL_COPY_TIMEOUT;
    let bytes = library_size(library, deadline)?;
    if bytes > LIBRARY_CAP_KB * 1024 {
        return Ok(vec![format!(
            "the library is {} MB, over the {} MB cap; nothing from it was copied",
            bytes / (1024 * 1024),
            LIBRARY_CAP_KB / 1024
        )]);
    }
    let _lock = project.lock()?;
    check_copy_deadline(deadline)?;
    let parent = project.dir().join("library");
    ensure!(
        is_real_dir(&parent) && !is_symlink(&parent),
        "{} is not a real directory; the library was not copied",
        parent.display()
    );
    let target = parent.join(&thread.id);
    // create_dir, not create_dir_all: never recreate a deleted project.
    make_copy_dir(&target)?;
    let source = crate::paths::canonicalize(library)?;
    let target = crate::paths::canonicalize(&target)?;
    ensure!(
        !source.starts_with(&target) && !target.starts_with(&source),
        "the library source and destination overlap; nothing was copied"
    );
    let mut budget = CopyBudget {
        remaining: LIBRARY_CAP_KB * 1024,
        deadline,
    };
    let mut buffer = [0_u8; 8192];
    copy_library_tree(&source, &target, &mut budget, &mut buffer, &mut notes)?;
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::runner::RealRunner;

    fn now() -> jiff::Timestamp {
        "2026-09-17T12:00:00Z".parse().unwrap()
    }

    fn ago(secs: i64) -> String {
        (now() - jiff::SignedDuration::from_secs(secs)).to_string()
    }

    fn open_thread() -> Thread {
        Thread {
            id: "t-0001".into(),
            status: Status::Open,
            created: ago(3600),
            ..Thread::default()
        }
    }

    fn live(state: Option<&str>, secs: i64) -> Live {
        Live {
            pane_exists: true,
            agent_state: state.map(str::to_string),
            state_secs: secs,
            ..Live::default()
        }
    }

    #[test]
    fn row1_resolved_wins_over_everything() {
        let t = Thread {
            status: Status::Resolved,
            prompt_pending: true,
            ..open_thread()
        };
        assert_eq!(
            group(&t, &live(Some("blocked"), 999), now()),
            Group::Resolved
        );
    }

    #[test]
    fn row2_starting_is_working_for_five_minutes() {
        let young = Thread {
            status: Status::Starting,
            created: ago(10),
            ..open_thread()
        };
        assert_eq!(group(&young, &Live::default(), now()), Group::Working);
        let old = Thread {
            status: Status::Starting,
            created: ago(301),
            ..open_thread()
        };
        assert_eq!(group(&old, &Live::default(), now()), Group::WaitingOnYou);
    }

    #[test]
    fn row3_waiting_on_you() {
        let failed = Thread {
            status: Status::Failed,
            ..open_thread()
        };
        assert_eq!(
            group(&failed, &live(Some("working"), 0), now()),
            Group::WaitingOnYou
        );

        let pending = Thread {
            prompt_pending: true,
            ..open_thread()
        };
        assert_eq!(
            group(&pending, &live(Some("blocked"), 60), now()),
            Group::WaitingOnYou
        );
        assert_eq!(
            group(&pending, &live(Some("unknown"), 60), now()),
            Group::WaitingOnYou
        );

        let gone = Live {
            pane_exists: false,
            agent_state: None,
            state_secs: 0,
            ..Live::default()
        };
        assert_eq!(group(&open_thread(), &gone, now()), Group::WaitingOnYou);

        assert_eq!(
            group(&open_thread(), &live(Some("blocked"), 30), now()),
            Group::WaitingOnYou
        );
    }

    #[test]
    fn row4_working_including_a_launch_in_progress() {
        assert_eq!(
            group(&open_thread(), &live(Some("working"), 0), now()),
            Group::Working
        );
        // A permission prompt answered quickly never shows as waiting.
        assert_eq!(
            group(&open_thread(), &live(Some("blocked"), 29), now()),
            Group::Working
        );
        // A new thread is Working, not Waiting on you, until an undetected-ready
        // agent has lasted 60 seconds.
        let pending = Thread {
            prompt_pending: true,
            ..open_thread()
        };
        assert_eq!(group(&pending, &live(None, 0), now()), Group::Working);
        assert_eq!(
            group(&pending, &live(Some("unknown"), 59), now()),
            Group::Working
        );
        assert_eq!(
            group(&pending, &live(Some("idle"), 500), now()),
            Group::Working
        );
    }

    #[test]
    fn row5_landing_needs_open_and_approved() {
        let t = Thread {
            report_hash: "h".into(),
            pr_state: "OPEN".into(),
            pr_review: "APPROVED".into(),
            ..open_thread()
        };
        assert_eq!(group(&t, &live(Some("idle"), 0), now()), Group::Landing);
        let t = Thread {
            pr_review: "CHANGES_REQUESTED".into(),
            ..t
        };
        assert_eq!(
            group(&t, &live(Some("idle"), 0), now()),
            Group::ReadyForReview
        );
    }

    #[test]
    fn row6_ready_for_review_until_ack_or_while_pr_open() {
        let t = Thread {
            report_hash: "h".into(),
            ..open_thread()
        };
        assert_eq!(
            group(&t, &live(Some("done"), 0), now()),
            Group::ReadyForReview
        );
        let acked = Thread {
            acked_report_hash: "h".into(),
            ..t.clone()
        };
        assert_eq!(group(&acked, &live(Some("done"), 0), now()), Group::Idle);
        let with_pr = Thread {
            pr_state: "OPEN".into(),
            ..acked
        };
        assert_eq!(
            group(&with_pr, &live(Some("done"), 0), now()),
            Group::ReadyForReview
        );
    }

    #[test]
    fn row7_idle_and_precedence() {
        assert_eq!(
            group(&open_thread(), &live(Some("idle"), 0), now()),
            Group::Idle
        );
        // Working (row 4) beats Ready for review (row 6).
        let t = Thread {
            report_hash: "h".into(),
            ..open_thread()
        };
        assert_eq!(group(&t, &live(Some("working"), 0), now()), Group::Working);
        // Blocked for long (row 3) beats an approved pull request (row 5).
        let t = Thread {
            pr_state: "OPEN".into(),
            pr_review: "APPROVED".into(),
            ..t
        };
        assert_eq!(
            group(&t, &live(Some("blocked"), 31), now()),
            Group::WaitingOnYou
        );
    }

    #[test]
    fn a_closed_pane_needs_you_only_without_a_report() {
        let gone = Live {
            pane_exists: false,
            ..Live::default()
        };
        let t = Thread {
            report_hash: "h".into(),
            ..open_thread()
        };
        assert_eq!(group(&t, &gone, now()), Group::ReadyForReview);
        let acked = Thread {
            acked_report_hash: "h".into(),
            ..t
        };
        assert_eq!(group(&acked, &gone, now()), Group::Idle);
        assert_eq!(group(&open_thread(), &gone, now()), Group::WaitingOnYou);
    }

    fn reported(activity: &str, percent: Option<u8>, age: i64, state: &str) -> Live {
        let record = crate::progress::Record {
            activity: activity.into(),
            percent,
            reported_at: 1,
            ..Default::default()
        };
        Live {
            report_age_secs: age,
            self_report: Some(record),
            ..live(Some(state), 0)
        }
    }

    #[test]
    fn self_reports_feed_the_group() {
        // Asked a question but the harness reads idle: needs you.
        assert_eq!(
            group(
                &open_thread(),
                &reported("Waiting for you", Some(40), 5, "idle"),
                now()
            ),
            Group::WaitingOnYou
        );
        // Waiting beats a new report, but not a harness that is working again.
        let t = Thread {
            report_hash: "h".into(),
            ..open_thread()
        };
        assert_eq!(
            group(&t, &reported("Waiting for you", None, 5, "idle"), now()),
            Group::WaitingOnYou
        );
        assert_eq!(
            group(&t, &reported("Waiting for you", None, 5, "working"), now()),
            Group::Working
        );
        // Under 100% and fresh: working, even between tool calls.
        assert_eq!(
            group(
                &open_thread(),
                &reported("Testing changes", Some(55), 30, "idle"),
                now()
            ),
            Group::Working
        );
        // Stale after five minutes, and 100% is done.
        assert_eq!(
            group(
                &open_thread(),
                &reported("Testing changes", Some(55), 400, "idle"),
                now()
            ),
            Group::Idle
        );
        assert_eq!(
            group(
                &open_thread(),
                &reported("Done", Some(100), 5, "idle"),
                now()
            ),
            Group::Idle
        );
        // A new report beats self-reported progress.
        assert_eq!(
            group(&t, &reported("Polishing", Some(90), 5, "idle"), now()),
            Group::ReadyForReview
        );
    }

    #[test]
    fn display_order_and_rank_digits() {
        let ranks: Vec<u8> = Group::DISPLAY_ORDER.iter().map(|g| g.rank()).collect();
        assert_eq!(ranks, [1, 2, 3, 4, 5, 6]);
        assert_eq!(Group::ReadyForReview.token(), "ready-for-review");
        assert_eq!(Group::WaitingOnYou.token(), "waiting-on-you");
        assert_eq!(Group::from_token("landing"), Some(Group::Landing));
    }

    fn agent(name: &str, cwd: &str) -> Agent {
        Agent {
            pane_id: "w2:p1".into(),
            tab_id: "w2:t1".into(),
            workspace_id: "w2".into(),
            name: name.into(),
            agent_status: "idle".into(),
            cwd: cwd.into(),
            ..Agent::default()
        }
    }

    fn placed_thread(kind: Kind) -> Thread {
        Thread {
            kind,
            pane_id: "w2:p1".into(),
            tab_id: "w2:t1".into(),
            workspace_id: "w2".into(),
            agent_name: "hp-demo-t-0001".into(),
            agent: "claude".into(),
            cwd: "/wt".into(),
            last_state: "idle".into(),
            last_state_change: ago(45),
            ..open_thread()
        }
    }

    #[test]
    fn identity_check_before_acting_on_a_pane() {
        let t = placed_thread(Kind::Worktree);
        assert!(agent_matches(&t, &agent("hp-demo-t-0001", "/wt")));
        assert!(!agent_matches(&t, &agent("hp-demo-t-0002", "/wt")));
        assert!(!agent_matches(&t, &agent("hp-demo-t-0001", "/other")));
        // Same ids but someone else's agent: treated as gone.
        let state = live_state(&t, &[agent("other", "/wt")], &[], now());
        assert!(!state.pane_exists);
        assert_eq!(state.agent_state, None);
        // Another kind in our pane is not ours either.
        let codex = Agent {
            agent: "codex".into(),
            ..agent("", "/wt")
        };
        assert!(!agent_matches(&t, &codex));
    }

    #[test]
    fn native_directory_spellings_keep_the_live_pane_but_never_a_reused_one() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("项目 worktree");
        let other = home.path().join("other directory");
        std::fs::create_dir_all(dir.join("child")).unwrap();
        std::fs::create_dir(&other).unwrap();
        let thread = Thread {
            cwd: format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR),
            ..placed_thread(Kind::Worktree)
        };
        let spellings = vec![dir.clone(), dir.join("child").join("..")];
        #[cfg(windows)]
        let spellings = {
            let mut spellings = spellings;
            spellings.push(PathBuf::from(dir.to_string_lossy().replace('\\', "/")));
            spellings.push(std::fs::canonicalize(&dir).unwrap());
            spellings
        };
        for cwd in spellings {
            let pane = Pane {
                pane_id: thread.pane_id.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
                ..Pane::default()
            };
            let ours = agent(&thread.agent_name, &pane.cwd);
            assert!(pane_matches(&thread, &pane));
            assert!(agent_matches(&thread, &ours));
            assert!(live_state(&thread, &[], std::slice::from_ref(&pane), now()).pane_exists);
            assert!(live_state(&thread, std::slice::from_ref(&ours), &[], now()).pane_exists);
            let reused = Pane {
                cwd: other.to_string_lossy().into_owned(),
                ..pane.clone()
            };
            assert!(!pane_matches(&thread, &reused));
            assert!(!agent_matches(
                &thread,
                &agent(&thread.agent_name, &reused.cwd)
            ));
            assert!(!live_state(&thread, &[], &[reused], now()).pane_exists);
            assert!(!pane_matches(
                &thread,
                &Pane {
                    pane_id: "w2:p9".into(),
                    ..pane
                }
            ));
            assert!(!agent_matches(
                &thread,
                &Agent {
                    name: "foreign".into(),
                    ..ours.clone()
                }
            ));
            assert!(!agent_matches(
                &thread,
                &Agent {
                    agent: "codex".into(),
                    ..ours
                }
            ));
        }
    }

    #[test]
    fn remote_directory_spellings_are_not_interpreted_as_native_paths() {
        let thread = Thread {
            machine: "box".into(),
            cwd: "/srv/worktree".into(),
            ..placed_thread(Kind::Worktree)
        };
        assert!(agent_matches(
            &thread,
            &agent(&thread.agent_name, "/srv/worktree")
        ));
        for cwd in ["/srv/worktree/", r"\srv\worktree", "/srv/worktree/child/.."] {
            let pane = Pane {
                pane_id: thread.pane_id.clone(),
                cwd: cwd.into(),
                ..Pane::default()
            };
            assert!(!pane_matches(&thread, &pane));
            assert!(!agent_matches(&thread, &agent(&thread.agent_name, cwd)));
            assert!(!live_state(&thread, &[], &[pane], now()).pane_exists);
        }
    }

    #[test]
    fn a_natively_resumed_unnamed_agent_is_ours_and_gets_renamed() {
        // After a server restart the pane id and cwd are the same, the tab may
        // have moved, and the resumed agent has no name.
        let t = placed_thread(Kind::Worktree);
        let resumed = Agent {
            tab_id: "w2:t9".into(),
            workspace_id: "w2".into(),
            agent: "claude".into(),
            ..agent("", "/wt")
        };
        assert!(agent_matches(&t, &resumed));
        assert!(needs_rename(&t, &resumed));
        assert!(live_state(&t, &[resumed], &[], now()).pane_exists);
        assert!(!needs_rename(&t, &agent("hp-demo-t-0001", "/wt")));
    }

    #[test]
    fn adopted_threads_match_without_the_name() {
        let t = Thread {
            agent_name: String::new(),
            ..placed_thread(Kind::Adopted)
        };
        assert!(agent_matches(&t, &agent("whatever", "/wt")));
        assert!(!agent_matches(&t, &agent("whatever", "/elsewhere")));
    }

    #[test]
    fn live_state_duration_comes_from_the_record_only_when_states_agree() {
        let t = placed_thread(Kind::Worktree);
        let same = live_state(&t, &[agent("hp-demo-t-0001", "/wt")], &[], now());
        assert_eq!(same.state_secs, 45);
        let mut other = agent("hp-demo-t-0001", "/wt");
        other.agent_status = "blocked".into();
        assert_eq!(live_state(&t, &[other], &[], now()).state_secs, 0);
    }

    #[test]
    fn ids_branches_and_dirs() {
        assert!(validate_id("t-0001").is_ok());
        assert!(validate_id("t-12345").is_ok());
        for bad in ["", "t-1", "t-00a1", "../t-0001", "x-0001"] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
        assert_eq!(
            branch_name("demo", "t-0001", "Fix the $(login) bug!"),
            "hp/demo/t-0001-fix-the-login-bug"
        );
        assert_eq!(branch_name("demo", "t-0002", "???"), "hp/demo/t-0002");
        assert_eq!(
            thread_dir("/wt/", "demo", "t-0001"),
            "/wt/.herdr-project/demo-t-0001"
        );
        let remote = Thread {
            machine: "box".into(),
            thread_dir: thread_dir("//srv/repo/", "demo", "t-0001"),
            ..Thread::default()
        };
        assert_eq!(
            remote.report_path(),
            "//srv/repo/.herdr-project/demo-t-0001/report.md"
        );
        assert_eq!(
            remote.library_path(),
            "//srv/repo/.herdr-project/demo-t-0001/library"
        );
        assert_eq!(
            launch_prompt("demo", "t-0001"),
            "Read .herdr-project/demo-t-0001/brief.md and do what it says."
        );
    }

    #[test]
    fn id_allocation_under_contention() {
        let root = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let project = project.clone();
                std::thread::spawn(move || allocate(&project, |_| {}).unwrap().id)
            })
            .collect();
        let mut ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8);
        assert_eq!(ids[0], "t-0001");
        assert_eq!(ids[7], "t-0008");
    }

    #[test]
    fn updates_are_atomic_and_keep_other_fields() {
        let root = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let t = allocate(&project, |t| t.title = "Hello".into()).unwrap();
        update(&project, &t.id, |t| t.pane_id = "w1:p2".into()).unwrap();
        update(&project, &t.id, |t| t.prompt_pending = true).unwrap();
        let t = load(&project, &t.id).unwrap();
        assert_eq!(
            (t.title.as_str(), t.pane_id.as_str(), t.prompt_pending),
            ("Hello", "w1:p2", true)
        );
        let leftovers = std::fs::read_dir(project.dir().join("threads"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn brief_order_and_memory_cap() {
        let files = vec![
            ("a.md".to_string(), "alpha fact".to_string()),
            ("b.md".to_string(), "x".repeat(MEMORY_CAP_CHARS)),
            ("c.md".to_string(), "gamma fact".to_string()),
        ];
        let repos = vec![
            project::Repo {
                path: "/srv/app".into(),
                machine: Some("box".into()),
            },
            project::Repo {
                path: "/home/me/lib".into(),
                machine: None,
            },
        ];
        let input = BriefInput {
            project_name: "Demo",
            slug: "demo",
            goal: "Ship it",
            repos: &repos,
            uploads_path: "/root/demo/uploads",
            remote: false,
            instructions: "Always run the tests.",
            memory_index: "# Memory\n- a\n- b\n- c",
            memory_files: &files,
            task: "Do the thing.",
            restart: true,
            report_path: "/wt/.herdr-project/demo-t-0001/report.md",
            library_path: "/wt/.herdr-project/demo-t-0001/library",
            report_prefix: "/bin/hp --root /r",
        };
        let brief = compose_brief(&input);
        let pos = |needle: &str| {
            brief
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        // The header comes first: name, goal, repos with machines, uploads, library, report.
        assert!(brief.starts_with("# Project\n\n- Project: Demo (`demo`)\n- Goal: Ship it\n"));
        assert!(pos("/srv/app on machine `box`") < pos("/home/me/lib (local)"));
        assert!(pos("Uploads, files from the user: `/root/demo/uploads`") < pos("# Thread brief"));
        assert!(pos("# Thread brief") < pos("previous attempt"));
        assert!(pos("previous attempt") < pos("Always run the tests."));
        assert!(pos("Always run the tests.") < pos("# Memory"));
        assert!(pos("# Memory") < pos("alpha fact"));
        assert!(pos("alpha fact") < pos("# Progress"));
        assert!(pos("/bin/hp --root /r report --percent 25") < pos("Do the thing."));
        assert!(pos("Do the thing.") < pos("# Paths"));
        assert!(brief.contains("gamma fact"));
        assert!(
            brief.contains(
                "Not inlined because project memory is over 32000 characters: memory/b.md."
            )
        );
        assert!(!brief.contains(&"x".repeat(100)));
        // No operational settings reach a thread.
        for word in [
            "max_parallel_threads",
            "auto_resolve_days",
            "nudge",
            "coordinator_agent",
            "thread_agent",
        ] {
            assert!(!brief.contains(word), "{word}");
        }

        let fresh = compose_brief(&BriefInput {
            goal: "",
            repos: &[],
            remote: true,
            instructions: "",
            memory_index: "",
            memory_files: &[],
            task: "t",
            restart: false,
            report_path: "r",
            library_path: "l",
            report_prefix: "",
            ..input
        });
        assert!(!fresh.contains("previous attempt"));
        assert!(fresh.contains("- Goal: (none set)\n- Repos: (none)\n"));
        assert!(fresh.contains("on the home machine; not copied"));
    }

    #[test]
    fn next_lines_come_from_the_next_section_only() {
        let report = "PR: https://github.com/o/r/pull/1\n## Report\n- not this\n## Next\n- Merge the PR\n* Fix CI\n3. Confirm assumption X\n\n## Remember\n- nor this\n";
        assert_eq!(
            next_lines(report),
            ["Merge the PR", "Fix CI", "Confirm assumption X"]
        );
        assert!(next_lines("## Report\nnothing\n").is_empty());
        assert!(next_lines("## Next\n").is_empty());
    }

    #[test]
    fn follow_ups_are_appended_to_the_task_file_with_a_timestamp() {
        let root = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let t = allocate(&project, |_| {}).unwrap();
        std::fs::write(task_path(&project, &t.id), "The task.").unwrap();
        append_follow_up(&project, &t.id, "Also do Y.\n").unwrap();
        append_follow_up(&project, &t.id, "And Z.").unwrap();
        let text = std::fs::read_to_string(task_path(&project, &t.id)).unwrap();
        assert!(
            text.starts_with("The task.\n\n## Follow-ups\n\n### 20"),
            "{text}"
        );
        assert_eq!(text.matches("## Follow-ups").count(), 1);
        assert_eq!(text.matches("\n### ").count(), 2);
        assert!(text.ends_with("And Z.\n"));

        // The coordinator's extra Next lines come after the report's.
        std::fs::write(
            home_report_path(&project, &t.id),
            "## Next\n- From the report\n",
        )
        .unwrap();
        std::fs::write(
            extra_next_path(&project, &t.id),
            "- Added by the coordinator\n",
        )
        .unwrap();
        assert_eq!(
            all_next(&project, &t.id),
            ["From the report", "Added by the coordinator"]
        );
    }

    fn local_thread(project: &Project, dir: &Path) -> Thread {
        let t = allocate(project, |t| {
            t.thread_dir = dir.to_string_lossy().into_owned()
        })
        .unwrap();
        std::fs::create_dir_all(dir.join("library")).unwrap();
        t
    }

    #[test]
    fn copies_report_and_library_and_skips_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "## Report\nok\n").unwrap();
        std::fs::write(dir.join("library/out.txt"), "data").unwrap();
        std::fs::create_dir(dir.join("library/it's a [folder]")).unwrap();
        std::fs::write(
            dir.join("library/it's a [folder]/$(data); file.txt"),
            "nested data",
        )
        .unwrap();

        let copied = copy_home_local(&project, &t, true);
        assert_eq!(copied.outcome, CopyOutcome::Complete);
        assert_eq!(
            copied.report_hash.as_deref(),
            Some(sha256_hex(b"## Report\nok\n").as_str())
        );
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "## Report\nok\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.dir().join("library/t-0001/out.txt")).unwrap(),
            "data"
        );
        assert_eq!(
            std::fs::read_to_string(
                project
                    .dir()
                    .join("library/t-0001/it's a [folder]/$(data); file.txt")
            )
            .unwrap(),
            "nested data"
        );
        std::fs::write(dir.join("library/out.txt"), "updated").unwrap();
        std::fs::write(dir.join("report.md"), "## Report\nupdated\n").unwrap();
        let copied = copy_home_local(&project, &t, true);
        assert_eq!(copied.outcome, CopyOutcome::Complete);
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "## Report\nupdated\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.dir().join("library/t-0001/out.txt")).unwrap(),
            "updated"
        );

        #[cfg(unix)]
        {
            let outside = work.path().join("outside.txt");
            std::fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("library/link")).unwrap();
            let copied = copy_home_local(&project, &t, true);
            assert!(matches!(copied.outcome, CopyOutcome::Partial(_)));
            assert!(!project.dir().join("library/t-0001/link").exists());
        }
    }

    #[test]
    fn source_hard_links_are_not_hashed_or_copied_and_previous_reports_survive() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "previous report").unwrap();
        assert_eq!(
            copy_home_local(&project, &t, false).outcome,
            CopyOutcome::Complete
        );
        let outside = work.path().join("private.txt");
        std::fs::write(&outside, "private outside data").unwrap();
        std::fs::remove_file(dir.join("report.md")).unwrap();
        std::fs::hard_link(&outside, dir.join("report.md")).unwrap();
        std::fs::hard_link(&outside, dir.join("library/private.txt")).unwrap();
        std::fs::write(dir.join("library/safe.txt"), "safe deliverable").unwrap();

        assert!(local_report_hash(&t).is_none());
        let copied = copy_home_local(&project, &t, true);
        assert!(
            matches!(&copied.outcome, CopyOutcome::Partial(notes) if notes.len() == 2 && notes.iter().all(|note| note.contains("singly linked")))
        );
        assert!(copied.report_hash.is_none());
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "previous report"
        );
        let target = project.dir().join("library").join(&t.id);
        assert!(!target.join("private.txt").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("safe.txt")).unwrap(),
            "safe deliverable"
        );
    }

    #[test]
    fn transfer_budget_bounds_growth_after_preflight_across_files() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "retained report").unwrap();
        assert_eq!(
            copy_home_local(&project, &t, false).outcome,
            CopyOutcome::Complete
        );
        let source = dir.join("library");
        for name in ["one.bin", "two.bin"] {
            std::fs::write(source.join(name), "new").unwrap();
        }
        let deadline = Instant::now() + LOCAL_COPY_TIMEOUT;
        assert_eq!(library_size(&source, deadline).unwrap(), 6);
        // Real source mutation after the exact preflight production uses.
        let grown_size = 30 * 1024 * 1024;
        for name in ["one.bin", "two.bin"] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(source.join(name))
                .unwrap()
                .set_len(grown_size)
                .unwrap();
        }
        let target = project.dir().join("library").join(&t.id);
        make_copy_dir(&target).unwrap();
        let mut budget = CopyBudget {
            remaining: LIBRARY_CAP_KB * 1024,
            deadline,
        };
        let mut buffer = [0_u8; 8192];
        let mut notes = Vec::new();
        assert!(
            !copy_library_tree(&source, &target, &mut budget, &mut buffer, &mut notes).unwrap()
        );
        assert_eq!(budget.remaining, 0);
        assert!(
            notes
                .iter()
                .any(|note| note.contains("grew beyond the 50 MB cap"))
        );
        let delivered: Vec<_> = std::fs::read_dir(&target)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            delivered.len(),
            1,
            "an incomplete staging file was published or retained"
        );
        assert_eq!(std::fs::metadata(&delivered[0]).unwrap().len(), grown_size);
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "retained report"
        );
    }

    #[test]
    fn an_expired_copy_deadline_prevents_destination_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(source.join("data.txt"), "data").unwrap();
        let mut budget = CopyBudget {
            remaining: LIBRARY_CAP_KB * 1024,
            deadline: Instant::now() - Duration::from_secs(1),
        };
        let mut buffer = [0_u8; 8192];
        let error = copy_library_tree(&source, &target, &mut budget, &mut buffer, &mut Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains("60 second timeout"));
        assert!(std::fs::read_dir(target).unwrap().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn readonly_source_artifacts_copy_twice_without_changing_source_permissions() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        let artifact = dir.join("library/result.txt");
        std::fs::write(&artifact, "readonly artifact").unwrap();
        let original_permissions = std::fs::metadata(&artifact).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&artifact, permissions).unwrap();
        std::fs::write(dir.join("report.md"), "first report").unwrap();
        assert_eq!(
            copy_home_local(&project, &t, true).outcome,
            CopyOutcome::Complete
        );
        let target = project.dir().join("library").join(&t.id).join("result.txt");
        assert!(!std::fs::metadata(&target).unwrap().permissions().readonly());

        std::fs::write(dir.join("report.md"), "updated report").unwrap();
        assert_eq!(
            copy_home_local(&project, &t, true).outcome,
            CopyOutcome::Complete
        );
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "updated report"
        );
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "readonly artifact"
        );
        let permissions = std::fs::metadata(&artifact).unwrap().permissions();
        assert!(
            permissions.readonly(),
            "copy changed the source's permissions"
        );
        std::fs::set_permissions(artifact, original_permissions).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_required_thread_paths_copy_report_and_library_and_retain_results() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let cwd = std::fs::canonicalize(work.path()).unwrap().join("working.");
        std::fs::create_dir(&cwd).unwrap();
        let cwd = crate::paths::canonicalize(&cwd).unwrap();
        let cwd_text = cwd.to_string_lossy();
        let t = allocate(&project, |t| {
            t.cwd = cwd_text.to_string();
            t.thread_dir = thread_dir(&cwd_text, &project.slug, &t.id);
        })
        .unwrap();
        std::fs::create_dir_all(t.library_path()).unwrap();
        std::fs::write(t.report_path(), "## Report\nverbatim result\n").unwrap();
        let library = PathBuf::from(t.library_path());
        std::fs::create_dir(library.join("nested")).unwrap();
        std::fs::write(
            library.join("nested").join("it's a [result].txt"),
            "library result",
        )
        .unwrap();

        let copied = copy_home_local(&project, &t, true);
        assert_eq!(copied.outcome, CopyOutcome::Complete);
        assert_eq!(
            copied.report_hash,
            Some(sha256_hex(b"## Report\nverbatim result\n"))
        );
        let home_report = home_report_path(&project, &t.id);
        let home_library = project
            .dir()
            .join("library")
            .join(&t.id)
            .join("nested")
            .join("it's a [result].txt");
        assert_eq!(
            std::fs::read_to_string(&home_report).unwrap(),
            "## Report\nverbatim result\n"
        );
        assert_eq!(
            std::fs::read_to_string(&home_library).unwrap(),
            "library result"
        );

        std::fs::remove_dir_all(&t.thread_dir).unwrap();
        assert_eq!(
            copy_home_local(&project, &t, true).outcome,
            CopyOutcome::Complete
        );
        assert_eq!(
            std::fs::read_to_string(home_report).unwrap(),
            "## Report\nverbatim result\n"
        );
        assert_eq!(
            std::fs::read_to_string(home_library).unwrap(),
            "library result"
        );
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_library_report_and_thread_dir_are_not_copied() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::remove_dir(dir.join("library")).unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("library")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("report.md")).unwrap();
        let copied = copy_home_local(&project, &t, true);
        match copied.outcome {
            CopyOutcome::Partial(notes) => assert_eq!(notes.len(), 2, "{notes:?}"),
            other => panic!("{other:?}"),
        }
        assert!(copied.report_hash.is_none());
        assert!(!home_report_path(&project, &t.id).exists());
        assert!(!project.dir().join("library/t-0001").exists());

        let real = work.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("report.md"), "secret").unwrap();
        let linked = work.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let t2 = allocate(&project, |t| {
            t.thread_dir = linked.to_string_lossy().into_owned()
        })
        .unwrap();
        let copied = copy_home_local(&project, &t2, true);
        assert!(matches!(copied.outcome, CopyOutcome::Partial(_)));
        assert!(!home_report_path(&project, &t2.id).exists());
    }

    #[test]
    fn library_over_the_cap_is_not_copied() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "r").unwrap();
        std::fs::File::create(dir.join("library/large.bin"))
            .unwrap()
            .set_len(LIBRARY_CAP_KB * 1024 + 1)
            .unwrap();
        let copied = copy_home_local(&project, &t, true);
        assert!(
            matches!(&copied.outcome, CopyOutcome::Partial(notes) if notes[0].contains("over the 50 MB cap"))
        );
        assert!(home_report_path(&project, &t.id).is_file());
        assert!(!project.dir().join("library").join(&t.id).exists());
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("library/large.bin"))
            .unwrap()
            .set_len(LIBRARY_CAP_KB * 1024)
            .unwrap();
        assert_eq!(
            copy_home_local(&project, &t, true).outcome,
            CopyOutcome::Complete
        );
        assert_eq!(
            std::fs::metadata(project.dir().join("library").join(&t.id).join("large.bin"))
                .unwrap()
                .len(),
            LIBRARY_CAP_KB * 1024
        );
    }

    #[test]
    fn failed_library_copy_retains_the_report() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "retained").unwrap();
        std::fs::write(dir.join("library/data.txt"), "data").unwrap();
        std::fs::write(project.dir().join("library").join(&t.id), "not a directory").unwrap();
        let copied = copy_home_local(&project, &t, true);
        assert!(matches!(copied.outcome, CopyOutcome::Failed(_)));
        assert_eq!(copied.report_hash, Some(sha256_hex(b"retained")));
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "retained"
        );
    }

    #[test]
    fn an_existing_destination_hard_link_does_not_modify_outside_data() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        let outside = work.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let target = project.dir().join("library").join(&t.id);
        std::fs::create_dir(&target).unwrap();
        std::fs::hard_link(&outside, target.join("data.txt")).unwrap();
        std::fs::write(dir.join("library/data.txt"), "deliverable").unwrap();
        assert_eq!(
            copy_home_local(&project, &t, true).outcome,
            CopyOutcome::Complete
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "deliverable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_skipped_at_source_and_rejected_at_destination() {
        use crate::remote::local_command;
        use crate::runner::Cmd;
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let project = project::create(root.path(), "demo", "", vec![]).unwrap();
        let dir = work.path().join(".herdr-project/demo-t-0001");
        let t = local_thread(&project, &dir);
        std::fs::write(dir.join("report.md"), "report retained").unwrap();
        let outside = work.path().join("it's outside [data]");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        let link_dir = |target: &Path, link: &Path| {
            let script = local_command(
                "New-Item",
                &[
                    "-ItemType",
                    "Junction",
                    "-Path",
                    &link.to_string_lossy(),
                    "-Target",
                    &target.to_string_lossy(),
                    "-ErrorAction",
                    "Stop",
                ],
            );
            let out = RealRunner
                .run(
                    &Cmd::new("pwsh.exe", std::time::Duration::from_secs(10)).args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        &script,
                    ]),
                )
                .unwrap();
            assert!(out.success(), "{}", out.error_text());
        };
        let source_link = dir.join("library/link");
        link_dir(&outside, &source_link);
        let copied = copy_home_local(&project, &t, true);
        assert!(matches!(copied.outcome, CopyOutcome::Partial(_)));
        let target = project.dir().join("library").join(&t.id);
        assert!(!target.join("link").exists());
        std::fs::remove_dir(&source_link).unwrap();
        std::fs::create_dir(&source_link).unwrap();
        std::fs::write(source_link.join("secret.txt"), "overwrite attempt").unwrap();
        link_dir(&outside, &target.join("link"));
        let copied = copy_home_local(&project, &t, true);
        assert!(matches!(copied.outcome, CopyOutcome::Failed(_)));
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "secret"
        );
        assert_eq!(
            std::fs::read_to_string(home_report_path(&project, &t.id)).unwrap(),
            "report retained"
        );
        std::fs::remove_dir(target.join("link")).unwrap();
    }
}

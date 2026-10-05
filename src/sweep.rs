//! `sweep`: what a project left behind and nothing uses any more. Worktrees
//! on `hp/<slug>/` branches (or a former slug's) with no open thread, local branches of resolved
//! threads whose pull request merged, tabs of resolved threads, their
//! workspaces still open on a worktree that is gone, working
//! folders of long-resolved tab threads, handled inbox items older than 30
//! days, and empty repository Spaces herdr grouped their worktrees under.
//! `--dry-run` lists; otherwise each is removed after a confirmation.
//! Current ownership is checked again under the project lock; branch deletion
//! compares against the approved merged tip, never a later commit.
//! Remote machines are left to `thread resolve`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::paths::Ctx;
use crate::project::Project;
use crate::runner::Cmd;
use crate::thread::{self, Kind, Status};
use crate::threads;

#[derive(Debug, Clone, PartialEq)]
pub enum Orphan {
    /// `thread` is the resolved thread that still owns it, if any: its files
    /// are copied home again, completely, before the worktree may go.
    Worktree {
        repo: String,
        path: String,
        workspace: Option<String>,
        branch: String,
        thread: Option<String>,
        owner_workspace: Option<String>,
    },
    Branch {
        repo: String,
        branch: String,
        id: String,
        head_oid: String,
        path: String,
        workspace: String,
    },
    Tab {
        id: String,
        tab: String,
    },
    /// A resolved thread's workspace, still open on a worktree folder that is gone.
    Workspace {
        id: String,
        workspace: String,
        repo: String,
        path: String,
        branch: String,
    },
    Folder {
        id: String,
        path: PathBuf,
    },
    Space(crate::spaces::Space),
    DoneItems {
        count: usize,
    },
}

impl Orphan {
    pub fn describe(&self) -> String {
        match self {
            Orphan::Worktree {
                path,
                branch,
                workspace,
                ..
            } => format!(
                "worktree {path} ({branch}){}{} with no open thread",
                if workspace.is_some() {
                    ", owning workspace "
                } else {
                    ""
                },
                workspace.as_deref().unwrap_or("")
            ),
            Orphan::Branch { branch, .. } => {
                format!("branch {branch}: its thread is resolved and its pull request merged")
            }
            Orphan::Tab { id, tab } => format!("tab {tab} of resolved thread {id}"),
            Orphan::Workspace { id, workspace, .. } => {
                format!("workspace {workspace} of resolved thread {id}, its worktree already gone")
            }
            Orphan::Folder { id, path } => format!(
                "working folder {} of {id}, resolved long ago (its report is home)",
                path.display()
            ),
            Orphan::Space(space) => format!(
                "empty Space {} ({}) its threads' worktrees were grouped under",
                space.label, space.id
            ),
            Orphan::DoneItems { count } => {
                format!("{count} handled inbox item(s) older than 30 days")
            }
        }
    }
}

fn git(ctx: &Ctx, repo: &str, args: &[&str]) -> Option<String> {
    let out = ctx
        .runner
        .run(
            &Cmd::new("git", Duration::from_secs(10))
                .args(["-C", repo])
                .args(args.iter().copied()),
        )
        .ok()?;
    out.success().then_some(out.stdout)
}

/// `git worktree list --porcelain`: (path, branch without refs/heads/).
pub fn parse_worktrees(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut path = String::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = p.to_string();
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            found.push((path.clone(), b.to_string()));
        }
    }
    found
}

fn older_than(stamp: &str, days: u32, now: jiff::Timestamp) -> bool {
    days > 0 && thread::seconds_since(stamp, now) > i64::from(days) * 86_400
}

pub fn find(ctx: &Ctx, project: &Project) -> Vec<Orphan> {
    let prefixes = project.branch_prefixes();
    let threads = thread::list(project);
    let settings = project
        .read_project_md()
        .map(|(s, _)| s)
        .unwrap_or_default();
    let view = threads::session_view(ctx, project);
    let now = jiff::Timestamp::now();
    let mut orphans = Vec::new();

    // Local repositories this project works in.
    let mut repos: Vec<String> = settings
        .repos
        .iter()
        .filter(|r| r.machine.is_none())
        .map(|r| r.path.clone())
        .collect();
    repos.extend(
        threads
            .iter()
            .filter(|t| !t.is_remote() && !t.repo.is_empty() && t.kind == Kind::Worktree)
            .map(|t| t.repo.clone()),
    );
    repos.sort();
    repos.dedup();
    let state = crate::steps::load_state(project);
    let open = |branch: &str, path: &str| {
        threads.iter().any(|t| {
            t.status != Status::Resolved
                && (t.branch == branch
                    || crate::paths::same_dir(t.worktree_path.as_ref(), path.as_ref()))
        })
    };
    for repo in &repos {
        let listed = git(ctx, repo, &["worktree", "list", "--porcelain"])
            .map(|t| parse_worktrees(&t))
            .unwrap_or_default();
        let mut with_worktree = Vec::new();
        for (path, branch) in listed {
            if !prefixes.iter().any(|p| branch.starts_with(p)) {
                continue;
            }
            with_worktree.push(branch.clone());
            if open(&branch, &path) {
                continue;
            }
            let owner = threads.iter().find(|t| {
                !t.is_remote()
                    && crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
                    && t.branch == branch
                    && crate::paths::same_dir(t.worktree_path.as_ref(), path.as_ref())
            });
            if owner.is_some_and(|t| t.kept_worktree)
                || threads.iter().any(|t| {
                    !t.is_remote()
                        && ((crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
                            && t.branch == branch)
                            || crate::paths::same_dir(t.worktree_path.as_ref(), path.as_ref()))
                        && owner.is_none_or(|owner| owner.id != t.id)
                })
            {
                continue;
            }
            // A same-directory pane elsewhere is not this thread's workspace.
            let workspace = owner
                .filter(|t| crate::paths::same_dir(path.as_ref(), t.worktree_path.as_ref()))
                .and_then(|t| {
                    view.as_ref()
                        .and_then(|v| threads::own_workspace(t, &v.panes))
                });
            orphans.push(Orphan::Worktree {
                repo: repo.clone(),
                path,
                workspace,
                branch,
                thread: owner.map(|t| t.id.clone()),
                owner_workspace: owner.map(|t| t.workspace_id.clone()),
            });
        }
        let refs: Vec<String> = prefixes.iter().map(|p| format!("refs/heads/{p}")).collect();
        let args: Vec<&str> = ["for-each-ref", "--format=%(refname:short)"]
            .into_iter()
            .chain(refs.iter().map(String::as_str))
            .collect();
        let branches = git(ctx, repo, &args).unwrap_or_default();
        for branch in branches.lines().map(str::trim).filter(|b| !b.is_empty()) {
            if with_worktree.iter().any(|b| b == branch) {
                continue;
            }
            let tip = git(
                ctx,
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            );
            let owner = threads.iter().find(|t| {
                !t.is_remote()
                    && crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
                    && t.branch == branch
                    && t.status == Status::Resolved
                    && !t.kept_worktree
                    && t.pr_state.eq_ignore_ascii_case("merged")
                    && state.prs.get(&t.id).is_some_and(|s| {
                        !s.head_oid.is_empty()
                            && tip.as_ref().is_some_and(|tip| tip.trim() == s.head_oid)
                    })
            });
            if let Some(owner) = owner {
                orphans.push(Orphan::Branch {
                    repo: repo.clone(),
                    branch: branch.to_string(),
                    id: owner.id.clone(),
                    head_oid: state.prs[&owner.id].head_oid.clone(),
                    path: owner.worktree_path.clone(),
                    workspace: owner.workspace_id.clone(),
                });
            }
        }
    }

    for t in threads
        .iter()
        .filter(|t| t.status == Status::Resolved && !t.is_remote())
    {
        if matches!(t.kind, Kind::Tab | Kind::Checkout)
            && let Some(view) = &view
            && !t.pane_id.is_empty()
            && thread::live_state(t, &view.agents, &view.panes, now).pane_exists
        {
            orphans.push(Orphan::Tab {
                id: t.id.clone(),
                tab: t.tab_id.clone(),
            });
        }
        if t.kind == Kind::Worktree
            && threads::worktree_gone(t)
            && !t.kept_worktree
            && let Some(workspace) = view
                .as_ref()
                .and_then(|v| threads::own_workspace(t, &v.panes))
        {
            orphans.push(Orphan::Workspace {
                id: t.id.clone(),
                workspace,
                repo: t.repo.clone(),
                path: t.worktree_path.clone(),
                branch: t.branch.clone(),
            });
        }
        let folder = project.dir().join("threads").join(&t.id);
        if t.kind == Kind::Tab
            && folder.is_dir()
            && older_than(&t.updated, settings.auto_resolve_days.max(1), now)
            && thread::home_report_path(project, &t.id).is_file()
        {
            orphans.push(Orphan::Folder {
                id: t.id.clone(),
                path: folder,
            });
        }
    }

    if let Some(view) = &view {
        orphans.extend(
            crate::spaces::empty(ctx, project, &view.herdr, true)
                .into_iter()
                .map(Orphan::Space),
        );
    }

    let limit = std::time::Duration::from_secs(
        u64::from(crate::steps::DONE_RETENTION_DAYS as u32) * 86_400,
    );
    let old_done = std::fs::read_dir(project.dir().join("inbox/done"))
        .map(|e| {
            e.flatten()
                .filter(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > limit)
                })
                .count()
        })
        .unwrap_or(0);
    if old_done > 0 {
        orphans.push(Orphan::DoneItems { count: old_done });
    }
    orphans
}

fn worktree_owner(
    project: &Project,
    id: &str,
    repo: &str,
    path: &str,
    branch: &str,
) -> Result<thread::Thread> {
    let t = thread::load(project, id)?;
    if t.status != Status::Resolved
        || t.kept_worktree
        || t.is_remote()
        || t.kind != Kind::Worktree
        || !crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
        || !crate::paths::same_dir(t.worktree_path.as_ref(), path.as_ref())
        || t.branch != branch
    {
        bail!("the thread no longer permits this cleanup");
    }
    Ok(t)
}

fn remove(ctx: &Ctx, project: &Project, orphan: &Orphan) -> Result<()> {
    // Copying takes its own short-lived project locks; never nest those locks.
    if let Orphan::Worktree {
        repo,
        path,
        branch,
        thread: Some(id),
        ..
    } = orphan
    {
        let t = worktree_owner(project, id, repo, path, branch)?;
        let copied = threads::final_copy(ctx, project, &t);
        if copied.outcome != thread::CopyOutcome::Complete {
            bail!("not everything in it could be copied home first");
        }
    }
    let lock = project.lock()?;
    match orphan {
        Orphan::Worktree {
            repo,
            path,
            branch,
            workspace: planned_workspace,
            thread: owner,
            owner_workspace,
        } => {
            let owner = owner
                .as_ref()
                .map(|id| worktree_owner(project, id, repo, path, branch))
                .transpose()?;
            if owner.as_ref().map(|t| &t.workspace_id) != owner_workspace.as_ref() {
                bail!("the recorded owning workspace changed");
            }
            if thread::list(project).iter().any(|t| {
                !t.is_remote()
                    && ((crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
                        && t.branch == *branch)
                        || crate::paths::same_dir(t.worktree_path.as_ref(), path.as_ref()))
                    && owner.as_ref().is_none_or(|owner| owner.id != t.id)
            }) {
                bail!("another thread now owns this worktree");
            }
            if !project
                .branch_prefixes()
                .iter()
                .any(|p| branch.starts_with(p))
            {
                bail!("this branch is no longer allocated to the project");
            }
            let listed = git(ctx, repo, &["worktree", "list", "--porcelain"])
                .ok_or_else(|| anyhow::anyhow!("could not verify the worktree's repository"))?;
            if !parse_worktrees(&listed)
                .iter()
                .any(|(p, b)| b == branch && crate::paths::same_dir(p.as_ref(), path.as_ref()))
            {
                bail!("the planned worktree or branch changed");
            }
            let view = threads::session_view(ctx, project);
            let workspace = owner.as_ref().and_then(|t| {
                view.as_ref()
                    .and_then(|v| threads::own_workspace(t, &v.panes))
            });
            if workspace != *planned_workspace {
                bail!("the worktree's owning workspace changed");
            }
            threads::remove_local_worktree(
                ctx,
                repo,
                path,
                branch,
                view.as_ref()
                    .zip(workspace.as_deref())
                    .map(|(view, workspace)| (&view.herdr, workspace)),
            )?;
            if let Some(owner) = owner {
                thread::update_locked(project, &owner.id, &lock, |t| t.worktree_path.clear())?;
            }
            Ok(())
        }
        Orphan::Branch {
            repo,
            branch,
            id,
            head_oid,
            path,
            workspace,
        } => {
            let t = thread::load(project, id)?;
            if t.status != Status::Resolved
                || t.kept_worktree
                || t.is_remote()
                || t.kind != Kind::Worktree
                || !crate::paths::same_dir(t.repo.as_ref(), repo.as_ref())
                || t.branch != *branch
                || t.worktree_path != *path
                || t.workspace_id != *workspace
                || !t.pr_state.eq_ignore_ascii_case("merged")
                || !crate::steps::load_state(project)
                    .prs
                    .get(id)
                    .is_some_and(|s| s.head_oid == *head_oid)
            {
                bail!("the thread no longer permits this branch cleanup");
            }
            if thread::list(project).iter().any(|other| {
                other.id != *id
                    && !other.is_remote()
                    && crate::paths::same_dir(other.repo.as_ref(), repo.as_ref())
                    && other.branch == *branch
            }) {
                bail!("the branch is now in use");
            }
            threads::delete_local_branch(ctx, repo, branch, head_oid)
        }
        Orphan::Tab { id, tab } => {
            let t = thread::load(project, id)?;
            let view = threads::session_view(ctx, project)
                .ok_or_else(|| anyhow::anyhow!("the session is not reachable"))?;
            if t.status != Status::Resolved
                || t.is_remote()
                || !matches!(t.kind, Kind::Tab | Kind::Checkout)
                || t.tab_id != *tab
                || !view
                    .panes
                    .iter()
                    .any(|p| thread::pane_matches(&t, p) && p.tab_id == *tab)
            {
                bail!("the thread no longer owns this resolved tab");
            }
            view.herdr
                .call(&["tab", "close", tab], crate::herdr::CALL_TIMEOUT)
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        Orphan::Workspace {
            id,
            workspace,
            repo,
            path,
            branch,
        } => {
            let t = worktree_owner(project, id, repo, path, branch)?;
            let view = threads::session_view(ctx, project)
                .ok_or_else(|| anyhow::anyhow!("the session is not reachable"))?;
            if !threads::worktree_gone(&t)
                || threads::own_workspace(&t, &view.panes).as_ref() != Some(workspace)
            {
                bail!("the gone worktree or its owning workspace changed");
            }
            view.herdr
                .call(
                    &["workspace", "close", workspace],
                    crate::herdr::CALL_TIMEOUT,
                )
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !threads::worktree_gone(&t) {
                bail!("the worktree folder reappeared; its recorded path was kept");
            }
            thread::update_locked(project, id, &lock, |t| t.worktree_path.clear())?;
            Ok(())
        }
        Orphan::Folder { id, path } => {
            let t = thread::load(project, id)?;
            let (settings, _) = project.read_project_md()?;
            if t.status != Status::Resolved
                || t.is_remote()
                || t.kind != Kind::Tab
                || *path != project.dir().join("threads").join(id)
                || !older_than(
                    &t.updated,
                    settings.auto_resolve_days.max(1),
                    jiff::Timestamp::now(),
                )
                || !thread::home_report_path(project, id).is_file()
            {
                bail!("the thread no longer permits this working-folder cleanup");
            }
            Ok(std::fs::remove_dir_all(path)?)
        }
        Orphan::Space(space) => {
            let view = threads::session_view(ctx, project)
                .ok_or_else(|| anyhow::anyhow!("the session is not reachable"))?;
            if !crate::spaces::empty(ctx, project, &view.herdr, true)
                .iter()
                .any(|current| current.id == space.id)
            {
                bail!("this Space is no longer empty and owned by the project");
            }
            crate::spaces::close(&view.herdr, space).map_err(|e| anyhow::anyhow!("{e}"))
        }
        Orphan::DoneItems { .. } => {
            crate::inbox::prune_done(project, crate::steps::DONE_RETENTION_DAYS);
            Ok(())
        }
    }
}

/// `sweep <slug> [--dry-run] [--yes]`.
pub fn run(ctx: &Ctx, slug: &str, dry_run: bool, yes: bool) -> Result<()> {
    let project = Project::load(&ctx.root, slug)?;
    let orphans = find(ctx, &project);
    if orphans.is_empty() {
        println!("nothing to clean in `{slug}`");
        return Ok(());
    }
    for orphan in &orphans {
        println!("{}", orphan.describe());
    }
    if dry_run {
        return Ok(());
    }
    if !yes {
        use std::io::{BufRead, IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            bail!("pass --yes to remove these (or --dry-run to only list them)");
        }
        print!("Remove all of this? [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            println!("nothing was removed");
            return Ok(());
        }
    }
    let mut failed = 0;
    for orphan in &orphans {
        match remove(ctx, &project, orphan) {
            Ok(()) => println!("removed: {}", orphan.describe()),
            Err(error) => {
                failed += 1;
                println!("cleanup failed: {} ({error:#})", orphan.describe());
            }
        }
    }
    println!(
        "swept `{slug}`: {} removed, {failed} failed",
        orphans.len() - failed
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_worktrees_are_parsed() {
        let text = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /wt/x\nHEAD def\nbranch refs/heads/hp/demo/t-0001-x\n\nworktree /wt/detached\nHEAD 123\ndetached\n";
        assert_eq!(
            parse_worktrees(text),
            [
                ("/repo".to_string(), "main".to_string()),
                ("/wt/x".into(), "hp/demo/t-0001-x".into())
            ]
        );
    }
}

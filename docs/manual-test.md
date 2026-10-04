# Manual test list

## Native Windows port

The coordinator check was exercised on 2026-10-04 on Windows 11 x64 with Herdr 0.9.3 and oh-my-pi 18.5.1; additional native observations are recorded below. Local native `cargo test --locked` passed **364 tests (359 unit + 5 real CLI)**. Actual runs also exercised workers/progress, failed-thread/server restart, named-pipe focus, ConPTY popup, report/artifact copy, dirty-worktree protection, clean orphan cleanup, rename/delete, configure/unconfigure, command ownership, installer preservation and plugin runtime. The [first hosted sync run](https://github.com/ubranch/herdr-projects/actions/runs/37242425743) passed Linux locked tests, debug build and actual CLI version/new/list/context smoke; hosted Windows was not green because a cold-start process-tree test fixture timed out before its descendant launched, so publication was correctly skipped. Host-client visual interaction and installation of a genuinely newer Windows release were not exercised.

Use the [Windows source install](getting-started.md#native-windows-x64-this-checkout) from the published `main` of [ubranch/herdr-projects](https://github.com/ubranch/herdr-projects), not an upstream Windows install or an unavailable Windows release asset. Run against a scratch named Herdr session and projects root under a path such as `C:\scratch\HP O'Brien 世界`, with separate `HERDR_CONFIG_PATH` and `XDG_BIN_HOME`. In the scratch Herdr config use `[terminal]` with `default_shell = "pwsh.exe"`. Start the session from an environment with `HOME` unset and `USERPROFILE` intact, passing `--session <scratch-name>` to session-sensitive commands. Preserve and use the user's existing OMP model/auth/skills; do not replace those settings to make a test pass. `scripts/dev-*` are Unix helpers, not native Windows setup scripts.

### Environment and agents

| Check | Acceptance | Status |
| --- | --- | --- |
| HOME unset; Unicode, spaces and apostrophe paths; actual coordinator | `new`/`open` with a native OMP profile starts the coordinator. It executes the printed PowerShell `skill` and `context` commands without losing the leading `&` or mangling paths, then replies `WINDOWS_COORDINATOR_READY`. | Exercised with Herdr 0.9.3 and OMP 18.5.1, using existing user model/auth/skills |
| Config, hooks and local IPC | In scratch hook/Herdr config files, `configure` preserves unrelated entries, including foreign sibling commands in mixed hook groups; `unconfigure` removes only plugin-owned commands and preserves later edits. The tab-bar command and hooks work when invoked through CMD with encoded `pwsh.exe` wrappers. `focus`/`unfocus` use the native named pipe while `herdr.sock` remains a regular `notUnixSocket` marker; an unavailable or stalled pipe fails within its deadline. | Actual native `focus`/`unfocus` passed. Configure/unconfigure in a fake home created hooks and NTFS `.claude`/`.agents` skill junctions, removed both junctions and preserved source `SKILL.md`; real user configs stayed untouched. Native suite passed mixed-hook/encoded-command and pipe deadline/cancellation fixtures; host tab-bar pixel rendering not exercised |
| Actual worker, progress and report | Start native OMP tab and git-worktree threads with a real task file. Their first turn follows the brief, not the coordinator instructions. Have them run `report --percent`, write `report.md` with `## Next`, and produce a library file. The ticker updates the group/progress, copies the report home, and forwards a prompt/Next line once. | Actual tab/worktree OMP workers each launched with one brief, reported 100%/Done and completed; worktree worker returned `WINDOWS_WORKER_READY`. Resolve copied `WINDOWS_NATIVE_COPY_READY` report and `WINDOWS_NATIVE_ARTIFACT_READY—café’s` artifact home exactly. Actual worker prompt/Next interaction was not exercised |
| Thread and server restart | Stop a worker and use `thread restart`; it resumes or starts the intended thread. Restart the scratch Herdr server and attach a client; recovery or explicit restart keeps the right thread identity, name and report, never matching a foreign pane when ids are reused. | Failed tab `t-0001` restarted through CLI with one brief and 100%/Done; raw verbatim-prefix cwd worked. Native Herdr 0.9.3 server restarted under isolated XDG after the controlling Eval kernel exited, restoring state/user OMP config; both actual workers completed. Native identity/cwd/resume/reused-pane fixtures passed |

### Copying and project lifecycle

| Check | Acceptance | Status |
| --- | --- | --- |
| Native copy and cap | With local `rsync`/`du` unavailable, nested singly linked regular files and the report copy home. A library of exactly 52,428,800 logical bytes copies; one byte more in preflight skips the library and preserves the report. Growth during copying cannot exceed the shared actual-transfer budget: discard the current staged file, report partial and retain the worktree. Source symlinks/junctions/hardlinks are skipped; report/hash handling also refuses source hardlinks using opened-handle validation. | Actual resolve copied the report and Unicode/smart-quote artifact exactly. Passing native fixtures cover cap/preflight, growth across files, source-link rejection and filesystem copying; no local `rsync`/`du` path is used |
| Destination boundary and partial copy | Destination symlinks/junctions and overlapping trees are refused; replacing a destination hardlink leaves its other name unchanged. Windows artifact copies stay writable even from a read-only source, without clearing attributes through existing destination hardlinks; Unix permissions are preserved. A read/write failure or the 60 second preflight/copy deadline leaves an already-copied report and the worktree intact, not a complete-copy claim. | Passing native fixtures cover destination hardlinks/junctions, writable copies of read-only sources, transfer budget/deadline and report retention on failed library copy |
| Dirty-worktree safety and resolve | Modify a tracked file before resolve: the dirty worktree and unmerged branch stay, and the copied report/library stay. A clean resolved thread removes only the intended worktree/workspace; partial copy prevents removal unless explicitly discarded. | Dirty tracked `fixture.txt` caused native Herdr `dirty_worktree_requires_force` refusal; worktree and branch stayed without forced cleanup after report/artifact copied home. Restoring the correct native CRLF fixture and closing its workspace allowed a clean orphan sweep: 1 removed, 0 kept, without force. Partial-copy safety fixtures passed |
| Rename/delete and concurrent writers | With the ticker and CLI writers active, rename updates records and synchronized `CLAUDE.md`; a foreign copy with an existing backup reports conflict and preserves both, including on name changes. Delete moves the project to `.trash/` without a sharing violation or a writer recreating its old folder. `.project-<slug>.lock` stays outside the moved folder and is never removed. | Actual new/rename/delete-to-trash succeeded with persistent root locks; doctor confirmed `AGENTS.md` and the `CLAUDE.md` copy. Native fixtures passed lock/waiter survival across moves, destination-token cutover, missing-project writer guards and foreign-copy/backup conflict preservation |
| Ticker lifecycle and attached popup | Repeated/concurrent `ticker start` leaves one ticker; stop releases its OS lock, not its persistent token. Readable metadata is in `.ticker.info`. With an attached native client, `prefix+a` opens the correctly scoped popup, keyboard actions work and closing restores the pane. Sidebar rows/tab count and notifications render. | Actual native ConPTY popup rendered scoped `Idle (2)` with both workers; `i` showed `t-0001` detail, Esc returned and closed. Exact manifest startup published a native ticker PID/version and then stopped; lock/start/stop fixtures passed. Host TUI tab-bar/sidebar pixels, popup-key/mouse interaction and notifications were not exercised |

### Installation and updater safety

| Check | Acceptance | Status |
| --- | --- | --- |
| Native command ownership | In a scratch `XDG_BIN_HOME`, startup/`doctor --fix` publishes `.exe` plus source/SHA-256 marker and refreshes an unchanged managed copy. Foreign/unmarked commands, a modified managed copy and destination links are left untouched; `doctor` identifies a missing PATH entry or a different command found first. | Actual `link-command.ps1` managed copy ran `--version` with correct hash/future source target; tampered managed copy and foreign file were preserved. Native fixtures passed source-update ownership, failed-marker rollback and doctor's shadowed/missing PATH preservation cases |
| Verified installer and running image | In an isolated release fixture, only the unique matching SHA-256 and expected runnable `--version` candidate is installed. SHA verification must work under Windows PowerShell 5.1 and PowerShell 7 even with inherited module paths. Missing/bad downloads fall back to source. A failed build or version validation leaves the old binary intact; a failed replacement restores it or explicitly reports rollback failure and the preserved previous path. A running old image is renamed rather than overwritten. | Windows PowerShell 5.1 verified download/SHA/version installation passed while the resident old-image PID stayed alive across rename/new install, with a truthful in-use note. Bad SHA and a correct-SHA Herdr 0.9.3 executable were rejected; missing-Cargo fallback failed and installed SHA stayed unchanged in both cases. A successful newer published Windows release was not exercised |
| Updater success and failure preservation | Use an isolated linked checkout with a newer release fixture: on clean `main`, update stops the old ticker, installs/verifies the new binary, runs its `doctor --fix` and restarts one ticker. Wrong branch/dirty checkout refuses before modification. A failed download/source build preserves the old usable binary and restarts its ticker; installed binary/command hashes, config, projects and reports remain intact. Replacement/rollback failures report the actual installed version or inability to verify it, not a false success. Do not switch the actual fork checkout to upstream `main` for this test. | Actual isolated linked `update --check` found installed 0.2.34/latest 0.2.36 on a local origin. Update pulled it and invoked the real Windows PowerShell installer; an intentionally invalid release fixture failed before compilation, preserving the old executable SHA and truthfully reporting failure/recovery. Unit refusal/decision cases passed; successful newer-version installation was not exercised |
| Plugin manifest and native runtime | Link this checkout in native Herdr 0.9.3; Windows build/pane entries must validate, and startup/actions must execute extensionless manifest commands as native `.exe` programs without a POSIX shell. | Native Herdr accepted the Windows build/pane manifest. Actual extensionless `pane.projects` ran in Herdr's ConPTY and rendered all project threads; exact `target/release/herdr-projects startup` vector exited 0 and published native ticker PID/version, then stopped |

## 0.2.0: the Herdr-native redesign

Checked on 2026-09-23 in a scratch `hp-dev` session (herdr 0.9.1, macOS, Claude Code 2.1.280). "Builder" means the builder ran it and read the result; "client-witnessed" needs a person looking at an attached Herdr client.

| Slice | Check | How it was checked |
| --- | --- | --- |
| 1 | `new` writes `AGENTS.md`, `CLAUDE.md` → `AGENTS.md`, `uploads/`; `open` starts an agent there and its first reply runs `skill` and `context` through the absolute path | Builder (Claude Code). Codex and OpenCode: client-witnessed |
| 1 | A `--kind tab` thread's first turn does not run them and does not call itself the coordinator | Builder |
| 1 | A brief starts with the project header and has no operational settings; `thread prompt` lands under `## Follow-ups`; `thread show --json` has the Next list | Builder, unit |
| 1 | `doctor --fix` adds the priming files and `routines/pr-followup.md` to an older project | Builder, unit |
| 2 | After `configure`, a thread's sub-line shows its activity within one tick; a thread that asks a question is `needs you` within one tick | Builder (hooks from a scratch settings file via `--settings`) |
| 2 | `unconfigure` leaves the hook files byte-identical, keeping later user edits | Unit, builder |
| 2 | A thread survives a server restart with native resume and keeps its group and name; a cleared name is re-applied | Builder (client attached through `script`) |
| 3 | Tokens `hp_project`, `hp_rank`, `hp_group`, the display name and `hp_sub` are set, a coordinator's display name is its project's name; old tokens cleared | Builder (`herdr api snapshot`) |
| 3 | Four-line rows, colours, the project count and `projects: N need you` render; `focus <slug>` narrows and `unfocus` restores the by-need order | Client-witnessed |
| 4 | `prefix+a` opens the popup scoped to the current project (also in a thread's workspace), and on all projects elsewhere; `P` opens the project picker on the current scope, `/` filters it, `esc` clears then closes, from any section; `↵` focuses the thread and the popup is gone | Client-witnessed (the same TUI was driven in a pane by the builder) |
| 4 | A Next number key reaches the thread and its task file; `s` stops a working thread; a settings edit reaches PROJECT.md; `X` asks first | Builder |
| 5 | Resolving a merged thread leaves no worktree, branch or workspace and keeps `threads/<id>.md` and `library/<id>/`; an unmerged one keeps its branch and says so | Builder |
| 5 | `sweep --dry-run` lists a planted orphan worktree and `sweep --yes` removes it; `archive` closes and hides, `unarchive` reopens | Builder |
| 6 | A real pull request is polled; a review comment fires `pr-followup` and the thread fixes it; forwarding "Merge the PR" merges it; the merge resolves and cleans the thread | Builder (private scratch repo `eliasstravik/hp-pr-probe`) |
| 6 | A needs-you event gives one notification titled `<Project> · t-0009` with the request sound; `mute` silences it; the nudge waits for a minute of idle | Unit. Seeing the notification: client-witnessed |

## Before 0.2.0

The acceptance checks from the original plan, by stage, with how each was checked on 2026-09-17 (herdr 0.9.1; macOS 26 on the home Mac, Linux aarch64 on the second machine). "Builder" means the builder ran it in the throwaway `hp-dev` session and read the result; "unit" means a test in `cargo test`; "client-witnessed" means it is visual and the client has to look. Details of each run are in [`herdr-notes.md`](herdr-notes.md).

## Set up a throwaway session

```bash
scripts/dev-server                                   # headless `hp-dev` session, root = <repo>/.dev-root
scripts/dev-hp new demo && scripts/dev-hp open demo --session hp-dev
scripts/dev-herdr pane read <pane>                   # read a pane; `pane send-keys <pane> Down Enter` answers a dialog
```

## Checks by stage

| Stage | Check | How it was checked |
| --- | --- | --- |
| 1 | `herdr plugin list` shows the plugin; after linking no ticker runs and `~/.herdr-projects` does not exist; `doctor --session hp-dev` reports the herdr version | Builder |
| 2 | `new demo` creates the skeleton; `open demo --session hp-dev` yields a workspace in `hp-dev` only, with a coordinator that ran `skill` and printed the digest | Builder |
| 2 | With an untrusted folder, `open` leaves `prime_pending = true`, and the ticker delivers the priming prompt after the dialog is accepted | Builder (folder moved under `/private/tmp` and symlinked, because `~/dev` is trusted) |
| 2 | The printed `context` command works from a scrubbed environment | Builder and unit (`tests/cli.rs`) |
| 2 | `ticker stop` ends the ticker within two ticks; `ticker start` after a rebuild replaces the old one | Builder (1 s; seen in `.ticker.log`) |
| 3 | `thread start` on a scratch repo: branch `hp/demo/t-0001-*`, record `open`, brief with instructions and memory, report written with no out-of-directory prompt; `git status` shows nothing from `.herdr-project/` | Builder |
| 3 | Kill the pane, `thread restart` brings it back; forced failure before the worktree exists then `thread restart` creates it; restart of a running thread refuses | Builder, unit (cases a to e) |
| 3 | Closing the pane of a thread with a report leaves it under Ready for review with `pane closed` | Builder, unit |
| 3 | `thread start` returns in under a minute; the ticker launches within two ticks; a missing agent binary gives `failed` after three attempts | Builder (1 s; 15 s; `--profile kimi`, not installed), unit |
| 3 | `thread prompt` reaches the thread; the README allow-list suppresses the prompt for the standard-input form | Builder (also a control: an off-list subcommand did prompt) |
| 3 | `--remove-worktree` refused for a tab thread and for a dirty worktree; with the ticker stopped, a late report survives `resolve --remove-worktree`; a tab thread starts in `threads/<id>/` | Builder, unit |
| 4 | Two tab threads in one workspace show different `thread` tokens | Builder (`api snapshot`) |
| 4 | `overview` run without a terminal returns at once; a finished thread stays under Ready for review until `thread ack` | Builder |
| 5 | A finishing thread gives one inbox item and one nudge, none after until a new item | Builder (with `nudge = true`), unit |
| 5 | A second session with the plugin linked produces no "pane gone" items | Builder (`hp-dev2`) |
| 5 | A command routine does not run until `routine_commands = true` and `routine approve` in a terminal; an edited command stops; approve without a terminal refuses | Builder (approved inside a herdr pane), unit |
| 5 | Fake `gh`: a merged pull request resolves its thread; a comment gives an item with no body; two events in one tick give two items | Unit. A run against a real pull request was not done (needs a push) |
| 6 | `thread start --machine` creates a worktree and agent on the second machine; the report and a library file come home within two minutes; Ready for review | Builder (about 70 s) |
| 6 | A short failed connection: no item, no group change, local ticks not slowed; a long one with a one-minute threshold: exactly one `outage` item and one recovery item | Builder (ssh shim on the ticker's `PATH`), unit |
| 6 | A title with quotes, spaces and `$(...)` reaches the remote `--label` unchanged and runs nothing | Builder |
| 6 | Always-on recipe: open a project on the second machine, reattach from this Mac with `herdr --remote <target> --session <name>`, answer a blocked prompt through it | Builder |
| 7 | `thread adopt` gives a thread with a brief; two adopted panes in one directory get separate thread directories; an adopted agent that ends in `done` still gets its pending prompt | Builder (first), unit (all three) |
| 7 | `adopt-workspace` creates a project from the current workspace | Builder: the action's handoff live, then the popup's core through the CLI. The popup itself: client-witnessed |
| 7 | A paused project refuses `thread start` and is skipped by the ticker; an archived one is hidden and its tokens are gone; `delete` refuses while the coordinator is alive, `--force` moves the folder to `.trash/` and leaves worktrees alone | Builder, unit |
| 7 | `new ../x`, `open ../x`, `thread list ../x` are refused | Unit (`tests/cli.rs`) |

## Client-witnessed checks

| Stage | Check | How to look |
| --- | --- | --- |
| 4 | `focus <slug>` shows only the project's panes in the sidebar, the coordinator first, then by attention; `unfocus` restores the full list | In a session with a project open and at least one thread plus one unrelated agent pane: run the `focus` action (or `herdr-projects focus <slug>`), look at the sidebar's agent list, then run `unfocus`. The builder verified that herdr accepts the request and that the tokens it filters on are present on the panes, but herdr does not expose the active view through `api snapshot`. |
| 6 | Remote thread panes in herdr's connected-machines sidebar: do they show the `project`, `thread` and `review` tokens? | With a remote thread running, select the machine in the sidebar (or run `herdr --remote <target>`) and look at the thread's agent row. The builder verified the tokens are set on the remote panes (`herdr --machine M api snapshot`) but cannot see a sidebar. `focus` does not cover remote threads either way. |
| 7 | The three interactive popups: `new` (asks name and goal, then creates and opens), `pick` (numbered project list for `open`, `pause`, `resume` when the current workspace is not a project) and `adopt` (asks the project name, pre-filled with the workspace label) | Run each action with `herdr plugin action invoke <id> --plugin herdr-projects`. The builder verified the actions' handoff and the code the popups run, but a headless session has no client to show or type into a popup. |
| 7 | The `overview` popup stays open until Enter, and the `doctor` action shows a notification | Run both actions with `herdr plugin action invoke`. Notifications are disabled in a headless session (`shown: false`). |

# herdr notes

What the builder verified about herdr, per stage, against the plan's "Assumptions to verify" table.

## Stage 1 (2026-09-17)

First pass ran on herdr 0.9.0; the client then upgraded the CLI to 0.9.1 and the rest was checked in a fresh `hp-dev` server on 0.9.1. The client's default session server was still 0.9.0 (`server_binary_stale: yes`) and was not touched.

| Assumption | Result |
| --- | --- |
| herdr is 0.9.1 or later on this Mac | Did not hold at first (`herdr 0.9.0`): stopped and asked; the client upgraded. Now `herdr 0.9.1`, and `doctor --session hp-dev` reports it. |
| A named session's server can start without an attached terminal | **Holds.** `herdr --session hp-dev server`, started detached with `HERDR_PROJECTS_ROOT` exported and pane variables scrubbed (`scripts/dev-herdr`), shows as `running` in `herdr session list`. |
| The builder can answer TUI dialogs with `herdr pane send-keys` | Mechanism holds: `pane send-text`, `pane send-keys <pane> Enter` and `pane read` work against a shell in `hp-dev`. Answering a real agent dialog is exercised in stage 2. |
| The herdr CLI targets a session when `HERDR_SOCKET_PATH` is set | **Holds.** With only `HERDR_SOCKET_PATH=<hp-dev socket>`, `herdr workspace list` returned hp-dev's (empty) list, not the default session's. |
| herdr commands have JSON output with ids | **Holds.** Output is JSON by default: `workspace create` returns `result.workspace.workspace_id` (`w1`), `result.tab.tab_id` (`w1:t1`), `result.root_pane.pane_id` (`w1:p1`), plus `cwd`, `foreground_cwd` and `agent_status`. `session list --json` returns `name`, `default`, `running`, `socket_path`. |
| An agent pane lacks the plugin environment variables | **Half true.** An ordinary agent pane (the builder's own) has `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` and `HERDR_ENV`. The plugin-only ones (`HERDR_PLUGIN_*`) were not present. Fallback: no change needed; the explicit `--root` prefix still works. |
| A detached child of a `[[startup]]` command survives | Not checkable until `ticker run` exists; checked at the start of stage 2, when the hp-dev server is restarted with a project present. `open` calls `ticker start` regardless, which is the fallback. |

Consequences noted:

- Because every pane has `HERDR_SOCKET_PATH`, a bare `herdr ...` or a flagless `doctor` run from a pane in the client's default session targets that session. All development commands therefore pass `--session hp-dev` and unset `HERDR_SOCKET_PATH`.
- Session sockets live at `~/.config/herdr/herdr.sock` (default) and `~/.config/herdr/sessions/<name>/herdr.sock`, but the binary asks `herdr session list --json` rather than assuming this layout.
- Panes in a named session also carry `HERDR_SESSION`, and inherit whatever the server was started with (`HERDR_PROJECTS_ROOT` was visible in an hp-dev pane). That is why the root is exported before the server starts.
- `herdr plugin link` is global (`~/.config/herdr/plugins.json`), not per session. After linking: `plugin list` shows `herdr-projects`, no `herdr-projects` process is running, and `~/.herdr-projects` does not exist.
- macOS ships `openrsync` (protocol 29) as `rsync`. Relevant to the stage 3 rsync assumption.

## Stage 2 (2026-09-17, herdr 0.9.1)

| Assumption | Result |
| --- | --- |
| A detached child of a `[[startup]]` command survives (carried over from stage 1) | **Holds.** With one project in the scratch root, restarting the `hp-dev` server ran `[[startup]]`; `ticker run` (new session via `setsid`, null stdio) was alive afterwards with the server's `HERDR_PROJECTS_ROOT`. |
| The builder can answer an agent's TUI dialogs with `pane send-keys` (completed from stage 1) | **Holds.** `pane send-keys <pane> Down` then `Enter` accepted claude's trust-this-folder dialog. |
| `workspace create`, `tab create`, `pane focus`/`agent focus`, `notification show` and `api snapshot` exist with the flags needed | **Holds.** `workspace create --cwd --label --focus/--no-focus`; `tab create --workspace --cwd --label`; `tab rename <TAB_ID> <LABEL>`; `agent focus <target>`; `notification show <TITLE> --body`; `api snapshot`. |
| A prompt sent while the user has half-typed text does not merge with or submit that text | **Does not hold.** With `HALFTYPED user draft` typed and not submitted, `herdr agent prompt` produced one submitted message `HALFTYPED user draftReply with exactly…`. **Fallback taken:** `nudge` defaults to `false`; with it off the ticker shows `herdr notification show` instead (built in stage 5); the risk goes in the README. |

Also learned:

- **herdr's CLI parser wants positionals first, options after.** `pane report-metadata --source x <PANE>` fails with `unknown option: x`; `pane report-metadata <PANE> --source x` works. There is no `--` separator except in `agent start`. Text in a positional slot may start with a dash (`agent prompt <target> "-x hello"` is accepted).
- **Errors** are one JSON object with `error.code` and `error.message`, exit status 1. `agent prompt` and `agent focus` accept a pane id as the target.
- **A trust dialog**: `agent start` fails fast (about 1 s, not a timeout) with `agent_not_ready` ("blocked during startup"), and `agent list` shows the agent as `blocked` with its name already set. After the dialog is accepted the state becomes `idle` and the ticker's pending-prompt path delivers. This settles the first stage 3 assumption early.
- **claude's folder trust is inherited from parent folders**, and `~/dev` is trusted on this Mac, so nothing under the scratch root shows a trust dialog. For the `demo2` check the project folder was moved to the builder's scratch directory under `/private/tmp` and symlinked back into `.dev-root`. That exposed that herdr reports a pane's *physical* working directory, so the recorded `cwd` is now the canonical project path.
- **Pane, tab and workspace ids restart from `w1` after a server restart**, so ids are reused for different panes across restarts. The identity check (ids plus working directory plus agent name) is what keeps a stale record from matching a new pane.
- `notification show` in a headless session returns `{"shown": false, "reason": "disabled"}` with exit 0.
- `api snapshot` exposes pane tokens at `result.snapshot.panes[].tokens` and `result.snapshot.agents[].tokens` (the coordinator pane showed `project`, `thread`, `rank`). Stage 4 sidebar checks can be read by the builder.
- An `hp-dev` server started from inside an agent's shell inherits that agent's environment (claude then reports "inherited CLAUDE_CODE_CHILD_SESSION"). `scripts/dev-server` therefore starts it with `env -i` and a minimal environment.
- The client's `claude` runs in auto mode, so the "ordinary first-command prompt" did not appear; the coordinator ran `skill` and `context` unprompted.

## Stage 3 (2026-09-17, herdr 0.9.1)

| Assumption | Result |
| --- | --- |
| `agent start` returns only once the agent is ready, and a trust dialog shows as `blocked` or a timeout | **Holds.** A ready agent returns in about 4 s with `agent_status: idle`. A trust dialog makes `agent start` fail after about 1 s with `agent_not_ready`, and `agent list` shows the agent `blocked`. A missing agent binary gives `timeout` after the full 20 s and no agent is listed. No extra `agent wait` is needed. |
| Pane, tab and workspace ids are not reused for a different pane, and either survive a server restart or all change together | **Partly.** Within one server run ids were never reused (`w1` … `wB`, hexadecimal). After a server restart they start again at `w1`, so an old record can name a new pane. The identity check (workspace id, tab id, working directory, agent name) is what prevents acting on the wrong pane, as the plan's fallback says. |
| `agent start` accepts a name that was used before | **Holds.** `thread restart` reused `hp-demo-t-0001` in a new pane after the old pane was closed. |
| `worktree remove` refuses a dirty worktree without a force option | **Holds.** Untracked or modified files give `dirty_worktree_requires_force`; the binary never passes `--force` and prints herdr's message unchanged. The branch is kept after a removal. |
| `rsync -rt` behaves the same with macOS's rsync and GNU rsync (symlinks skipped without `-l`) | **Holds on macOS.** `openrsync` (protocol 29) prints `skipping non-regular file` for file and directory symlinks, copies the rest and exits 0. The binary also walks the library with `lstat` first, so a partial copy is reported without parsing rsync's output. GNU rsync is checked on the remote machine in stage 6. |
| The working directory herdr reports for a pane does not change when the agent changes directory | **Holds for claude.** A shell's `cd` does change `cwd`, but claude running `cd /usr/local && pwd` left the agent's reported `cwd` unchanged. Other agent kinds are untested; if one changes it, that thread shows as `pane closed` rather than being acted on wrongly. |

Also learned:

- **`worktree open` needs `--cwd <repo>`**; with `--path` alone it answers `worktree_not_found`.
- **herdr puts worktrees under `~/.herdr/worktrees/<repo>/<branch-with-hyphens>`**, and `worktree create` also opens a workspace for the main repository when none is open. The binary records the path herdr returns and never builds it.
- **Excluded files do not protect a worktree from removal.** With only `.herdr-project/` (listed in `info/exclude`) present, `worktree remove` succeeds and deletes it. This is why `--remove-worktree` requires a complete final copy.
- **Every new worktree shows claude's trust dialog** on this Mac, because `~/.herdr/worktrees` is not under a trusted parent. So under the defaults every worktree thread begins as `prompt_pending`, shows under Waiting on you after 60 s, and needs one Enter in its pane. **Tab threads do not** (their folder is under the project folder, which the user trusted when opening the coordinator). After the dialog, claude's ordinary first-edit and first-command prompts show the thread under Waiting on you after 30 s each, as the plan expects. The README says so.
- **A failed `git worktree add` can leave the branch behind** (git creates the branch before the directory). `thread restart` then takes case (b) and asks for a human look, as designed.
- **Allow-list patterns of the form `Bash(<binary> --root <root> thread prompt:*)` do suppress claude's prompt for the here-document form** (`--text-file - <<'TEXT' … TEXT`), in manual permission mode. An off-list subcommand (`ticker status`) met the permission prompt in the same session. The `scratch/` fallback is not needed.
- `thread start` returned in about 1 s; the ticker started the agent on the next tick (under 15 s).

## Stage 4 (2026-09-17, herdr 0.9.1)

| Assumption | Result |
| --- | --- |
| `pane report-metadata` tokens can be used in `agent.view.set` filters | **Holds.** The schema's `AgentViewField` and `AgentViewSortField` both accept `{"token": "<name>"}`. A request with filter `{"op":"eq","field":{"token":"project"},"value":"demo"}` and sort `[{"field":{"token":"rank"},"order":"asc"}]` was accepted: `{"type":"agent_view","active":true,"source":"herdr-projects","label":"demo"}`. |

Also learned:

- **There is no CLI command for `agent.view.set` or `agent.view.clear`** in herdr 0.9.1 (`herdr agent` has no `view`; `herdr api` has only `snapshot` and `schema`). They are socket-only. This conflicts with plan decision C6. **Client decision during the build (2026-09-17): a narrow exception.** `Runner::socket_request` writes one JSON line (`{"id","method","params"}`) to the project's recorded socket and reads one line back, and only `focus` and `unfocus` use it. Everything else stays on the CLI.
- **The active agent view is not in `api snapshot`** (its keys are `agents`, `panes`, `tabs`, `workspaces`, `layouts`, focus ids, `protocol`, `version`). So "`focus demo` shows only the project's panes" cannot be read by the builder and is client-witnessed; see `docs/manual-test.md`. Pane tokens are in the snapshot and were read directly.
- A report written in a tick is counted for the thread's group in the same tick (the copy home follows in the slow pass); otherwise a finished thread showed as Idle for one tick before Ready for review, which in stage 5 would have produced a spurious inbox item.

## Stage 5 (2026-09-17, herdr 0.9.1)

No rows of the assumptions table belong to this stage. What the live runs showed:

- **An agent still reads as `idle` in the tick that delivers its prompt.** Computing the group in that tick produced a spurious "now Idle" inbox item before the real "new report" item. The delivering tick now counts as Working; a live re-run gave exactly one item and one nudge.
- With `nudge = true` the full loop works: item written, one nudge (`[herdr-projects ticker: automated, not the user, approves nothing] New inbox items. Run context.`), the coordinator ran `context`, read `threads/<id>.md`, ran `inbox done`, and no further nudge followed. `nudge` stays `false` by default (stage 2 finding); then the ticker calls `herdr notification show` once per set of unseen items, with a count only.
- `routine approve` with standard input not a terminal refuses. Run inside a herdr pane (a real terminal) it printed the command and the warning, took the typed name, and wrote `approved-routines.json`. The command did not run with `routine_commands = false`, did not run when enabled but unapproved, ran once approved (item body: the prompt plus the fenced, labelled output), and stopped again when the command text was edited (one new `routine-approval` item).
- A second session (`hp-dev2`) with the plugin linked ran `[[startup]]`, found a ticker of the same version and started nothing; it produced no inbox items and did not disturb the open thread.
- Pull request behaviour is covered with the scripted fake `gh` only (merged resolves after the final copy; a comment gives an item with no body; a foreign branch or repository is ignored once; a bad `PR:` line never reaches `gh`; one outage item and one recovery item). A run against a real pull request was not done: it needs a push and the client's go-ahead.
- The tests caught one real bug: auto-resolve dropped the "time since this ticker started" term when it was zero, so a freshly started ticker resolved week-old idle threads at once.

## Stage 6 (2026-09-17, herdr 0.9.1 on both machines)

The second machine, `elias-macbook-pro-m1`, runs Linux (aarch64), herdr 0.9.1, GNU rsync 3.5.0, git and claude. The client approved `herdr machine add` against its **default** session, and approved copying the source there, building it and linking the plugin, with the test project in a throwaway `hp-dev` session on that machine.

| Assumption | Result |
| --- | --- |
| `herdr --machine M worktree create` and `agent start` work and return the remote worktree path | **Holds.** `worktree create` returns `worktree.path` and `root_pane.cwd` as they are on the remote (`/home/…/.herdr/worktrees/<repo>/<branch>`); `agent start`, `agent prompt`, `agent list`, `pane list`, `pane send-keys`, `pane report-metadata` and `api snapshot` all work through `--machine`. |
| `herdr machine list --json` exposes an SSH target | **Holds.** Each entry has `id`, `label`, `target`, `session`, `enabled`, `selected`. `[machines.<label>] ssh` in `config.toml` remains the fallback. |
| `herdr --machine M worktree open` works for `thread restart` on a remote thread | **Holds** (with `--cwd <repo>`, as locally). `worktree remove --workspace` also works remotely and keeps the branch. |
| `rsync` is present on this Mac and on the remote machine | **Holds.** openrsync here, GNU rsync 3.5.0 there; a library file came home over `rsync -rt -e ssh`. The full test suite, including the symlink-skipping copy test, also passes on the Linux machine, which completes the stage 3 rsync row for GNU rsync. |
| Setting `HERDR_SOCKET_PATH` to the recorded local socket does not break `herdr --machine M ...`, and `--machine` targets the remote's default session | **Holds.** With `HERDR_SOCKET_PATH` set to the `hp-dev` socket, `herdr --machine M workspace list` listed the remote default session's workspaces. **But `--machine` cannot be combined with `--session`** ("--machine cannot be combined with other launch options; it uses the saved machine's session"), so the binary's env-variable form is the right one; the remote session is whatever `machine add --remote-session` saved (default here). |
| Remote thread panes appear in the local sidebar, carry pane tokens, and are included by `focus` | **Not as written; fallback taken.** Tokens reported through `--machine` are set on the *remote* server's panes (seen in `herdr --machine M api snapshot`), and the local server's snapshot contains no remote panes. `focus` installs its view on the project's local server only. So remote threads are in the text `overview`, in `thread list` and in inbox items, and their tokens show when the user looks at that machine; `focus` does not cover them. The README says so. Whether herdr's connected-machines sidebar shows the tokens is client-witnessed (`docs/manual-test.md`). |
| `herdr --remote <target>` attaches to a herdr server on the other machine, and a user can reach a blocked remote pane through it | **Holds.** From a pane on this Mac, `herdr --remote elias-macbook-pro-m1 --session hp-dev` showed the remote project's workspace and its blocked coordinator; `Down`, `Enter` typed into that client answered claude's trust dialog on the remote, and the remote ticker then delivered the priming prompt. Without `--session` it attaches to the remote default session, where a remote thread's pane and agent (`hp-demo-t-0009`) were visible. |

Also learned:

- **A remote thread ran end to end**: `thread start --machine` returned in 4 s (one ssh call for origin and base, one `herdr --machine worktree create`, one ssh call for directory, `info/exclude` and brief); the ticker launched the agent at the next remote poll; after the agent finished, the report and a library file were in the home project and the thread showed Ready for review about 70 s later.
- **A title of `Remote "hello" $(touch /tmp/hp-pwned2) it's` reached the remote `--label` unchanged and ran nothing**; the branch became `hp/demo/t-0009-remote-hello-touch-tmp-hp-pwned2-it-s`.
- **Outages** were simulated with an `ssh` shim first on the ticker's `PATH` (it fails like an unreachable host while a flag file exists); `herdr --machine` uses `ssh` from `PATH`, so this covers both herdr and the binary's own calls without touching the other machine or the client's ssh config. About 100 s of failure with the default threshold: no inbox item, no group change, one logged failed poll, and a local thread started during it had its agent launched 15 s later. With `HERDR_PROJECTS_OUTAGE_SECS=60`: exactly one `outage` item (after the second failed poll), still one after 2.5 more minutes down, then one "reachable again" item at the first successful poll.
- **The always-on recipe works**: the plugin built on the Linux machine (`cargo build --release --locked`, 12 s) and all tests pass there; a project opened in a session on that machine has its own ticker there and needs nothing from this Mac. Two things to document: (1) `herdr plugin link` fails with `plugin_requires_newer_herdr` while that machine's *running server* is still 0.9.0, even though the CLI is 0.9.1 — restart the server after upgrading; (2) attaching with `herdr --remote` to a server that was started by hand over ssh asks whether to restart it ("may not survive SSH connection loss"); answer `n` to keep its panes.
- The source was copied to `~/dev/herdr-projects` on that machine with rsync (there is no GitHub repository yet) and the plugin is still linked there.

## Stage 7 (2026-09-17, herdr 0.9.1)

| Assumption | Result |
| --- | --- |
| A plugin action can open a plugin pane with `herdr plugin pane open`, and the action (not the popup) receives the originating pane in `HERDR_PANE_ID` or `HERDR_PLUGIN_CONTEXT_JSON` | **Holds.** An action's environment has `HERDR_PANE_ID`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_SOCKET_PATH`, `HERDR_SESSION`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ACTION_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_BIN_PATH` and `HERDR_PLUGIN_CONTEXT_JSON` (`workspace_id`, `workspace_label`, `workspace_cwd`, `tab_id`, `tab_label`, `focused_pane_id`, `focused_pane_cwd`, `focused_pane_agent`, `focused_pane_status`, `invocation_source`, `correlation_id`). `herdr plugin pane open --plugin herdr-projects --entrypoint <id>` from an action returns `{"type":"ok"}` and the popup's command runs. **The popup does not get `HERDR_PANE_ID`, `HERDR_TAB_ID` or `HERDR_WORKSPACE_ID`** (it does get the context JSON), which is why the action captures the pane and hands it over in `handoff.json` in the plugin state directory. |

Also learned:

- **Popups are not panes in `pane list`**, and the throwaway session has no client attached, so the builder cannot type into a popup. The three interactive popups (`new`, `pick`, `adopt`) are client-witnessed (`docs/manual-test.md`). What was checked instead: the actions write the right handoff and open the right entrypoint (unit tests and one live `plugin action invoke adopt-workspace`, which captured pane `wG:p1`, the workspace label and its directory), and the popup's core ran live through `adopt-workspace --name … --pane … --workspace-cwd …`: project created, opened in the same session, pane adopted as `t-0001`.
- **claude's trust follows the git root, not only the parent folder**: a fresh `git init` inside the trusted `~/dev` tree still showed the trust dialog. An agent adopted while blocked on it got `prompt_pending = true`, and the ticker delivered the brief line once the dialog was answered.
- `plugin action invoke` takes its context from the session's focused pane (`invocation_source: "cli"`), so actions can be exercised from the CLI.
- After `delete --force` the ticker did not recreate the project folder (checked 35 s later): writers re-check `PROJECT.md` after taking the lock.

## Stage 8 (2026-09-17)

- The development `[safety]` table and routine approval were removed; `~/.config/herdr-projects/` held nothing else and was removed. The `hp-dev` session, the scratch root `.dev-root`, the scratch repositories and their worktrees were removed. `~/.herdr-projects` was never created.
- `delete demo --force` moved the project to `.trash/` and left the scratch repository's thread branch in place.
- Left in place for the client to decide: the plugin is linked on this Mac and on the second machine (source at `~/dev/herdr-projects` there), and `elias-macbook-pro-m1` is a saved herdr machine.
- The README follows the structure of the client's `herdr-call` and `herdr-agent-progress` READMEs at the client's request; the detail the plan asked the README to carry (symlink, allow-list, soft-guard and routine warnings, the `unfocus` note) is in `docs/getting-started.md` and `docs/operations.md`, with the warnings summarised in the README's questions. The `/landingpage-readme` skill can only be run by the client, so it was not used.

## After the build (2026-09-18, herdr servers restarted on 0.9.1 on both machines)

- With both default servers on 0.9.1 the plugin loads in them: all nine actions are listed on this Mac and on the second machine, `plugin link` works there without a named session, and no ticker runs on either (no projects yet).
- **A herdr server not started from a login shell gives plugins a minimal `PATH`.** The `doctor` action in this Mac's default session reported `gh` as not installed although it is at `/opt/homebrew/bin/gh`. A ticker started by `[[startup]]` would have silently skipped pull request follow-up. The binary now appends `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin` and `~/.cargo/bin` to its own `PATH` at startup; the same action then reported `gh` and `gh auth` as ok.
- `eliasstravik/herdr-projects` was created as a private repository with the `herdr-plugin` topic. The name had been a redirect to `herdr-tracker` (that repository's earlier name); creating the new repository replaced the redirect. No local clone used the old URL.

## The 0.2.0 redesign (2026-09-23, herdr 0.9.1)

- **`pane current --current`** returns `pane_id`, `terminal_id`, `agent` and `agent_session` (`{agent, kind, source, value}`) for the calling pane; `agent get` and `agent list` also carry `state_change_seq`. The pane id works as the progress binding; the terminal id tells a reused pane id after a restart from the old pane.
- **`pane report-metadata` answers success with an empty body.** The CLI wrapper treats an empty successful reply as success.
- **An agent started as a child process is detected** (0.2.2, `open` in a shell pane): Herdr names the kind, state and session as for `agent start`. `agent list` then reports the shell's directory as `cwd` and the directory of the pane's foreground process-group leader (here `herdr-projects` itself, not the agent) as `foreground_cwd`, so `open` enters the project home before it starts the agent, and coordinators match on either field.
- **A pane from `workspace create` is not an available shell for a moment**: `agent start` returns `agent_pane_busy`. `open` retries for up to ten seconds.
- **Native resume needs a client.** After a server restart, a headless session resumes Claude panes only once a client attaches (`script -q /dev/null herdr --session <name>` is enough). In this run the resumed agents kept their names; the ticker still renames an unnamed one.
- **`worktree remove --workspace <ws>`** removes the checkout, closes the linked workspace and keeps the branch.
- **`workspace close`** without `--group` closed a repository's primary workspace when none of its linked worktrees were open.
- **`herdr config check`** reads the file named by `HERDR_CONFIG_PATH`, so `configure` validates a candidate before writing the real config.
- **`herdr --default-config`** lists the built-in keys as commented `# action = "key"` lines under `[keys]`; `prefix+a` is free in 0.9.1.
- **`--display-agent`** changes the agent name a row shows; whether `--title` shows in any row token was not verifiable without a client, so it is not used.
- **The globally linked plugin runs its `[[startup]]` in every session**, scratch ones included: after restarting a scratch server, the main checkout's ticker replaced the development ticker until it was restarted.
- **Claude Code's `--settings <file>`** loads extra hooks, which is how the progress hooks were tested without touching `~/.claude/settings.json`.
- **Codex hangs without a terminal** on this Mac (`codex --version` included), so Codex coordinators and threads were started but not exercised.

## `update` (2026-09-23, herdr 0.9.1)

Checked against a throwaway Herdr server with its own `HOME`, never the default session.

- **Re-running `herdr plugin install OWNER/REPO` on an installed plugin updates it in place.** The preview ends with `replaces: herdr-projects from github:…@<old ref>`. Herdr clones into `plugins/.tmp-install-*/checkout`, runs the build there and swaps it in only when the build passes: the plugin root (`plugins/github/<repo>-<hash>/`) keeps its path, so `~/.local/bin` links, hooks and `AGENTS.md` paths stay valid. No uninstall is needed.
- **A failed build leaves the old install as it was**: `Plugin was not installed.`, exit code 1. A missing `--ref` fails the same way, before anything is touched.
- The build runs in the CLI process, with the caller's environment (`CARGO_HOME` and the like). `plugin install`, `plugin list` and `plugin uninstall` go to the server named by `HERDR_SOCKET_PATH`.
- `plugin list --plugin ID --json` names the install type: `source.kind` is `github` (with `owner`, `repo`, `requested_ref`, `resolved_commit`, `managed_path`) or `local`, and `plugin_root` is the folder. The managed clone is a git checkout, detached at the fetched commit, with `origin` set to the GitHub repository.
- `plugin link` does not build; `herdr plugin link` has no `--yes`.
- Releases are the `vX.Y.Z` tags (GitHub releases). `update` reads them with `git ls-remote --tags --refs origin` in the plugin root, which works for both install types, and installs the newest tag with `--ref`. A release must be tagged for `update` and `doctor` to see it.
- On macOS, copying a new binary over the file of one that has run gets the next run killed (exit 137, code signature cache). Cargo and Herdr's swap both write a new file, so neither is affected.

## Native Windows port

Coordinator evidence was recorded on 2026-10-04; additional native observations follow. These runs used Windows 11 x64 with **native Herdr 0.9.3** and **oh-my-pi 18.5.1**, not WSL:

- A coordinator ran with `HOME` unset (`USERPROFILE` supplied the native home), using Unicode and apostrophe-containing paths. It executed the printed PowerShell `skill` and `context` commands and replied **`WINDOWS_COORDINATOR_READY`**.
- The scratch Herdr config used `[terminal]` with `default_shell = "pwsh.exe"` (PowerShell 7). The run used the user's existing OMP model/auth/skills; they were not replaced with a clean configuration.
- A real Git-worktree OMP worker launched once with one brief, executed the printed PowerShell progress command and replied **`WINDOWS_WORKER_READY`**; native metadata showed 100% and `Done`. Failed tab thread `t-0001` was restarted through the CLI, launched OMP with one brief and also reported 100%/Done. Its raw verbatim-prefix working directory was retained successfully. Both actual workers completed; actual forwarded prompt/Next interaction was not exercised.
- Actual `focus` and `unfocus` commands passed through native named-pipe IPC. The native popup rendered in Herdr's ConPTY with scoped `Idle (2)` and both workers; `i` showed `t-0001` detail, Esc returned to the list and then closed it. This is terminal-rendering/navigation evidence, not a pass for all sidebar, notifications or popup-key handoff checks.
- Under Windows PowerShell 5.1, a verified download/SHA/version installation succeeded while a resident old-image PID stayed alive: its executable was renamed aside, the verified new image was installed, and the installer truthfully reported the old image still in use. A bad SHA and a correct-SHA Herdr 0.9.3 executable were both rejected; source fallback with Cargo unavailable failed, leaving the installed SHA unchanged in both cases.

The actual `link-command.ps1` managed command copy ran `--version`; its ownership hash and future installed-source target were correct. A tampered managed copy and a foreign file were both preserved.

Actual `thread resolve` for `t-0002` copied the report and Unicode/smart-quote artifact home. Reading them confirmed exact `WINDOWS_NATIVE_COPY_READY` and `WINDOWS_NATIVE_ARTIFACT_READY—café’s` contents. Dirty tracked `fixture.txt` caused native Herdr's `dirty_worktree_requires_force` refusal; the worktree and branch were retained without forced cleanup.

Project `native-lifecycle-smoke` was created, renamed to `native-renamed` and deleted to trash successfully with persistent root-level namespace locks. Doctor confirmed the generated `AGENTS.md` and regular `CLAUDE.md` copy.

The native Herdr 0.9.3 server restarted under isolated XDG after the controlling Eval kernel exited, restored existing state and the user's OMP config, and both actual OMP workers completed. Native Herdr accepted the Windows build/pane manifest. The actual extensionless `pane.projects` command ran in Herdr's ConPTY and rendered all project threads; the exact `target/release/herdr-projects startup` vector exited 0, published native ticker PID/version and was then stopped.

After restoring the tracked fixture with correct native CRLF and closing its workspace, a clean orphan worktree sweep removed 1 and kept 0, without force. Actual configure/unconfigure in a fake home created hooks and NTFS skill junctions under `.claude` and `.agents`; removal left both links absent and the bundled source `SKILL.md` intact. The user's actual configs were untouched.

An isolated native linked checkout's `update --check` discovered installed 0.2.34 and latest 0.2.36 on a local origin. Update performed the pull and invoked the real Windows PowerShell installer. The intentionally invalid release fixture failed before compilation, leaving the old executable SHA unchanged and truthfully reporting failure/recovery. This verifies update failure preservation, not installation of a genuinely newer Windows release.

Final native **`cargo test --locked` passed 364 tests: 359 unit tests and 5 real CLI tests**. Native coverage includes copy cap/growth/deadline and hardlink/junction safety, persistent lock/waiter behavior, priming backup conflicts, pipe cancellation and process-tree cleanup. The scrubbed PowerShell CLI fixture needed standard `PATHEXT` to classify `.exe` as a native application; differential probes confirmed that on both Windows PowerShell 5.1 and PowerShell 7. The fix was confined to the fixture environment, not the user's configuration.

Host TUI tab-bar/sidebar pixel rendering, interactive popup mouse/key handoff and a successful genuinely newer Windows release install were not exercised. No Unix runtime was run during the initial local port verification; that historical limitation is superseded by the hosted Linux evidence below, not by a new macOS claim.

The port implements native named-pipe IPC (`herdr.sock` is a regular `notUnixSocket` liveness marker), encoded `pwsh.exe` hook/tab-bar commands, NTFS skill junctions and synchronized regular `CLAUDE.md` copies without symlink privileges. Priming ownership is an exact current/prior `AGENTS.md` byte match; a foreign `CLAUDE.md` with an existing backup produces a conflict and preserves both. Hook configure/unconfigure preserves foreign sibling commands in mixed groups. Local report/hash/library reads require singly linked regular opened handles; the library copy enforces a shared 50 MiB actual-transfer budget and a 60 second preflight/copy deadline, keeping reports/worktrees on partial or failed copies. Windows artifact copies remain writable without clearing destination-hardlink attributes; Unix permissions are preserved. Local copying uses neither `rsync` nor `du`; SSH scripts and remote copying still require POSIX tools. Project locks are persistent root-level `.project-<slug>.lock` tokens, and ticker metadata is atomically published in `.ticker.info` separately from `.ticker.lock`. Stop an old Unix ticker with its old binary before upgrading to this layout.

The checkout's Windows installer stages and validates a source build or verified release candidate, replaces a running image by rename and attempts rollback on replacement failure. Its SHA-256 calculation uses .NET rather than depending on a PowerShell module import, including when module paths are inherited across Windows PowerShell 5.1 and PowerShell 7. The terminal command is a managed `.exe` copy with a source/SHA-256 marker; unrelated or modified targets are preserved. The observed failure checks establish candidate rejection and old-image preservation, not every possible filesystem replacement/rollback fault.

The reviewed native port and upstream-sync workflow were committed as `51d37d1` and pushed to the [ubranch fork](https://github.com/ubranch/herdr-projects)'s `main`; the local checkout is now on `main`. Windows source installation and local plugin linking became available from that published branch. At that source-publication checkpoint, the Windows MSVC release workflow existed, but no Windows release asset had yet been published.

The enabled **Upstream sync** workflow runs on a best-effort 15-minute UTC schedule (`:07`, `:22`, `:37`, `:52`) or manual dispatch. Only merged upstream `main` code is eligible for candidate testing/promotion; open branches and PRs are tracked without merging, executing or installing their unmerged code. Candidate bundles must pass formatting, strict all-target Clippy, locked tests/builds and real CLI smoke in the read-only Windows MSVC and Ubuntu Linux gates before a separately guarded write-only publisher can promote them. Conflicts, failed gates or changed state stop publication; there is no force/reset promotion.

The [first hosted run](https://github.com/ubranch/herdr-projects/actions/runs/37242425743) passed preparation (including real-Git regression coverage) in 11 seconds. Linux passed locked full tests, a debug build and actual CLI `--version`/`new`/`list`/`context` smoke in 38 seconds. Hosted Windows had 358 unit passes and one failure: the cold PowerShell process-tree fixture had not launched its descendant before the three-second timeout. Publication was correctly skipped.

The corrected Windows fixture re-executes the native test binary instead of waiting for PowerShell startup. A retained PID/creation-time-matched process handle proves the descendant is alive before the unchanged three-second timeout and terminated afterward. The targeted native scenario and full **364-test suite** passed locally; the first hosted run above did not establish Windows success.

### Lint and native release readiness

The source cleanup passed `cargo +stable fmt --all -- --check` and `cargo +stable clippy --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings` on native Windows with Rust 1.98.1. No lint suppressions or skipped targets were added. The full native suite still passed **364 tests**. A focused review found no correctness/security findings in the launch allowance, Windows path/UTF-16/ownership changes or CI trust boundary.

`cargo +stable build --release --locked` produced the optimized native `target\release\herdr-projects.exe`. Its actual version, project creation, listing and context commands passed in a temporary Unicode/apostrophe path with `HOME` unset and an isolated `USERPROFILE`. The debug CLI additionally exercised pause/refused thread start/resume/rename and path-traversal refusal. These source checks do not install, reload or restart the plugin in existing Herdr sessions.

The [earlier hosted run](https://github.com/ubranch/herdr-projects/actions/runs/37244454852) passed Windows/Linux tests, debug builds and real CLI smoke. The updated workflow also requires formatting and strict Clippy; that earlier run is not evidence for these new lint gates. Read the current Actions run for new hosted results. Windows release assets were unpublished at that checkpoint; a local release build is not a published release or proof of every interactive host UI feature.

## Extended native verification (2026-10-05, local working source)

The earlier port observations above remain historical. The reviewed working source passed native Windows formatting, strict all-target Clippy and **381 tests**. Actual Mac verification passed strict all-target Clippy, **366 tests**, a debug build and the newly built CLI's project creation, goal/parallelism changes and `context --peek`; exact `PROJECT.md` values were checked. These are local source results, not a newly published release or new hosted CI claim.

The Mac run exposed eight Unix-only no-op socket-key conversions and CRLF in two shell scripts despite the existing `*.sh text eol=lf` rule. The owned conversion now moves Unix strings without copying and preserves Windows path-component identity; borrowed keys use the existing platform identity. Only `link-command.sh` and `install.sh` line endings were normalized. The actual Mac linker changed from `set: -\r: invalid option` to publishing the exact intended future symlink in a private fake home. The failed receipts were retained; previously successful Mac formatting/Clippy were reused only after proving every other source byte and archive mode unchanged.

| Boundary | Actual additional proof |
| --- | --- |
| Project writers | Two real native setting processes blocked behind the persistent OS lock, then both succeeded; the Unicode/apostrophe goal and parallelism value survived together. Rename/trash retained the lock tokens. |
| Cleanup ownership | Genuine Git allocations and native reopen/resolve transitions proved that cleanup rechecks current permissions and new ownership. Checked-out and moved-tip refs survived; eligible clean allocations and an approved unchanged ref alone were removed. Initial stale records and merge-permission inputs were explicitly synthetic, not live provider/GitHub claims. |
| Managed commands | The Rust manager, PowerShell 7 and Windows PowerShell 5.1 blocked behind the same signed command token. Publication matched the source hash; a read-only marker failure restored the old bytes/attributes, and foreign or modified targets/tokens stayed intact. |
| Persisted progress | Twenty-three installed native CLI invocations exercised explicitly synthetic historical raw-hash records: equivalent socket selectors, session/terminal collisions, newer/older records, the one-time marker and foreign/malformed data. Forty-three observed checks passed; no fake provider or terminal identity was presented as live evidence. |
| TASKS grammar | With clearly labeled synthetic task/profile/route inputs, 119 installed native CLI invocations proved owner parsing, precedence, boundaries and note association. All 139 assertions passed without starting workers or paid prompts; successful dispatch is a separate live check. |

The actual attached Windows ConPTY memory viewer also displayed an owned 240-line document. Enter, Down/Up, PageDown/PageUp, Home/End and repeated end-clamping matched eight observed viewport first lines. Native `y` copied the exact document path without a newline and restored all four previously saved clipboard formats. This proves native terminal cells and interaction, not a desktop pixel screenshot.

The existing Windows installer then performed one staged optimized source build and installed `0.2.34+da073ca.1791214154`. Both mapped previous images were retained rather than overwritten. The installed CLI ran successfully, and a version-aware restart replaced only the owned Main ticker with that same version. The Beta ticker and remote worker were not restarted; the tested saved Mac SSH profile had already been removed at the user's request.

### Published release checks

The first fork release, `v0.2.35`, built and published all five binaries plus `SHA256SUMS` in [run 37361099552](https://github.com/ubranch/herdr-projects/actions/runs/37361099552). Native formatting, strict Clippy and tests passed on Windows, macOS ARM, and both Linux runners; Intel macOS was cross-built. The actual macOS release installer and CLI smoke passed. Windows also downloaded and installed the published binary and created the Unicode-path project, but Python's CP1252 stdout failed while printing its output. This was a CI logger failure, not a failed binary installation.

The `v0.2.36` patch runs the release and upstream-sync CLI smoke loggers with `python -X utf8`, preserving the Unicode test path rather than suppressing encoding errors. The published `v0.2.35` tag remains unchanged. Verification of the patch release is recorded by its own Actions run and release assets.

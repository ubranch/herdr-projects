# Operations and development

How Herdr Projects works, what it writes where, what its safety settings do and don't stop, and how to run threads on other machines.

## How it works

- **It relies on Herdr and nothing else.** No other plugin is needed or called. Pull requests open in your browser, text files open in a new Herdr tab running `$EDITOR`.
- **A project is a folder.** `~/.herdr-projects/<slug>/` holds `AGENTS.md`, which tells any agent started in that folder that it is the coordinator and which commands to run. `CLAUDE.md` is a relative symlink on Unix and a synchronized regular copy on Windows. Several coordinators can share the folder.
- **The coordinator is an ordinary agent** following a skill (`herdr-projects skill` prints it). Plugin code does not route messages, plan work or decide anything.
- **The binary does mechanics.** Starting a thread, copying reports, cleaning up after a resolve: each is one deterministic subcommand. It talks to Herdr through Herdr's CLI. The exception is the agent view (`focus`, `unfocus`, the default sort): Herdr has no CLI for `agent.view.set`, so those send one JSON line through a Unix socket or, on Windows, Herdr's native Win32 named pipe.
- **Agents report their own progress.** `herdr-projects report --percent N --activity "..."`, run by the agent in its pane, writes one small JSON file per pane under `<root>/.progress/` which the ticker shows on the pane's sub-line for five minutes. Thread briefs and the coordinator skill carry the instructions, so any agent reports; hooks in Claude Code, Codex, Droid, Gemini CLI and Copilot CLI (installed by `configure`) also inject them and a reminder. There is no daemon and no database.
- **Files are the record, prompts are nudges.** Threads write a report file, the ticker writes events to an inbox folder, and the coordinator reads state with `context` at the start of every turn. A missed prompt loses nothing.
- **One ticker per projects root** checks every 15 seconds: coordinators (any agent in a project folder, also one started by hand in a project never opened), thread state and groups, sidebar tokens and the per-project grouping of agents and Spaces, pending prompts, changed reports, pull requests (every two minutes), routines, auto-resolve, notifications. Remote machines are polled once a minute.
- **Tools are found even under a bare `PATH`.** The binary appends Unix tool folders (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin`) and the home `.local/bin` and `.cargo/bin`, never ahead of your existing entries. Windows also appends `%APPDATA%\npm` and `%LOCALAPPDATA%\Microsoft\WinGet\Links`. Herdr, Git, PowerShell 7 and your agent still need to be installed and discoverable; local copying does not require `rsync` or `du`.
- **Cleanup is part of the flow, never forced.** Resolving a thread removes its worktree (Herdr and git refuse a dirty one, and the plugin never forces) and, once its pull request is merged, its local branch. Reports and library files always stay. Text from reports, pull requests and command output is never placed in a prompt.

Native Windows local commands target PowerShell 7, including approved routine commands and the commands printed in briefs and `context`; keep the leading `&` before a quoted executable. Set Herdr's `[terminal] default_shell = "pwsh.exe"`. Hooks and tab-bar commands use encoded `pwsh.exe` wrappers, surviving CMD's extra parsing layer. Install/update scripts run through `powershell.exe`. SSH scripts and remote paths remain POSIX; a Windows port does not make a remote Windows shell a supported SSH target.

On Windows, the `socket_path` returned by `herdr session list --json` still names `herdr.sock`, but that file is a regular liveness marker containing `notUnixSocket`. Direct requests use `\\.\pipe\<socket_path>` with the exact host-reported path string, not a canonicalized path or an address read from the marker. Pipe reads/writes have a bounded deadline and cancel outstanding overlapped I/O on timeout. Unix retains Unix-domain sockets.

Windows session/ownership comparisons accept equivalent separator, case and verbatim-prefix spellings without changing the address used for named-pipe requests. On first CLI initialization, a serialized, persistent progress migration recognizes previously written socket hashes and retains terminal/session boundaries. Conflicting, invalid or foreign entries are preserved rather than silently merged or overwritten.

Local report/library copies use native filesystem operations on both platforms. Source reports, hashes and library files are read only after validating the opened handle as a singly linked regular file: source symlinks, Windows junctions and hardlinks are not followed/copied. Destination symlinks/junctions and overlapping source/destination trees are rejected. Each regular destination file is replaced atomically rather than written through, so an existing destination hardlink cannot modify its other names.

The library cap is **50 MiB (52,428,800 logical bytes)**, not disk allocation. Preflight counts eligible files; an oversized library is skipped. Copying also shares one actual-transfer byte budget across all library files, so growth after preflight cannot bypass the cap. Exactly the cap is allowed; exceeding it returns a partial copy, discards the current staged file and leaves already-published files/report intact. A 60 second deadline covers library preflight and copying; timeout is a failed copy. The tree copy is not one transaction, and a partial/failed copy prevents automatic worktree removal unless you explicitly discard the uncopied files.

Managed Windows artifact copies stay writable; Unix copies preserve source permissions. The copier never clears an existing destination's read-only attribute through a hardlink. A read-only destination that cannot be replaced causes a copy failure, preserving the already-copied report and the worktree.

Cleanup confirms that the worktree folder is actually gone before clearing its record. In the exercised Windows Git build, an ignored junction-containing worktree could remain after Git reported success; the plugin retained the directory and recorded path rather than force-delete it. `--discard-uncopied` accepts copy loss, not dirty-worktree removal or unsafe filesystem cleanup.

## Where things live

```
~/.herdr-projects/<project>/
  PROJECT.md              settings (TOML between +++ lines) and your standing instructions
  AGENTS.md, CLAUDE.md    who is the coordinator, by working directory; written by the binary
  MEMORY.md, memory/      project memory; the coordinator's
  TASKS.md                the task list, each task with optional indented notes; the coordinator's
  routines/<name>.md      routines, including pr-followup.md; the coordinator's
  uploads/                files you give the threads
  scratch/                the coordinator's temporary files
  threads/<id>.toml       thread record          threads/<id>.md       home copy of its report
  threads/<id>.task.md    the task and every forwarded prompt (## Follow-ups)
  threads/<id>.next.md    Next lines the coordinator added    threads/<id>/  a tab thread's folder
  inbox/, inbox/done/     events for the coordinator
  library/<id>/           home copy of files a thread produced
  .state/                 status, coordinator record, live coordinators, ticker state
~/.herdr-projects/.project-<slug>.lock          persistent per-project lock token
~/.herdr-projects/.ticker.lock  .ticker.info  .ticker.log  .progress/  .trash/
~/.config/herdr-projects/config.toml             yours: root, profiles, safety tables, machines
~/.config/herdr-projects/owned.json              what `configure` changed, for `unconfigure`
~/.config/herdr-projects/approved-routines.json  written only by `routine approve`
```

Here `~` is `HOME`, falling back to `USERPROFILE` on native Windows. Herdr Projects keeps its own config in `~/.config/herdr-projects` on both platforms; Herdr's Windows config/runtime directory is `%APPDATA%\herdr` (`XDG_CONFIG_HOME` wins when set), or `HERDR_CONFIG_PATH` for its config file.

Per-project lock tokens live outside the project folder so rename/delete can move that folder on Windows. `.project-<slug>.lock` and `.ticker.lock` are persistent tokens: **never remove them while processes or waiters can exist**. Removing one can split waiters across different files. A token's existence is not evidence that its OS lock is held. Ticker PID, version, start time and tool paths are atomically published in `.ticker.info`; `.ticker.lock` no longer contains readable metadata. Before upgrading from the old Unix layout, run `ticker stop` with the old binary, then install, `doctor --fix` and `ticker start`. The Windows ticker starts detached; timeout-controlled native subprocess groups use process-tree termination rather than Unix signals.

Every thread works from `<its working directory>/.herdr-project/<project>-<id>/`: `brief.md` (written by the binary), `report.md` and `library/` (written by the agent). In a git repository that folder is in `info/exclude`, so nothing in it is committed. Git therefore treats it as clean and removing a worktree deletes it, which is why a resolve keeps the worktree when the final copy home was partial.

`PROJECT.md` settings, all changeable from the popup's settings section, from chat, or with `herdr-projects set <project> <key> <value>`: `name` (the workspace label), `goal`, `repos` (`repos.add PATH[@MACHINE]`, `repos.remove PATH`), `coordinator_profile` and `thread_profile` (the default [profiles](#agent-profiles); the old names `coordinator_agent` and `thread_agent` still read), `max_parallel_threads` (10), `auto_resolve_days` (7), `nudge` (`true`), `mute` (`false`).

## Commands

| Command | What it does |
| --- | --- |
| `new <name> [--goal] [--repo PATH[@MACHINE]]... [--thread-profile P] [--coordinator-profile P]` | Create a project folder. The profiles default to `[defaults]` in config.toml, else `claude`. |
| `open <project> [--profile P] [--new] [--tab] [--session N \| --socket P] [--rebind]` | A coordinator agent in the project folder; focuses a running one. From a shell pane inside Herdr it runs in that pane and quitting it returns to the shell; `--tab`, the popup, actions and a terminal outside Herdr use a tab of the project's workspace. |
| `context <project> [--peek]` | The digest the coordinator reads every turn. |
| `assignable <project> [--refresh] [--check OWNER]` | Who TASKS.md tasks may be assigned to (see [Task owners](#task-owners)); `--check` refuses an owner that is not valid. |
| `coordinator prompt <project> --text-file F` | A sentence to the coordinator (the popup's task keys use it). |
| `thread start <project> --title T [--repo PATH] [--kind worktree\|tab\|checkout] [--profile P] [--machine M] [--base REF] --task-file F [--from-task TITLE]` | New thread; `-` reads the task from standard input. `--kind` is the placement; `--profile` is the agent, one the project allows. `--from-task` adds a TASKS.md task's notes and takes `--profile` and `--machine` from its owner. |
| `thread prompt`, `thread next [--line N \| --add TEXT]`, `thread stop`, `thread restart [--profile P]` | Steer a thread. Prompts are recorded in its task file. |
| `thread read [--lines N]`, `thread keys [KEY]... [--text T]`, `thread brief` | Answer a thread's pane without going there: print what it shows (a trust dialog, a question menu, a permission prompt), then type text and press keys (`up`, `down`, `enter`, `esc`, `tab`, a digit). `thread keys` refuses a trust screen when `trust_screens` is `user`. `thread brief` sends a brief the ticker has not delivered yet (the ticker waits for a settled, empty input box, counts a brief sent only once the agent starts working, never types it twice, and after three unconfirmed tries leaves an inbox item). |
| `thread list/show [--json]`, `thread ack`, `thread adopt` | Look at threads. |
| `thread resolve [--keep-worktree] [--discard-uncopied] [--skip-copy] [--reopen]` | Final copy home, then clean up. |
| `sweep <project> [--dry-run] [--yes]` | Remove what nothing uses any more. |
| `set <project> <key> <value>`, `routine list/toggle/approve`, `safety show/yolo/set` | Settings, routines and safety (see below). |
| `profile list [--project P \| --names]`, `profile resolve [NAME]`, `profile add/edit/remove`, `profile allow threads\|coordinator NAMES... [--project P] [--all]`, `profile default threads\|coordinator NAME` | [Agent profiles](#agent-profiles). Everything but `list` and `resolve` needs a person at a terminal. |
| `pause`, `resume`, `archive`, `unarchive`, `delete [--force]` | Project lifecycle. |
| `rename <project> <new-slug> [--name NAME] [--dry-run]` | A new slug (folder name), and with `--name` a new display name. Refused while a thread is not resolved. While agents run in the project folder (its coordinator, which may run it itself) the ticker takes over: once they are idle (at most 5 minutes) it closes their panes, renames, reopens the coordinator in the new folder resuming its conversation, and prompts it with a note; `thread start` is refused meanwhile. The folder moves in one step; the thread records, the coordinator record, `AGENTS.md`, the project's `[safety]` table (yolo, profile lists), routine approvals and the home Space's label follow it. Branches and worktrees keep `hp/<old>/` and `sweep` still finds them; the coordinator's conversation follows (Claude Code's transcript is copied to the new folder; other harnesses resume by id, one that cannot starts anew). It lists what it could not update (other machines), then runs `doctor`. Run it again to finish one that stopped halfway. |
| `popup [project]`, `focus [project]`, `unfocus`, `overview [project]`, `needs-you --line` | Views. |
| `configure [--key K] [--hooks-only] [--dry-run]`, `unconfigure`, `report`, `progress` | Sidebar, keys, hooks, the `autoproject` skill, self-reports. |
| `open-file <path>`, `open-url <url>` | Open a text file in a new tab with `$EDITOR`, or a PR in the browser. |
| `ticker start \| run \| stop \| status`, `doctor [--fix]`, `skill` | Housekeeping. `doctor --fix` refreshes the command in `$XDG_BIN_HOME` or `~/.local/bin`: a Unix symlink, or a Windows `.exe` copy with a source/SHA-256 ownership marker. Foreign or modified commands are left alone. |
| `update [--check]` | Update to the newest release: fetch, rebuild, `doctor --fix`, restart the ticker. |

## Groups

Every thread is in one group, shown in the sidebar, the popup and the digest, needs-you first:

1. **Waiting on you** (`needs you`): a failed start, a pane that is gone before a report, a launch stuck on a dialog, the agent blocked on a question or permission for 30 seconds, or the agent's own report `Waiting for you` while it is not working.
2. **Ready for review** (`review`): a report you haven't acknowledged, or a report with an open pull request, while the agent is not working.
3. **Landing**: an open pull request that is approved.
4. **Working**: the agent works, a launch is under way, or the agent reported progress under 100% in the last five minutes.
5. **Idle**, then **Resolved**.

Threads idle for `auto_resolve_days` are resolved (and cleaned) after a final copy home.

## The popup

`prefix+a` (or the **Projects** action) opens it, scoped to the current workspace's project: the coordinator's workspace or a thread's, found by where its panes work. From any section, `P` opens a project picker with All projects first and the current scope highlighted: `↑`/`↓` (or `k`/`j`) move, `↵` switches, `esc` closes (archived projects are skipped; `↵` on a settings project row still jumps there). In the picker `/` filters by name or slug as you type; `esc` clears the filter, then closes. `/` in any section opens the picker straight into the filter. Outside a project it opens on all projects. Every key runs a CLI command; the popup can do nothing the CLI cannot.

| Section | Keys |
| --- | --- |
| threads | `↵` jump to the pane · `1`-`9` send that Next line to the thread · `s` stop (Escape) · `r` restart with a kind picker · `a` ack · `x` resolve · `o` open the PR · `i` detail (report, Next list, files: `↵` opens, `y` copies the path) · `c` start or focus a coordinator of a chosen kind · `S` sweep |
| tasks | `↵` jump to the delegated thread (or show the notes of a task without one) · `i` notes (a task with notes ends in `≡`) · `d` delegate · `m` done · `D` drop (each sends a sentence to the coordinator, which stays the only writer of TASKS.md) |
| inbox | `↵` detail · `a` done |
| routines | `↵` enable or disable · `i` the prompt |
| settings | `↵` edit (also a safety row) · `Y` yolo mode on or off (asks before turning on) · `p` pause or resume · `A` archive · `X` delete (asks first). Unscoped, the rows are the all-projects safety defaults |
| memory | `↵` read (change memory by asking the coordinator) |

## Safety settings

They live in `~/.config/herdr-projects/config.toml`, per project and as an all-projects default; a project's own value wins, then the default, then the built-in value. Change them in the popup's settings section (`Y` toggles yolo mode, `↵` edits a row) or in a terminal; `safety show <project>|--global` prints them and where each comes from.

```sh
herdr-projects safety yolo billing on          # or off, or default (use the all-projects value)
herdr-projects safety yolo --global on         # every project without its own value
herdr-projects safety set billing routine_commands on
herdr-projects safety set billing start_threads default
```

`safety yolo` and `safety set` refuse unless standard input and output are a terminal, and ask `y/N` before writing. What they write:

```toml
[safety.default]                   # all projects
yolo = true

[safety."/Users/you/.herdr-projects/billing"]
yolo = false                       # on: threads start without asking, agents skip permission prompts
start_threads = "propose"          # or "auto": the coordinator starts threads without asking
trust_screens = "user"             # or "coordinator": who answers trust screens in thread panes (unset: follows yolo)
coordinator_agent_args = []        # extra arguments for every coordinator's agent CLI
thread_agent_args = []             # extra arguments for every thread's agent CLI
routine_commands = false           # true lets approved routines run shell commands
```

**Yolo mode** is the one switch for "never stop to ask": `start_threads` becomes `auto` whatever it says, and every coordinator and thread launches with its own harness's skip-permissions flag, added after the profile's own arguments: Claude Code `--dangerously-skip-permissions`, Codex `--dangerously-bypass-approvals-and-sandbox`, Gemini CLI and Qwen Code `--yolo`, Cursor Agent `--force`, OpenCode `--auto`, Copilot CLI `--allow-all-tools`, Amp `--dangerously-allow-all`, Pi nothing (it never asks). Other kinds have no known flag: their agents still ask (`safety show` says so for the project's kinds). Routine commands are not part of yolo mode: a routine command runs with no agent in the loop, so it stays behind `routine_commands` and a per-command approval.

**Trust screens** are an agent's "trust this folder?" dialog, its restricted-folder chooser, and its hooks or settings review (Codex's "Hooks need review"). The harness saves the answer for every later session in that folder, so `trust_screens` says who gives it: `coordinator` (it answers them with `thread keys` for the thread's own folder) or `user` (it tells you, and `thread keys` refuses while one shows). Unset, it follows yolo mode: on, the coordinator; off, you. Whatever the setting, nothing is typed into a trust screen: a thread's brief waits, and `thread prompt` refuses, while one shows, even when Herdr reads the pane as idle, and the thread shows `needs you`.

A change reaches agents launched after it. Running agents keep the flags they started with until restarted: `r` on a thread in the popup, or quit the coordinator and `open` it again (it resumes its session). A running coordinator sees a new `start_threads` at its next `context`.

`thread_agent_args` and `coordinator_agent_args` are from before profiles. They were written for the harness of the project's default profile, so they now reach only a built-in profile of that harness: Claude flags stay with Claude threads and no longer break a Codex thread. Move them into a profile of your own (below) and remove them.

## Agent profiles

A profile is a named launch setup: a harness (any Herdr agent kind), a model, a reasoning effort, extra arguments and a one-line description. The coordinator only ever chooses a profile by name (`thread start --profile deep`, `open --profile claude`), and `context` lists the ones it may use with their descriptions, so it can pick one per task. It can never pass a launch flag itself: `--agent-arg` is gone. Yolo mode still adds the harness's skip-permissions flag on top of a profile's arguments.

```toml
[profiles.luna]
agent = "omp"
args = ["--config", "~/.omp/agent/luna.yml"]   # `~/` is expanded for local agents
description = "Cheap tier for small, clear tasks"

[profiles.deep]
agent = "codex"
model = "gpt-5.5"
effort = "high"
description = "Hard debugging and design"

[profiles.claude]                  # replaces the built-in `claude`
agent = "claude"
args = ["--add-dir", "~/dev/shared"]

[defaults]                         # what `new` writes into a new PROJECT.md
thread_profile = "claude"
coordinator_profile = "claude"

[safety.default]                   # every project without its own list
thread_profiles = ["claude", "luna", "deep"]   # absent: every profile
coordinator_profiles = ["claude"]

[safety."/Users/you/.herdr-projects/billing"]
thread_profiles = ["luna"]         # this project's own list wins
```

- **Built-ins.** Every Herdr agent kind is a profile of the same name with no arguments, so `thread_profile = "codex"` works with no config at all. Lists show the built-ins whose CLI is on `PATH` and looks signed in. Herdr exposes neither (its `integration.list` sees only hook files and some binaries), so the binary checks itself, offline: credential files and variables such as `~/.codex/auth.json`, `~/.claude.json`'s account, `~/.gemini/oauth_creds.json`, `CURSOR_API_KEY`. A kind with no known check counts once installed. Any built-in can be named even when it is not listed (a remote machine's, say).
- **Model and effort** become the harness's own flags: `--model NAME` for every harness; effort as `--effort` (Claude Code: low, medium, high, xhigh, max; Copilot CLI: none to max), `-c model_reasoning_effort="…"` (Codex: none to ultra), `--thinking` (pi, oh-my-pi: off to max). Cursor puts effort in the model id (`gpt-5.6-sol-xhigh`), and Gemini CLI and OpenCode have no launch flag for it: use the model id or `args`.
- **Allow-lists.** A project's own `thread_profiles` / `coordinator_profiles` wins, then `[safety.default]`'s, then every profile. A profile off the list is refused at `thread start`, `thread restart` and `open`, and the ticker checks again at every launch: a thread whose profile you removed or disallowed since fails with the reason and an inbox item. The project's defaults (`thread_profile`, `coordinator_profile` in PROJECT.md) must be allowed too; `doctor` says when one is not.
- **Who changes what.** Profiles and lists live only in config.toml, which the coordinator never writes. `profile add/edit/remove/allow/default` refuse without a person at a terminal, as `routine approve` does; the popup's settings section writes them directly (`n` new profile, `↵` edit, `d` delete, `↵` on a list toggles profiles with space). The default profiles in PROJECT.md are ordinary settings the coordinator may change when you ask, only to an allowed profile. This is a soft boundary: an agent that fakes a terminal or edits config.toml by hand is stopped only by its own permission prompts.
- **Old threads and coordinators.** A thread started before profiles keeps its harness and its stored model flag. `open` resumes or reuses a coordinator only when it runs the same profile; one started before profiles counts as the built-in of its kind.

## Task owners

Each task in `TASKS.md` has at most one owner, in brackets after its title. The thread running it comes after, as status:

```
- [ ] Write the release notes (me)
- [ ] Rename the settings keys (codex-fast)          a profile, on this machine
- [ ] Fix the M1 build (@elias-macbook-pro-m1)       that machine; its coordinator picks the profile
- [ ] Profile the ticker (deep@elias-macbook-pro-m1) that profile on that machine
- [ ] Look into Safari logouts                       unassigned
- [ ] Review the contract (Priya)                    a person, only when you name them
- [ ] Fix login (claude) · t-0007                    delegated: t-0007 runs it
```

- **Agent owners.** The coordinator assigns only valid names: the thread profiles the project allows here, and each machine with the profiles it lists. Machines come from two places, used together: `herdr machine list` (their profiles are fetched over SSH by running `herdr-projects profile list --names` there), and config.toml, for a machine this one cannot reach, such as a sandboxed VM that only learns the names:

  ```toml
  [machines.elias-macbook-pro-m1]
  profiles = ["claude", "codex-fast"]   # names only; gives no access
  ```

  A machine with an `ssh` key there is fetched as well. A machine is valid only if one of these sources knows it, and `profile@machine` only if that profile is in the machine's list from the lookup or from config.toml. `context` prints one `Assignable:` line (`claude, codex-fast, @m1: claude|pi`), and `assignable <project>` the full list. The SSH lookups are cached for an hour in `<root>/.machines.json`, so `context` does not reach other machines every turn; `assignable --refresh` looks again. A machine that did not answer and has no profiles in config.toml shows as `@m1 (not reached, profiles unknown)`: `@m1` is valid, `profile@m1` is refused. With no machines, only local profiles are valid.
- **Checked owners.** The coordinator checks a profile or machine owner with `assignable --check` before writing it and refuses one that is not valid.
- **People.** `me`, or any name or text you give (`(Elias)`, `(Priya Rao)`), is written only when you name that owner. A bare name that is no profile here is a person: shown as written and never delegated.
- **Delegating.** `thread start --from-task "<title>"` starts the thread with the owner's profile and machine (`--profile`, `--machine`); a flag that contradicts the owner is refused. A remote thread runs its machine's own definition of the profile (see [Threads on other machines](#threads-on-other-machines)), and a `@machine` owner gets that machine's default thread profile.
- **Old lines.** `(agent)` reads as unassigned and `(agent → t-0007)` as this machine's default profile with thread t-0007.

## The allow-list for your coordinator

The coordinator runs the binary every turn, so allow-list it in your agent by subcommand, never the bare binary. `context` prints the exact prefix; patterns must match the actual shell and prefix. The example below is for Claude Code's Unix `Bash` tool in the project folder's `.claude/settings.local.json`. On Windows the printed prefix is PowerShell (`& '<binary>' --root '<root>'`); use your harness's matching PowerShell/shell permission syntax rather than assuming these `Bash(...)` patterns apply.

```json
{ "permissions": { "allow": [
  "Bash(<binary> --root <root> skill:*)",
  "Bash(<binary> --root <root> context:*)",
  "Bash(<binary> --root <root> assignable:*)",
  "Bash(<binary> --root <root> report:*)",
  "Bash(<binary> --root <root> inbox done:*)",
  "Bash(<binary> --root <root> list:*)",
  "Bash(<binary> --root <root> routine list:*)",
  "Bash(<binary> --root <root> thread list:*)",
  "Bash(<binary> --root <root> thread show:*)",
  "Bash(<binary> --root <root> thread prompt:*)",
  "Bash(<binary> --root <root> thread next:*)",
  "Bash(<binary> --root <root> thread read:*)",
  "Bash(<binary> --root <root> thread brief:*)",
  "Bash(<binary> --root <root> thread keys:*)",
  "Bash(<binary> --root <root> thread ack:*)",
  "Bash(<binary> --root <root> thread restart:*)"
] } }
```

- Allow `thread start` only where you've set `start_threads = "auto"`. Left off the list, every thread start meets your agent's own permission prompt.
- With `thread keys` on the list, the coordinator answers a thread's questions and permission prompts itself, by the skill's rules, and its trust screens when `trust_screens` is `coordinator`. Remove it to confirm each answer first.
- Never allow `thread resolve`, `sweep`, `delete`, `rename`, `archive`, `routine approve`, `configure` or `unconfigure`.

## What the safety settings do and don't stop

- **They are soft.** Agents have a shell. The guards are the skill text, your agent's permission prompts, keeping `config.toml` and approvals outside every agent's working directory, and `routine approve`, `safety yolo` and `safety set` refusing without a terminal and a typed confirmation. An agent's shell commands have no terminal, so it cannot flip them by running the CLI; one that fakes a terminal (`script`) or edits `config.toml` directly is stopped only by its own permission prompts, which yolo mode turns off.
- **The coordinator answers threads' prompts.** With `thread keys` it approves plainly in-task permission prompts once, accepts trust screens for the thread's own folder when `trust_screens` is `coordinator`, and asks you about the rest. That is the skill's judgement, not a hard rule (except that `thread keys` refuses trust screens when `trust_screens` is `user`); take `thread keys` off the allow-list to see each one first.
- **A thread can impersonate you.** Any thread agent can prompt the coordinator's pane through Herdr. The skill's rule that a go-ahead must name the threads lowers the risk; it does not remove it.
- **An approved routine command covers the command text only.** `./check.sh` keeps its hash while the script changes.
- **Prompt injection is reduced, not removed.** No GitHub text reaches a prompt from the plugin, but threads read pull request comments themselves with `gh`, and memory is inlined into every later brief.
- **Hooks run in every agent session on the machine.** They exit at once outside a Herdr pane.
- **Cost.** Every thread is a full agent session, and each nudge and each `context` spends coordinator tokens.

## Nudges and notifications

- **Notifications** go out once per event, titled `<Project> · <thread>`: `needs you · ...` with Herdr's request sound; a new report or a merged pull request with the done sound; failed checks, review activity and due routines without sound. `mute = true` silences a project except for errors (a broken routine file, `gh` failing for ten minutes).
- **Nudges** (`nudge = true`, the default for new projects) prompt a coordinator with one line saying what happened (`[hp inbox] t-0040 PR merged; t-0043 blocked on a prompt`) once a set of new inbox items arrives. On Herdr 0.9.1 a prompt merges with text you have half-typed, so the ticker only prompts a coordinator whose state has not changed and been idle for 60 seconds and whose input box, read from its screen, has looked empty for 10 seconds, and picks the one that changed most recently when several qualify. A box it cannot read (a menu, a scrolled view, a harness other than Claude, Codex, Cursor, Gemini, OpenCode and Pi) counts as not empty, and the nudge waits. `thread prompt` refuses to type into a thread's box that holds a draft. `nudge = false` turns this off; notifications still come.

## Routines

A file `routines/<name>.md` with TOML front matter; the body is the prompt.

- `schedule = "every <N>m|h|d"` or `"daily HH:MM"` (local time): the coordinator gets the body as an inbox item when it is due; while that item is unhandled, later runs add none. With no coordinator running (any agent in the project folder counts), a due run does nothing, runs no command and is not made up later; `routine list`, the popup and `doctor` show it as `skipped: no coordinator`. `routine list` shows each routine's last and next run. An optional `command` runs (`sh -c` on Unix, `pwsh.exe -NoProfile -NonInteractive -Command` on Windows, in the project folder with a 60 second timeout) only when `routine_commands = true` and you have run `herdr-projects routine approve <project> <name>` in a terminal; its output reaches the coordinator capped at 4,000 characters inside a fence labelled as untrusted.
- `on = "pr"`, optionally `events = ["opened", "checks-failed", "review", "merged"]`: fired by the ticker's pull request poll. The body goes to the thread whose pull request changed, as a prompt, with facts the binary generates (how many checks fail, how many comments, the `gh` commands to read them). It needs no coordinator, only the open thread.
- Every project has `routines/pr-followup.md` (`checks-failed`, `review`): it tells the thread to fix failing checks and address review comments. Turn it off in the popup's routines section; `doctor --fix` puts it back if the file is missing.

## Cleanup

- **Resolve on merge**: the ticker resolves a thread whose pull request merged only when its agent is neither working nor waiting on you, its own progress says `Done` (an agent that reports no progress: it has written a report since the merge or 10 minutes have passed), and no other pull request its report names is still open. A thread that merged its own pull request can still tag, deploy, land more pull requests and write its final report.
- **Resolve** (popup `x`, chat, or `thread resolve`) copies the report and library home, then removes the worktree with `herdr worktree remove --workspace` (which also closes the workspace) or `git worktree remove` and `git worktree prune`, deletes the local branch if the pull request is merged (it asks GitHub once more first, and finds a pull request by the thread's branch when the report has no `PR:` line), and closes a tab thread's tab. Once no open thread uses the repository's primary workspace that Herdr grouped the worktree under, and it has no agent and only idle, unfocused shells, it is closed too, with one inbox item; the ticker retries one it had to keep. An adopted pane is left alone. The inbox item lists what was removed and what was kept, and why.
- **Merged pull requests and auto-resolve** clean up the same way.
- **Sweep** lists and removes worktrees on `hp/<project>/` branches with no open thread, branches of resolved threads whose pull request merged, tabs of resolved threads, working folders of tab threads resolved longer than `auto_resolve_days` ago, empty repository workspaces its threads' worktrees were grouped under, and handled inbox items older than 30 days. `doctor` shows the same list. Sweep covers local repositories.
- **Archive** closes the project's workspace and its threads' workspaces and hides the project; nothing is deleted, and `unarchive` reopens it. Archive never closes a repository's primary workspace.
- **Delete** moves the folder to `.trash/`.

## Threads on other machines

Save the machine with `herdr machine add --label <label> <ssh target>` (both machines need Herdr 0.9.1 or newer; local Windows verification used 0.9.3), then list a repo as `/path/on/machine@<label>` or pass `thread start --machine <label>`. The home machine owns the project; only outbound SSH from home is needed, in batch mode. Remote execution still requires a POSIX host and shell, plus SSH/SCP and `rsync` on the copy path; this is separate from native local Windows operation.

On native Windows, remote library transfer requires a matching `rsync` and OpenSSH runtime distribution. The exercised pair was MSYS2 `rsync` 3.5.1 with its matching OpenSSH 10.5p1; pairing that `rsync` with Windows' built-in `ssh.exe` failed during the real transfer. Put the matching distribution's binary directory ahead of other SSH installations in the environment that starts the Projects ticker. Keep its DLLs and directory layout intact, and configure its SSH home to use your existing keys without copying them. Local drive destinations are passed as `/proc/cygdrive/<drive>/...`; spaces, apostrophes and Unicode are preserved.

**Verification limit:** the isolated test-only compatibility launcher selected MSYS2 OpenSSH only for `rsync`'s child transport; it is not shipped by the plugin and is not a fix for Windows OpenSSH interoperability. The real MSYS2-rsync/Win32-OpenSSH contrasts failed with protocol exit 12, including `--blocking-io`. Native Win32 OpenSSH worker control and single-file report transfer were exercised separately. The matching-MSYS2 library result must not be presented as native-only Windows OpenSSH directory-transfer proof.

- **Profiles are the machine's own.** `thread start --machine m1 [--profile NAME]` runs `herdr-projects profile resolve [NAME]` on m1 over SSH, which prints m1's definition of that profile (harness, model and effort flags, arguments, `~/` expanded to m1's home) or, without a name, m1's `[defaults] thread_profile`. The thread record keeps it, and the ticker launches with it, so the profile need not exist here. The name must still be on this project's allow-list, checked again at every launch; yolo mode adds its flags as for any thread. `thread restart --profile` looks it up again. m1 needs a herdr-projects that has `profile resolve`; an older one is refused with a hint to run `herdr-projects update` there. A machine known only from config.toml `profiles` has no SSH access, so nothing starts on it from here.
- The worktree, the brief and the report live on the remote machine. The home ticker polls it once a minute and copies a changed report with `scp` and the thread's `library/` with `rsync -rt` (symbolic links are never followed; a library over 50 MB is not copied).
- Remote threads get no self-reports: `report` writes on the machine where the agent runs. Their group comes from the agent state Herdr detects and from their pull request.
- A machine that doesn't answer is left alone: no state is read, threads keep their last group, and after ten minutes you get one `outage` inbox item, and one more when it is back.
- Tasks with no repository always run locally, as tabs.

## Laptop-closed operation

Install Herdr and this plugin on an always-on machine, keep the projects root there, open the project there, and attach from your laptop with `herdr --remote <ssh target>` (add `--session <name>` for a named session). The ticker runs on that machine. If Herdr asks whether to restart a remote server "that may not survive SSH connection loss", answering `n` keeps its panes.

## Windows fork maintenance

The maintenance policy for [`ubranch/herdr-projects`](https://github.com/ubranch/herdr-projects) is to preserve the Windows port on stable `main` and merge only [`eliasstravik/herdr-projects`'s upstream `main`](https://github.com/eliasstravik/herdr-projects). The workflow discovers upstream's default branch through GitHub's API. **Track only** applies to every other upstream branch and every open PR: inventory their metadata, but never check out, merge, test or install their code. This includes [upstream Windows PR #100](https://github.com/eliasstravik/herdr-projects/pull/100); it is not blindly merged. Once a PR merges into upstream `main`, it is an ordinary main update.

Promotion requires strict formatting/Clippy gates, tests and real compiled-CLI smoke checks for the exact merge candidate on `windows-2022` (`x86_64-pc-windows-msvc`) and `ubuntu-24.04` (`x86_64-unknown-linux-gnu`). Each gate installs stable Rust with `rustfmt` and `clippy`, then runs `cargo +stable fmt --all -- --check` and `cargo +stable clippy --locked --all-targets --target <native-target> -- -D warnings` before locked Cargo tests/builds and the CLI's `--version`, isolated-root `new`, `list` and `context --peek`. These are required checks, not evidence that they have passed. Candidate checks retain read-only permissions and no repository credentials. Only the separately gated publish job has write permission; it verifies the candidate SHA and base/upstream ancestry without checking out or executing candidate code, checks that fork `main` still matches the prepared base and pushes a fast-forward. Merge conflicts, failing gates or a changed base stop promotion and leave fork `main` unchanged. Never force-push, reset or automatically resolve conflicts over the port.

### Enable, run and inspect

1. The Windows port and [`.github/workflows/upstream-sync.yml`](../.github/workflows/upstream-sync.yml) are published on the fork's default branch, `main`. Enable Actions in the fork and enable **Upstream sync** if disabled. The workflow is restricted to this fork's `main`; publication and activation are prerequisites, not evidence that CI has passed.
2. In the fork's [Actions](https://github.com/ubranch/herdr-projects/actions/workflows/upstream-sync.yml), select **Upstream sync → Run workflow → main → Run workflow**. There are no inputs. Manual dispatch runs the gates even when upstream has not changed; scheduled runs skip them when no update is needed.
3. Open that run's **Summary** for the pinned SHAs, branch/open-PR inventory links and preparation/promotion outcome. Download **Artifacts → upstream-inventory** for `upstream-inventory.json`, retained for seven days. Read the gate job logs for actual formatter, Clippy, test, build and smoke results; earlier local Windows verification is not proof of a new hosted run.

The configured cron is `7,22,37,52 * * * *`: every 15 minutes, offset from the hour, in UTC. Each run refreshes the branch/open-PR inventory, even without a main update. This is best effort, not a continuous-service guarantee: [GitHub schedules](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule) run only from a workflow on the default branch and can be delayed or dropped. Scheduled workflows are [disabled by default on public forks and after 60 days without repository activity](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/disable-and-enable-workflows); re-enable **Upstream sync** in Actions and dispatch it manually when needed.

### Update a local Windows checkout

The Windows port and sync workflow are published on fork `main`. Use a **clean `main` checkout** whose `origin` is `ubranch/herdr-projects`, then run:

```powershell
git pull --ff-only origin main
if ($LASTEXITCODE -ne 0) { throw 'Pull failed; preserve the checkout and do not install.' }
```

Rebuild/install deliberately with `scripts/install.ps1` using the [Windows update instructions](getting-started.md#updating). The Rust executable implements the plugin; PowerShell provides build/install/command-link glue, not a separate plugin implementation. If the checkout is dirty or cannot fast-forward, preserve the development work and resolve it manually; do not reset it or switch to upstream `main`. Cloud sync updates only the fork repository: it does not install or overwrite local binaries or development work. Windows release assets remain unpublished; sync does not create a release. Only a deliberate installed-plugin update stops/restarts its ticker; source formatting, linting and smoke checks must leave the ticker and existing Herdr sessions alone.

## Development

The helper scripts below are Unix-only. On native Windows, build with the source installer and use a scratch named Herdr session, a separate `HERDR_PROJECTS_ROOT`, `HERDR_CONFIG_PATH` and `XDG_BIN_HOME`; set `[terminal] default_shell = "pwsh.exe"` in the scratch config. Keep the user's existing OMP model/auth/skills rather than resetting or replacing them. The [native Windows manual checks](manual-test.md#native-windows-port) are acceptance cases, not a claim that every case has passed.

```bash
cargo test                       # unit tests and scenarios against a scripted fake runner
scripts/dev-server               # a throwaway `hp-dev` Herdr session with a scratch root
scripts/dev-hp <subcommand>      # the binary against <repo>/.dev-root; pass --session hp-dev to open/doctor
scripts/dev-herdr <args>         # herdr against that session
HERDR_CONFIG_PATH=<copy> ...     # point configure and `herdr config check` at a scratch config
```

For gated-sync regression checks, run `bash scripts/test-sync-upstream.sh` from the repository root. It requires Git/Bash and uses temporary Git repositories.

Never develop against your default session, `~/.herdr-projects` or your real `config.toml`. [`herdr-notes.md`](herdr-notes.md) records what was verified about Herdr, and [`manual-test.md`](manual-test.md) lists the acceptance checks, including the visual ones only a person can confirm.

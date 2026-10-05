# Getting started: open your first project

Install the plugin, run `configure` once, create a project, and talk to its coordinator.

## 1. Check the prerequisites

- macOS/Linux with [Herdr](https://herdr.dev) 0.9.1 or newer, or native Windows x64 (exercised with Herdr 0.9.3). Check `herdr status`: both the client and the running server must meet the requirement. An already-running server retains its old version after `herdr update`; defer plugin installation if `herdr plugin link` or `install` reports `plugin_requires_newer_herdr`, rather than interrupting active sessions. On Windows, install PowerShell 7 (`pwsh.exe`) and use it as Herdr's session shell.
- For the source fallback only: Rust/Cargo 1.89 or newer and a C compiler. On Windows this requires the `x86_64-pc-windows-msvc` Rust toolchain and Visual Studio C++ Build Tools with a Windows SDK. Fork releases from v0.2.35 provide prebuilt binaries for macOS (Apple Silicon and Intel), Linux and native Windows x64, so a verified prebuilt install needs neither Rust nor compiler tools. On macOS, `xcode-select --install` installs Apple's build tools. Install Rust with [rustup](https://rustup.rs).
- Git.
- An installed, signed-in agent CLI Herdr can start, on `PATH` (`claude`, `codex`, `opencode`, `oh-my-pi` and more). Claude Code is the one exercised most on Unix; a native Windows coordinator has run with oh-my-pi 18.5.1 using the user's existing model, auth and skills. Every agent that can run a shell command reports its own progress: thread briefs and the coordinator skill carry the instructions. `configure` also installs hooks for Claude Code, Codex, Droid, Gemini CLI and Copilot CLI, which add a reminder about once a minute.
- Optional: `gh`, logged in, for pull request follow-up; `ssh`, `scp` and `rsync` for threads on POSIX machines. Local report/library copying needs neither `rsync` nor `du`, including on Windows.

The plugin needs no hosted service and no API key. It depends on Herdr and nothing else, no other plugin included.

## 2. Install the plugin

### macOS/Linux

```bash
herdr plugin install ubranch/herdr-projects
```

Review the install preview. Herdr clones the fork, runs `scripts/install.sh`, and registers the plugin. The script downloads the release's prebuilt binary for your machine and checks it against the release's `SHA256SUMS`. When there is no such binary, the download fails or the checksum does not match, it says so and runs `cargo build --release --locked` instead. Set `HERDR_PROJECTS_BUILD=source` to always build from source. A Git checkout with tracked changes, or whose `HEAD` is not at its version's published release tag, also builds from source. Its startup command starts a background ticker only when you have at least one project.

On Unix the command is a symlink at `~/.local/bin/herdr-projects` (`$XDG_BIN_HOME` when set). Startup and `doctor --fix` refresh plugin-owned or dangling links, never a regular file or a link to a different local checkout. If the bin directory is not on your `PATH`, add it in your shell profile (`doctor` says so):

```bash
export PATH="$HOME/.local/bin:$PATH"
herdr-projects doctor
```

### Native Windows x64: fork main

```powershell
herdr plugin install ubranch/herdr-projects
```

Review the install preview. Herdr runs `scripts/install.ps1` and registers the plugin. The [fork releases](https://github.com/ubranch/herdr-projects/releases) provide native Windows x64 binaries from v0.2.35. This fork maintains the Windows port of [eliasstravik/herdr-projects](https://github.com/eliasstravik/herdr-projects); use the fork install target, not upstream `main` or an unmerged upstream Windows PR. The plugin is the compiled Rust `herdr-projects.exe`; PowerShell scripts provide download/build/install/command-link glue, not a separate implementation. `herdr plugin link` registers a checkout but does not build it.

For a linked checkout instead, clone fork `main` into an unused directory:

```powershell
git clone --branch main https://github.com/ubranch/herdr-projects.git C:\Projects\herdr-projects
if ($LASTEXITCODE -ne 0) { throw 'Clone failed; do not continue.' }
```

Use a clean fork `main` checkout: `git branch --show-current` must print `main`, `git status --short` must be empty, and `git remote get-url origin` must name `ubranch/herdr-projects`. Preserve any development work in another checkout; do not reset it or switch it to upstream `main`.

For this linked-checkout option, run the following in PowerShell 7 only when deliberately installing/registering the plugin. For an existing installation, use [Updating](#updating) instead. Source-only formatting, linting and smoke checks do not need installation or changes to the ticker or existing Herdr sessions.

```powershell
Set-Location -LiteralPath 'C:\Projects\herdr-projects' -ErrorAction Stop
git pull --ff-only origin main
if ($LASTEXITCODE -ne 0) { throw 'Pull failed; preserve the checkout and do not install.' }
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install.ps1
if ($LASTEXITCODE -ne 0) { throw 'Install failed; do not continue.' }
herdr plugin link .
if ($LASTEXITCODE -ne 0) { throw 'Plugin link failed; do not continue.' }
& .\target\release\herdr-projects.exe doctor --fix
if ($LASTEXITCODE -ne 0) { throw 'Installed, but doctor reported problems; read its output.' }
```

The installer first attempts the matching prebuilt release: a Git checkout must have no tracked changes and its `HEAD` must match its version's published tag. Downloads must match the unique `SHA256SUMS` entry and report the expected version before installation. A checkout between releases, a missing asset, or a failed download/verification uses the existing `cargo build --release --locked` source fallback. Set `HERDR_PROJECTS_BUILD=source` only when deliberately forcing that build. The installer stages and validates either candidate, then replaces `target\release\herdr-projects.exe` by rename so it does not overwrite a running image. Failed build/validation leaves the installed binary alone; a failed replacement attempts rollback.

Startup or the explicit `doctor --fix` above installs a regular `herdr-projects.exe` command copy with `.herdr-projects-command.json` recording its source and SHA-256. It uses `$env:XDG_BIN_HOME`, else `~\.local\bin`; `~` is `$env:HOME`, falling back to `$env:USERPROFILE`. Foreign, modified, or unmarked commands are left alone. No command symlink privileges are needed.

Add that bin directory to this shell's `PATH`:

```powershell
$homeDir = if ($env:HOME) { $env:HOME } else { $env:USERPROFILE }
$binDir = if ($env:XDG_BIN_HOME) { $env:XDG_BIN_HOME } else { Join-Path $homeDir '.local\bin' }
$env:Path = "$binDir;$env:Path"
herdr-projects doctor
```

For future shells, add `$binDir`'s resolved path to your **user Path** in Windows Environment Variables. Start future Herdr sessions from a shell with `pwsh.exe`, Git, your agent CLI and the command directory on `PATH`; an already-running server retains its old environment. Leave existing Herdr sessions running.

In Herdr's existing config (`%APPDATA%\herdr\config.toml` by default on Windows), set or update the existing terminal setting; do not replace the rest of your config:

```toml
[terminal]
default_shell = "pwsh.exe"
```

Generated local commands and coordinator briefs use PowerShell syntax; keep their leading `&` and quoted paths. Hooks and the tab-bar command launch `pwsh.exe -EncodedCommand`, so they also work when Herdr invokes them through CMD. Installation uses Windows PowerShell (`powershell.exe`); SSH commands remain POSIX shell commands.

## 3. Run configure once

```bash
herdr-projects configure --dry-run   # shows what it would change
herdr-projects configure
```

Or run `herdr plugin action invoke configure --plugin herdr-projects`. It records each change, so `herdr-projects unconfigure` removes exactly what it added:

- **Your Herdr config** (`~/.config/herdr/config.toml` on Unix; `%APPDATA%\herdr\config.toml` on Windows, or the `XDG_CONFIG_HOME`/`HERDR_CONFIG_PATH` override). The sub-line row under agents (`$hp_sub`), a one-line card in place of Herdr's built-in rows that shows each project's head in bold (rows you wrote yourself are left alone), the popup key `prefix+a` and a tab-bar entry `projects: N need you`. Herdr checks the result with `herdr config check` before anything is written. Pick another key with `configure --key prefix+y`; a key Herdr or you already use is refused.
- **Progress hooks** for each installed harness: Claude Code (`~/.claude/settings.json`), Codex (`~/.codex/hooks.json`), Droid (`~/.factory/settings.json`), Gemini CLI (`~/.gemini/settings.json`) and Copilot CLI (its own `~/.copilot/hooks/herdr-projects.json`). They tell an agent running in a Herdr pane how to report its progress, and remind it about once a minute. Outside Herdr they do nothing. Existing hooks and comments are kept; the Windows wrappers use encoded PowerShell commands rather than POSIX redirections.
- **The `autoproject` skill**, linked from the plugin's `skill/autoproject` into `~/.claude/skills` and Codex's `~/.agents/skills`: symlinks on Unix, NTFS directory junctions on Windows without elevation or Developer Mode. A coordinator loads it with `/autoproject` to run an independently reviewed improvement loop. An unrelated skill of that name is left alone, and `doctor` names it. If you configured before the skill shipped, `doctor --fix` installs it for configured harnesses.

Configure reloads the Herdr server's config. The sidebar rows are drawn by your client: if they don't show yet, run **reload config** in Herdr (`prefix+shift+r`).

If you used the standalone Agent Progress plugin, `doctor` prints the two commands that remove its hooks, so only one set runs.

## 4. Create and open a project

From a Herdr pane, run **Projects: new project** with `herdr plugin action invoke new --plugin herdr-projects`. It asks for a name and a goal, creates the project, and opens it. Or from a terminal inside Herdr:

```bash
herdr-projects new "Billing" --goal "Ship the new billing page" --repo ~/dev/app
herdr-projects open billing
```

On Windows, use a native repo path, for example `herdr-projects new "Billing" --goal "Ship the new billing page" --repo 'C:\dev\app'`, then `herdr-projects open billing`. The plain CLI commands below also run in PowerShell; use PowerShell syntax for paths and shell expressions.

`new` creates `~/.herdr-projects/billing/` with an `AGENTS.md` and matching `CLAUDE.md`: a relative symlink on Unix, a synchronized regular file on Windows. Plugin priming repairs and renames keep the Windows copy current. A Windows copy is considered plugin-owned only when its bytes match the current or prior `AGENTS.md`; an unrelated file is saved as `CLAUDE.md.before-herdr-projects`. If that backup already exists, a conflict preserves both foreign files rather than deleting one or creating more backups; a name change reports that conflict too. `open` starts your agent in that folder, right in the pane you typed it in. Quit the agent and you are back at your shell. The agent reads `AGENTS.md`, which tells it that it is the coordinator and which two commands to run. Nothing is typed into it for you.

- `open billing --tab` starts it in a new tab of the project's own workspace instead. The plugin's actions and the popup always do that, and so does `open` run outside Herdr.
- When a coordinator is already running, `open` jumps to it. `open --new` starts another beside it, with a fresh conversation.
- `open billing --profile codex` starts another agent: every installed, signed-in harness is a profile, and your own profiles (a model, an effort, extra flags) are made in the popup's settings or with `profile add` ([operations](operations.md#agent-profiles)). Any agent you start by hand in that folder is a coordinator too, with no `open` needed, and several can run side by side.
- `open` resumes the agent's last session when Herdr recorded one for that profile.
- The first time, your agent may ask whether you trust the folder: answer it in the coordinator's pane.

## 5. Tell the coordinator what you want

Type in the coordinator's pane, for example: "Add a billing page: API endpoint, the page itself, and end-to-end tests."

On a new project it restates the goal, lists the repos, and asks for the first piece of work. It proposes threads and waits until you name the ones to start (or say "all"). Tell it how you like threads run ("workers use codex", "at most two at a time") and it remembers.

Everything about the project can be changed in chat: goal, instructions, repos, settings, tasks, routines, memory. You never need to edit a file.

## 6. Watch the threads

Each code thread runs in its own worktree workspace on a branch named `hp/<project>/<id>-<title>`; a task with no repository runs as a tab in the project's workspace.

- **The sidebar** shows each thread as `t-0003 · <title>` with a line under it that adds what Herdr's own state word does not say: `needs you · ~55%`, `review · PR #4`, `~40%`, `12m quiet`, `landing · PR #4`. The agent's own activity follows on the same line. The tab bar says `projects: 2 need you`. Agents and Spaces are grouped by project: each project starts with its own head row, the home Space and the coordinator, which show the project's name in bold, and its threads by need and its other Spaces follow as Herdr's own rows; agents or Spaces outside any project come last. Selecting a row lights only that row. The ticker keeps the Spaces in these blocks, so a Space you drag elsewhere moves back.
- **The popup** (`prefix+a`) lists threads, tasks, inbox, routines, settings and memory. Every thread report ends with a `## Next` list; press a number to send that line back to the thread, which then does it with its own tools. Other keys jump to a thread, stop it, restart it with another profile, resolve it, open its PR, edit settings, pause or archive the project.
- **Notifications** name the project and thread: `Billing · t-0003`, `needs you · blocked` with a sound; a new report or a merge with a softer one. `mute = true` (popup settings) silences a project.

New worktrees are folders your agent hasn't trusted yet, so a code thread usually starts with your agent's trust dialog and shows `needs you` until it is answered. Who answers is the `trust_screens` safety setting: in yolo mode the coordinator does (with `thread keys`); otherwise you do, in its pane. The brief waits until then. Codex lets a worktree inherit its repo's trust, so trusting the repo once there covers its worktrees.

When a pull request fails its checks or gets review comments, the ready-made `pr-followup` routine prompts the thread to fix them. When it merges, the thread is resolved and its worktree, workspace and branch are removed. Its report stays in `threads/<id>.md` and its files in `library/<id>/`.

## Check your setup

```bash
herdr-projects doctor          # what is installed, configured, and left over
herdr-projects doctor --fix    # repairs the plugin's own files in each project
herdr-projects ticker status
```

- **A project made with an older version**: `doctor --fix` adds `AGENTS.md`, matching `CLAUDE.md` (Unix link or Windows copy), `uploads/` and `routines/pr-followup.md`, rewrites binary paths that point at a moved binary, and links the `autoproject` skill for each harness you configured. It never touches another plugin's entries.
- **`open` says the session is not reachable**: run it inside Herdr, or pass `--session <name>`. A project belongs to the session it was first opened in. On Windows `herdr.sock` is a regular liveness marker containing `notUnixSocket`, not a Unix socket; direct view requests use Herdr's named pipe.
- **A thread stays at "no agent"**: the ticker launches agents, one per machine per tick (about 15 seconds). After three failed launches the thread is marked failed with the reason; `thread restart` tries again.
- **Herdr was restarted**: Herdr resumes Claude and Codex panes itself; the ticker gives resumed threads their names back. Threads of other agents need `thread restart`.

## Updating

**Native Windows fork checkout:** the port and sync workflow are published on `ubranch/herdr-projects`'s `main`, and fork releases from v0.2.35 include Windows x64 assets. `update` is release-driven; it does not pick up every fork-main source change, and a linked install must be clean and on `main`. Do not switch to upstream `main` to satisfy that guard.

When deliberately updating the installed plugin from a linked checkout, use the clean fork `main` checkout described [above](#native-windows-x64-fork-main), fast-forward it, then stop only the plugin ticker with the installed binary before replacing it. The installer uses the verified prebuilt at its version's published release commit and the locked-source fallback between releases. Leave Herdr sessions, coordinators and threads running. Do not run this installation/ticker restart procedure during source-only formatting, linting or smoke checks.

```powershell
Set-Location -LiteralPath 'C:\Projects\herdr-projects' -ErrorAction Stop
git pull --ff-only origin main
if ($LASTEXITCODE -ne 0) { throw 'Pull failed; preserve the checkout and do not install.' }
herdr-projects ticker stop
if ($LASTEXITCODE -ne 0) { throw 'Ticker did not stop; do not replace the binary.' }
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install.ps1
if ($LASTEXITCODE -ne 0) {
    herdr-projects ticker start
    if ($LASTEXITCODE -ne 0) { throw 'Install failed and the ticker could not restart; read the errors before retrying.' }
    throw 'Install failed; read the installer error before retrying.'
}
& .\target\release\herdr-projects.exe doctor --fix
$doctorExit = $LASTEXITCODE
& .\target\release\herdr-projects.exe ticker start
if ($LASTEXITCODE -ne 0) { throw 'Installed, but the ticker did not start; read its output.' }
if ($doctorExit -ne 0) { throw 'Installed and ticker restarted, but doctor reported problems; read its output.' }
```

Run this from the checkout. If an earlier Unix ticker is running when adopting this port, stop it **with the old binary before replacing it**: `.ticker.lock` is now a persistent lock token and readable metadata is in `.ticker.info`. Do not delete either ticker or project lock tokens to clear a stale status; status follows the held OS lock, not the presence of a file.

**Once, on macOS/Linux if you're on 0.2.2 or older** (`herdr-projects --version`), which has no `update` yet:

```bash
herdr-projects ticker stop
herdr plugin install ubranch/herdr-projects
herdr-projects doctor --fix
herdr-projects ticker start
```

Herdr reinstalls the plugin in the same folder, and the plugin keeps your `~/.local/bin/herdr-projects` link pointing at it. If you linked a local checkout with `herdr plugin link` instead, run `git pull --ff-only origin main` from a clean `main` checkout and `sh scripts/install.sh` in place of the `herdr plugin install` line.

**From then on, for release-based macOS/Linux updates:**

```bash
herdr-projects update           # fetch, install the new binary, doctor --fix, restart the ticker
herdr-projects update --check   # only print the installed and the newest version
```

`update` works for both install types and changes nothing when you're already on the newest release. Its `doctor --fix` also links the `autoproject` skill for each harness you configured, so existing users don't need to run `configure` again. A linked checkout must be on `main` with no uncommitted changes, or `update` stops and says why. When the install fails, the old version stays installed and the ticker is restarted. `doctor` says when a newer version is out.

## Remove

```bash
herdr-projects unconfigure
herdr-projects ticker stop
herdr plugin uninstall herdr-projects      # or: herdr plugin unlink herdr-projects
```

Your projects stay in `~/.herdr-projects/`; delete them yourself if you no longer want them.

mod actions;
mod adopt;
mod agents;
mod assign;
mod brief;
mod cli;
mod command_link;
mod coordinator;
mod doctor;
mod grouping;
mod herdr;
mod inbox;
mod lifecycle;
mod names;
mod notify;
mod overview;
mod paths;
mod popup;
mod pr;
mod profiles;
mod progress;
mod project;
mod prompt_box;
mod remote;
mod rename;
mod routine;
mod runner;
mod safety;
#[cfg(test)]
mod scenarios;
mod settings;
mod setup;
mod sidebar;
mod spaces;
mod steps;
mod sweep;
mod tasks;
mod thread;
mod threads;
mod ticker;
mod trust_screen;
mod update;

/// Crate version plus a build identifier (short git hash and build time), so a
/// rebuilt binary always differs from the one a running ticker was started from.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("HP_BUILD_ID"));

/// `PATH` as this process received it, before [`extend_path`]: what the
/// user's shell resolves, for `doctor`'s command check.
pub static USER_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// A herdr server that was not started from a login shell hands its plugins a
/// minimal `PATH`, so `gh`, `rsync` or the agent CLI may be missing for the
/// ticker although they work in the user's terminal. The usual install folders
/// are appended (never prepended: what the user's `PATH` resolves still wins).
fn extend_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let _ = USER_PATH.set(current.to_string_lossy().into_owned());
    let mut dirs: Vec<std::path::PathBuf> = std::env::split_paths(&current).collect();
    let env = paths::Env::from_process().ok();
    #[cfg(unix)]
    let mut extra: Vec<std::path::PathBuf> =
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
            .iter()
            .map(Into::into)
            .collect();
    #[cfg(windows)]
    let mut extra: Vec<std::path::PathBuf> = Vec::new();
    if let Some(env) = env {
        extra.push(env.home.join(".local/bin"));
        extra.push(env.home.join(".cargo/bin"));
        #[cfg(windows)]
        {
            if let Some(appdata) = env.var("APPDATA") {
                extra.push(std::path::Path::new(appdata).join("npm"));
            }
            if let Some(local) = env.var("LOCALAPPDATA") {
                extra.push(std::path::Path::new(local).join("Microsoft/WinGet/Links"));
            }
        }
    }
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        // SAFETY: first thing in `main`, before any thread exists.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

fn main() {
    extend_path();
    if let Err(error) = cli::run() {
        eprintln!("herdr-projects: {error:#}");
        std::process::exit(1);
    }
}

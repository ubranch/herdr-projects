//! The `herdr-projects` command on `PATH`: a Unix symlink or managed Windows
//! executable copy in `~/.local/bin` (or `$XDG_BIN_HOME`). Plugin start and
//! `doctor --fix` refresh it; foreign files are always left alone.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths::Env;

pub const NAME: &str = "herdr-projects";

#[cfg(unix)]
const FILE_NAME: &str = NAME;
#[cfg(windows)]
const FILE_NAME: &str = "herdr-projects.exe";

/// The folder the link goes in: `$XDG_BIN_HOME`, else `~/.local/bin`.
pub fn bin_dir(env: &Env) -> PathBuf {
    env.var("XDG_BIN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.join(".local/bin"))
}

pub fn link_path(env: &Env) -> PathBuf {
    bin_dir(env).join(FILE_NAME)
}

/// Whether `binary` is an installed build (`target/release/herdr-projects`),
/// not a test or debug binary that must never become the user's command.
pub fn installable(binary: &Path) -> bool {
    binary.ends_with(Path::new("release").join(FILE_NAME))
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// The link or managed copy of `binary`.
    Ours,
    Missing,
    /// A dangling/plugin link or outdated managed copy: safe to replace.
    Stale(PathBuf),
    /// A file, or a link somewhere else (a checkout of the user's own): never touched.
    Foreign(String),
}

#[cfg(windows)]
#[derive(serde::Serialize, serde::Deserialize)]
struct InstalledCommand {
    binary: PathBuf,
    sha256: String,
}

#[cfg(windows)]
fn marker_path(binary: &Path) -> PathBuf {
    binary.with_file_name(".herdr-projects-command.json")
}

#[cfg(windows)]
const LOCK_SIGNATURE: &[u8] = b"herdr-projects command transaction lock\n";

#[cfg(windows)]
fn plain_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.is_file()
        && metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            == 0
}

#[cfg(windows)]
fn move_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // No REPLACE_EXISTING: publication and restoration never clobber a new owner.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            0,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Shared with link-command.ps1: a persistent signed, non-reparse token, opened
/// without delete sharing and exclusively locked at offset 0 for u64::MAX bytes.
/// Never remove the token: existing waiters must continue to lock the same file.
#[cfg(windows)]
fn transaction_lock(command: &Path) -> Result<std::fs::File> {
    use std::io::{Read, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let path = command.with_file_name(".herdr-projects-command.lock");
    let mut options = std::fs::File::options();
    options
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let temporary = path.with_file_name(format!(
                ".herdr-projects-command.{}.{nonce}.lock.tmp",
                std::process::id()
            ));
            let mut staged = std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let result = (|| -> std::io::Result<()> {
                staged.write_all(LOCK_SIGNATURE)?;
                staged.sync_all()?;
                drop(staged);
                match move_new(&temporary, &path) {
                    Err(_) if std::fs::symlink_metadata(&path).is_ok() => Ok(()),
                    result => result,
                }
            })();
            let _ = std::fs::remove_file(&temporary);
            result?;
            options.open(&path)?
        }
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        plain_file(&file.metadata()?),
        "foreign command lock: {}",
        path.display()
    );
    file.lock()
        .with_context(|| format!("could not lock {}", path.display()))?;
    let mut signature = [0; LOCK_SIGNATURE.len()];
    anyhow::ensure!(
        file.metadata()?.len() == LOCK_SIGNATURE.len() as u64
            && file.read_exact(&mut signature).is_ok()
            && signature.as_slice() == LOCK_SIGNATURE,
        "foreign command lock: {}",
        path.display()
    );
    Ok(file)
}

#[cfg(windows)]
fn file_identity(path: &Path) -> Result<(u32, u32, u32)> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
    };
    let file = std::fs::File::options()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    anyhow::ensure!(
        plain_file(&file.metadata()?),
        "not a regular file: {}",
        path.display()
    );
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

#[cfg(windows)]
fn marker_snapshot(command: &Path) -> Result<Option<Vec<u8>>> {
    let marker = marker_path(command);
    match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            anyhow::ensure!(
                plain_file(&metadata),
                "foreign command marker: {}",
                marker.display()
            );
            Ok(Some(std::fs::read(marker)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn file_hash(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(windows)]
fn managed_copy(binary: &Path) -> Option<InstalledCommand> {
    if !std::fs::symlink_metadata(binary).is_ok_and(|meta| plain_file(&meta))
        || !std::fs::symlink_metadata(marker_path(binary)).is_ok_and(|meta| plain_file(&meta))
    {
        return None;
    }
    let marker: InstalledCommand = crate::project::read_json(&marker_path(binary))?;
    (file_hash(binary).ok().as_deref() == Some(marker.sha256.as_str())).then_some(marker)
}

/// A copied command still belongs to the plugin checkout, for hooks and updates.
#[cfg(windows)]
pub fn installed_source(binary: &Path) -> Option<PathBuf> {
    let marker = managed_copy(binary)?;
    if !installable(&marker.binary) || !marker.binary.is_file() {
        return None;
    }
    crate::paths::canonicalize(&marker.binary).ok()
}
pub fn state(env: &Env, binary: &Path) -> State {
    #[cfg(windows)]
    {
        let command = link_path(env);
        if std::fs::symlink_metadata(&command).is_err() {
            return State::Missing;
        }
        match managed_copy(&command) {
            Some(marker)
                if file_hash(binary).ok().as_deref() == Some(marker.sha256.as_str())
                    && crate::paths::canonicalize(binary).ok()
                        == crate::paths::canonicalize(&marker.binary).ok() =>
            {
                State::Ours
            }
            Some(marker) => State::Stale(marker.binary),
            None => State::Foreign("not an unchanged herdr-projects command copy".into()),
        }
    }
    #[cfg(unix)]
    {
        let link = link_path(env);
        let Ok(meta) = std::fs::symlink_metadata(&link) else {
            return State::Missing;
        };
        if !meta.file_type().is_symlink() {
            return State::Foreign("a file, not a link".into());
        }
        let Ok(target) = std::fs::read_link(&link) else {
            return State::Foreign("an unreadable link".into());
        };
        let target = link.parent().map(|dir| dir.join(&target)).unwrap_or(target);
        let binary = crate::paths::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
        match crate::paths::canonicalize(&target) {
            Ok(resolved) if resolved == binary => State::Ours,
            Err(_) => State::Stale(target),
            Ok(_) if target.starts_with(plugins_dir(env)) => State::Stale(target),
            Ok(_) => State::Foreign(format!("a link to {}", target.display())),
        }
    }
}

/// Herdr's plugin installs live under `<herdr config dir>/plugins`.
#[cfg(unix)]
fn plugins_dir(env: &Env) -> PathBuf {
    env.herdr_config_dir().join("plugins")
}

/// Makes the link when it is missing or stale; returns the state found.
#[cfg(unix)]
pub fn ensure(env: &Env, binary: &Path) -> Result<State> {
    let found = state(env, binary);
    if matches!(found, State::Missing | State::Stale(_)) {
        let link = link_path(env);
        let dir = bin_dir(env);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        // Link under a temporary name, then rename over: never a moment without a command.
        let tmp = dir.join(format!(".{NAME}.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(binary, &tmp)
            .with_context(|| format!("could not link {}", link.display()))?;
        if let Err(error) = std::fs::rename(&tmp, &link) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("could not link {}", link.display()));
        }
    }
    Ok(found)
}

#[cfg(windows)]
pub fn ensure(env: &Env, binary: &Path) -> Result<State> {
    if state(env, binary) == State::Ours {
        return Ok(State::Ours);
    }
    let command = link_path(env);
    let dir = bin_dir(env);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let _lock = transaction_lock(&command)?;
    let found = state(env, binary);
    if !matches!(found, State::Missing | State::Stale(_)) {
        return Ok(found);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let tmp = dir.join(format!(".{NAME}.{}.{}.tmp.exe", std::process::id(), nonce));
    let temporary_marker = tmp.with_extension("json");
    let previous = dir.join(format!(
        ".{NAME}.{}.{}.previous.exe",
        std::process::id(),
        nonce
    ));
    let mut command_staged = false;
    let mut marker_staged = false;
    let result = (|| -> Result<State> {
        use std::io::Write;
        std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        command_staged = true;
        std::fs::copy(binary, &tmp)?;
        let marker = InstalledCommand {
            binary: crate::paths::canonicalize(binary)?,
            sha256: file_hash(&tmp)?,
        };
        let mut staged_marker = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&temporary_marker)?;
        marker_staged = true;
        serde_json::to_writer(&mut staged_marker, &marker)?;
        staged_marker.write_all(b"\n")?;
        staged_marker.sync_all()?;
        drop(staged_marker);
        let published_identity = file_identity(&tmp)?;
        // Staging can take time; never publish over a command changed meanwhile.
        let found = state(env, binary);
        if !matches!(found, State::Missing | State::Stale(_)) {
            return Ok(found);
        }
        let previous_marker = marker_snapshot(&command)?;
        let replaced = matches!(found, State::Stale(_));
        if replaced {
            move_new(&command, &previous)?;
        }
        if let Err(error) = move_new(&tmp, &command) {
            if replaced {
                move_new(&previous, &command)
                    .with_context(|| format!("could not restore {}", command.display()))?;
            }
            return Err(error.into());
        }
        if let Err(error) = std::fs::rename(&temporary_marker, marker_path(&command)) {
            anyhow::ensure!(
                file_identity(&command).ok() == Some(published_identity)
                    && file_hash(&command).ok().as_deref() == Some(marker.sha256.as_str())
                    && marker_snapshot(&command).ok().as_ref() == Some(&previous_marker),
                "could not publish command marker: {error}; command or marker changed; previous command retained at {}",
                previous.display()
            );
            std::fs::remove_file(&command)
                .with_context(|| format!("could not roll back {}", command.display()))?;
            if replaced {
                move_new(&previous, &command)
                    .with_context(|| format!("could not restore {}", command.display()))?;
            }
            return Err(error.into());
        }
        if replaced && let Err(error) = std::fs::remove_file(&previous) {
            eprintln!(
                "herdr-projects: previous command retained at {}: {error}",
                previous.display()
            );
        }
        Ok(found)
    })();
    if command_staged {
        let _ = std::fs::remove_file(&tmp);
    }
    if marker_staged {
        let _ = std::fs::remove_file(&temporary_marker);
    }
    result.with_context(|| format!("could not install {}", command.display()))
}

/// What a shell with `path_var` runs for `herdr-projects`, if anything.
pub fn resolves_to(env: &Env, path_var: &str) -> Option<PathBuf> {
    crate::profiles::find_executable_on_path(path_var, env.var("PATHEXT"), NAME)
}

/// The `doctor` line: `(ok, detail)` where `None` is a warning.
pub fn check(env: &Env, binary: &Path, path_var: &str, fix: bool) -> (Option<bool>, String) {
    let link = link_path(env);
    if !installable(binary) {
        return (
            None,
            format!(
                "{} is not an installed build, so {} was not checked",
                binary.display(),
                link.display()
            ),
        );
    }
    let (found, fixed) = if fix {
        match ensure(env, binary) {
            Ok(found) => {
                let fixed = matches!(found, State::Missing | State::Stale(_));
                (found, fixed)
            }
            Err(error) => return (Some(false), format!("could not fix: {error:#}")),
        }
    } else {
        (state(env, binary), false)
    };
    let mut detail = match &found {
        State::Foreign(what) => {
            return (
                None,
                format!(
                    "{} is {what}, so it was left alone; move it away and run `doctor --fix` to install this binary there",
                    link.display()
                ),
            );
        }
        State::Missing if !fix => {
            return (
                None,
                format!(
                    "{} is missing; `doctor --fix` (or restarting Herdr) installs it",
                    link.display()
                ),
            );
        }
        State::Stale(old) if !fix => {
            return (
                None,
                format!(
                    "{} refers to {}, not this binary; `doctor --fix` (or restarting Herdr) refreshes it",
                    link.display(),
                    old.display()
                ),
            );
        }
        _ if fixed => format!("fixed: {} now runs this binary", link.display()),
        _ => format!("{} runs this binary", link.display()),
    };
    let dir = bin_dir(env);
    if !std::env::split_paths(path_var).any(|d| d == dir) {
        #[cfg(unix)]
        detail.push_str(&format!(
            "; but {} is not on your PATH: add `export PATH=\"{}:$PATH\"` to your shell profile",
            dir.display(),
            dir.display()
        ));
        #[cfg(windows)]
        detail.push_str(&format!("; but {} is not on your PATH: add it to your user Path in Windows Environment Variables", dir.display()));
        return (None, detail);
    }
    match resolves_to(env, path_var) {
        Some(first)
            if crate::paths::canonicalize(&first).ok()
                != crate::paths::canonicalize(&link).ok()
                && crate::paths::canonicalize(&first).ok()
                    != crate::paths::canonicalize(binary).ok() =>
        {
            detail.push_str(&format!(
                "; but your PATH finds {} first, which is another binary",
                first.display()
            ));
            (None, detail)
        }
        _ => (Some(true), detail),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn setup() -> (tempfile::TempDir, Env, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let binary = home
            .path()
            .join(".config/herdr/plugins/github/herdr-projects-abc/target/release/herdr-projects");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        (home, env, binary)
    }

    fn path_with(env: &Env) -> String {
        bin_dir(env).display().to_string() + ":/usr/bin"
    }

    #[test]
    fn a_missing_link_is_made_and_then_ours() {
        let (_home, env, binary) = setup();
        assert_eq!(ensure(&env, &binary).unwrap(), State::Missing);
        assert_eq!(std::fs::read_link(link_path(&env)).unwrap(), binary);
        assert_eq!(ensure(&env, &binary).unwrap(), State::Ours);
        let (ok, detail) = check(&env, &binary, &path_with(&env), false);
        assert_eq!(ok, Some(true), "{detail}");
    }

    #[test]
    fn a_dangling_link_or_one_into_another_plugin_install_is_replaced() {
        let (home, env, binary) = setup();
        std::fs::create_dir_all(bin_dir(&env)).unwrap();
        std::os::unix::fs::symlink(home.path().join("gone/herdr-projects"), link_path(&env))
            .unwrap();
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Stale(_)));
        assert_eq!(std::fs::read_link(link_path(&env)).unwrap(), binary);

        let other = home
            .path()
            .join(".config/herdr/plugins/github/herdr-projects-old/target/release/herdr-projects");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "").unwrap();
        std::fs::remove_file(link_path(&env)).unwrap();
        std::os::unix::fs::symlink(&other, link_path(&env)).unwrap();
        let (ok, detail) = check(&env, &binary, &path_with(&env), true);
        assert_eq!(ok, Some(true), "{detail}");
        assert!(detail.starts_with("fixed:"), "{detail}");
        assert_eq!(std::fs::read_link(link_path(&env)).unwrap(), binary);
    }

    #[test]
    fn a_file_or_a_link_to_the_users_own_checkout_is_never_touched() {
        let (home, env, binary) = setup();
        std::fs::create_dir_all(bin_dir(&env)).unwrap();
        std::fs::write(link_path(&env), "mine").unwrap();
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Foreign(_)));
        assert_eq!(std::fs::read_to_string(link_path(&env)).unwrap(), "mine");
        let (ok, detail) = check(&env, &binary, &path_with(&env), true);
        assert_eq!(ok, None);
        assert!(detail.contains("left alone"), "{detail}");

        std::fs::remove_file(link_path(&env)).unwrap();
        let own = home
            .path()
            .join("dev/herdr-projects/target/release/herdr-projects");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, "").unwrap();
        std::os::unix::fs::symlink(&own, link_path(&env)).unwrap();
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Foreign(_)));
        assert_eq!(std::fs::read_link(link_path(&env)).unwrap(), own);
    }

    #[test]
    fn a_bin_dir_missing_from_path_is_a_warning_with_the_fix() {
        let (_home, env, binary) = setup();
        let (ok, detail) = check(&env, &binary, "/usr/bin", true);
        assert_eq!(ok, None);
        assert!(
            detail.contains("is not on your PATH: add `export PATH="),
            "{detail}"
        );
        assert!(link_path(&env).is_symlink());
    }

    #[test]
    fn another_binary_earlier_on_path_is_named() {
        let (home, env, binary) = setup();
        let early = home.path().join("early");
        std::fs::create_dir_all(&early).unwrap();
        std::fs::write(early.join(NAME), "").unwrap();
        std::fs::set_permissions(early.join(NAME), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", early.display(), path_with(&env));
        let (ok, detail) = check(&env, &binary, &path, true);
        assert_eq!(ok, None);
        assert!(
            detail.contains("finds") && detail.contains("early"),
            "{detail}"
        );
    }

    #[test]
    fn xdg_bin_home_wins_and_debug_builds_are_never_linked() {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[("XDG_BIN_HOME", "/xdg/bin")]);
        assert_eq!(link_path(&env), PathBuf::from("/xdg/bin/herdr-projects"));
        let (ok, _) = check(
            &env,
            Path::new("/src/target/debug/herdr-projects"),
            "",
            true,
        );
        assert_eq!(ok, None);
        assert!(!Path::new("/xdg/bin").exists());
    }

    /// Runs scripts/link-command.sh for `checkout` with `home` as HOME.
    fn run_script(home: &Path, checkout: &Path) -> String {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/link-command.sh");
        let out = std::process::Command::new("sh")
            .arg(script)
            .arg(checkout)
            .env_clear()
            .env("HOME", home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", home.join(".local/bin").display()),
            )
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    #[test]
    fn the_install_script_links_where_herdr_will_move_the_checkout() {
        let home = tempfile::tempdir().unwrap();
        let plugins = home.path().join(".config/herdr/plugins");
        let checkout = plugins.join(".tmp-install-12-34/checkout");
        let link = home.path().join(".local/bin/herdr-projects");
        // Herdr's folder for plugin id `herdr-projects`: slug plus sha256's first 12 hex digits.
        let final_binary =
            plugins.join("github/herdr-projects-b1278ffb803c/target/release/herdr-projects");

        // Anywhere but a Herdr install checkout: nothing.
        run_script(home.path(), &home.path().join("dev/herdr-projects"));
        assert!(std::fs::symlink_metadata(&link).is_err());

        let said = run_script(home.path(), &checkout);
        assert_eq!(std::fs::read_link(&link).unwrap(), final_binary, "{said}");
        assert!(said.contains("linked"), "{said}");

        // A dangling link elsewhere is replaced; a working one is left alone.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(home.path().join("gone"), &link).unwrap();
        run_script(home.path(), &checkout);
        assert_eq!(std::fs::read_link(&link).unwrap(), final_binary);
        let own = home.path().join("own");
        std::fs::write(&own, "").unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&own, &link).unwrap();
        assert!(run_script(home.path(), &checkout).contains("left"));
        assert_eq!(std::fs::read_link(&link).unwrap(), own);

        // A file is never touched.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "mine").unwrap();
        assert!(run_script(home.path(), &checkout).contains("not a link"));
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "mine");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Env, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let env = Env::for_test(home.path(), &[]);
        let binary = env
            .herdr_config_dir()
            .join("plugins/github/项目 plugin/target/release")
            .join(FILE_NAME);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"first build").unwrap();
        (home, env, binary)
    }

    #[test]
    fn command_copy_tracks_source_updates_without_symlinks() {
        let (_home, env, binary) = setup();
        assert!(installable(&binary));
        assert_eq!(ensure(&env, &binary).unwrap(), State::Missing);
        let command = link_path(&env);
        assert!(!command.is_symlink());
        assert_eq!(std::fs::read(&command).unwrap(), b"first build");
        assert_eq!(
            installed_source(&command).unwrap(),
            crate::paths::canonicalize(&binary).unwrap()
        );
        assert_eq!(ensure(&env, &binary).unwrap(), State::Ours);

        std::fs::write(&binary, b"second build").unwrap();
        assert!(matches!(state(&env, &binary), State::Stale(_)));
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Stale(_)));
        assert_eq!(std::fs::read(&command).unwrap(), b"second build");
        assert_eq!(ensure(&env, &binary).unwrap(), State::Ours);
        let path = std::env::join_paths([bin_dir(&env)])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (ok, detail) = check(&env, &binary, &path, false);
        assert_eq!(ok, Some(true), "{detail}");
    }

    #[test]
    fn foreign_or_user_modified_commands_are_preserved() {
        let (_home, env, binary) = setup();
        std::fs::create_dir_all(bin_dir(&env)).unwrap();
        std::fs::write(link_path(&env), b"mine").unwrap();
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Foreign(_)));
        assert_eq!(std::fs::read(link_path(&env)).unwrap(), b"mine");
        std::fs::remove_file(link_path(&env)).unwrap();
        ensure(&env, &binary).unwrap();
        std::fs::write(link_path(&env), b"my replacement").unwrap();
        assert!(matches!(ensure(&env, &binary).unwrap(), State::Foreign(_)));
        assert_eq!(std::fs::read(link_path(&env)).unwrap(), b"my replacement");
        assert!(installed_source(&link_path(&env)).is_none());
    }

    #[test]
    fn reinstalling_identical_bytes_updates_the_source_checkout() {
        let (home, env, binary) = setup();
        ensure(&env, &binary).unwrap();
        let moved = home
            .path()
            .join("new plugin/target/release")
            .join(FILE_NAME);
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::copy(&binary, &moved).unwrap();
        assert!(matches!(ensure(&env, &moved).unwrap(), State::Stale(_)));
        assert_eq!(
            installed_source(&link_path(&env)).unwrap(),
            crate::paths::canonicalize(&moved).unwrap()
        );
    }

    #[test]
    fn native_install_script_refreshes_owned_command_and_preserves_foreign_copies() {
        let system_root = std::env::var_os("SystemRoot").expect("Windows has SystemRoot");
        let mut shells =
            vec![Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe")];
        if let Some(pwsh) = crate::profiles::find_executable_on_path(
            &std::env::var("PATH").unwrap_or_default(),
            Some(".EXE"),
            "pwsh.exe",
        ) {
            shells.push(pwsh);
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/link-command.ps1");
        for shell in shells {
            let home = tempfile::tempdir().unwrap();
            let env = Env::for_test(home.path(), &[]);
            let plugins = env.herdr_config_dir().join("plugins");
            let checkout = plugins.join(".tmp-install-12-34/checkout");
            let source = checkout.join("target/release/herdr-projects.exe");
            let binary = plugins
                .join("github/herdr-projects-b1278ffb803c/target/release/herdr-projects.exe");
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            let command = link_path(&env);
            let marker = marker_path(&command);
            let script_command = || {
                let mut process = std::process::Command::new(&shell);
                process
                    .env_clear()
                    .env("SystemRoot", &system_root)
                    .env("HOME", home.path())
                    .env("TEMP", home.path())
                    .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
                    .env("XDG_BIN_HOME", bin_dir(&env))
                    .env("PATH", bin_dir(&env))
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                    ])
                    .arg(&script)
                    .arg(&checkout);
                process
            };
            let run = || {
                let out = script_command().output().unwrap();
                assert!(
                    out.status.success(),
                    "{}: {}",
                    shell.display(),
                    String::from_utf8_lossy(&out.stderr)
                );
            };

            for build in [b"first build".as_slice(), b"second build".as_slice()] {
                std::fs::write(&source, build).unwrap();
                std::fs::write(&binary, build).unwrap();
                run();
                assert_eq!(std::fs::read(&command).unwrap(), build);
                assert_eq!(state(&env, &binary), State::Ours);
                assert_eq!(
                    installed_source(&command).unwrap(),
                    crate::paths::canonicalize(&binary).unwrap()
                );
            }

            let marker_before_failure = std::fs::read(&marker).unwrap();
            let original_permissions = std::fs::metadata(&marker).unwrap().permissions();
            let mut permissions = original_permissions.clone();
            permissions.set_readonly(true);
            std::fs::set_permissions(&marker, permissions).unwrap();
            std::fs::write(&source, b"failed PowerShell refresh").unwrap();
            let failed = script_command().output();
            let still_readonly = std::fs::metadata(&marker).unwrap().permissions().readonly();
            std::fs::set_permissions(&marker, original_permissions).unwrap();
            assert!(!failed.unwrap().status.success());
            assert!(still_readonly);
            assert_eq!(std::fs::read(&command).unwrap(), b"second build");
            assert_eq!(std::fs::read(&marker).unwrap(), marker_before_failure);

            let rust_binary = home.path().join("other/target/release/herdr-projects.exe");
            std::fs::create_dir_all(rust_binary.parent().unwrap()).unwrap();
            std::fs::write(&rust_binary, b"Rust refresh").unwrap();
            std::fs::write(&source, b"PowerShell refresh").unwrap();
            std::fs::write(&binary, b"PowerShell refresh").unwrap();
            let before = std::fs::read(&command).unwrap();
            let before_marker = std::fs::read(&marker).unwrap();
            std::thread::scope(|scope| {
                use std::sync::mpsc::{RecvTimeoutError, channel};
                use std::time::Duration;
                let held = transaction_lock(&command).unwrap();
                let (started, ready) = channel();
                let (rust_done, rust_completed) = channel();
                let (script_done, script_completed) = channel();
                let writer_env = &env;
                let writer_binary = &rust_binary;
                let rust_writer = scope.spawn(move || {
                    started.send(()).unwrap();
                    let result = ensure(writer_env, writer_binary);
                    rust_done.send(()).unwrap();
                    result
                });
                let child = script_command()
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let native_writer = scope.spawn(move || {
                    let output = child.wait_with_output().unwrap();
                    script_done.send(()).unwrap();
                    output
                });
                ready.recv().unwrap();
                assert!(matches!(
                    rust_completed.recv_timeout(Duration::from_millis(250)),
                    Err(RecvTimeoutError::Timeout)
                ));
                assert!(matches!(
                    script_completed.recv_timeout(Duration::from_secs(2)),
                    Err(RecvTimeoutError::Timeout)
                ));
                assert_eq!(std::fs::read(&command).unwrap(), before);
                assert_eq!(std::fs::read(&marker).unwrap(), before_marker);
                drop(held);
                assert!(matches!(
                    rust_writer.join().unwrap().unwrap(),
                    State::Stale(_)
                ));
                let output = native_writer.join().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            });
            let installed = installed_source(&command).unwrap();
            assert!(
                installed == crate::paths::canonicalize(&binary).unwrap()
                    || installed == crate::paths::canonicalize(&rust_binary).unwrap()
            );
            assert_eq!(
                std::fs::read(&command).unwrap(),
                std::fs::read(&installed).unwrap()
            );
            assert_eq!(state(&env, &installed), State::Ours);

            // Neither writer may overwrite an unrelated token, or publish while rejecting it.
            let lock = command.with_file_name(".herdr-projects-command.lock");
            std::fs::write(&lock, b"foreign lock file").unwrap();
            let saved_command = std::fs::read(&command).unwrap();
            let marker_before_lock_failure = std::fs::read(&marker).unwrap();
            std::fs::write(&rust_binary, b"blocked Rust refresh").unwrap();
            assert!(ensure(&env, &rust_binary).is_err());
            assert!(!script_command().output().unwrap().status.success());
            assert_eq!(std::fs::read(&lock).unwrap(), b"foreign lock file");
            assert_eq!(std::fs::read(&command).unwrap(), saved_command);
            assert_eq!(std::fs::read(&marker).unwrap(), marker_before_lock_failure);
            std::fs::write(&lock, LOCK_SIGNATURE).unwrap();

            let saved_marker = std::fs::read(&marker).unwrap();
            std::fs::write(&command, b"tampered").unwrap();
            run();
            assert_eq!(std::fs::read(&command).unwrap(), b"tampered");
            assert_eq!(std::fs::read(&marker).unwrap(), saved_marker);
            std::fs::remove_file(&marker).unwrap();
            std::fs::write(&command, b"foreign").unwrap();
            run();
            assert_eq!(std::fs::read(&command).unwrap(), b"foreign");
            assert!(!marker.exists());
        }
    }

    #[test]
    fn failed_marker_publication_restores_the_previous_command() {
        let (_home, env, binary) = setup();
        ensure(&env, &binary).unwrap();
        let marker = marker_path(&link_path(&env));
        let original_permissions = std::fs::metadata(&marker).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&marker, permissions).unwrap();
        std::fs::write(&binary, b"next build").unwrap();
        let result = ensure(&env, &binary);
        std::fs::set_permissions(&marker, original_permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(link_path(&env)).unwrap(), b"first build");
        assert!(matches!(state(&env, &binary), State::Stale(_)));
        ensure(&env, &binary).unwrap();
        assert_eq!(std::fs::read(link_path(&env)).unwrap(), b"next build");
    }

    #[test]
    fn doctor_preserves_managed_command_when_path_is_shadowed_or_missing() {
        let (home, env, binary) = setup();
        let early = home.path().join("earlier path");
        std::fs::create_dir(&early).unwrap();
        std::fs::write(early.join("herdr-projects.cmd"), b"@echo off").unwrap();
        let shadow = early.join("herdr-projects.cmd");
        let path = std::env::join_paths([early, bin_dir(&env)])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (ok, _) = check(&env, &binary, &path, true);
        assert_eq!(ok, None);
        assert_eq!(
            crate::paths::canonicalize(resolves_to(&env, &path).unwrap()).unwrap(),
            crate::paths::canonicalize(&shadow).unwrap()
        );
        assert_eq!(state(&env, &binary), State::Ours);
        let (ok, _) = check(&env, &binary, "", false);
        assert_eq!(ok, None);
        assert_eq!(std::fs::read(link_path(&env)).unwrap(), b"first build");
    }
}

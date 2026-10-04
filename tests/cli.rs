//! End-to-end checks of the built binary with a scrubbed environment.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_herdr-projects");

fn hp(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_clear()
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn context_prints_a_usable_prefix_in_a_scrubbed_environment() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("my root's café");
    let root_arg = root.to_str().unwrap();
    assert!(hp(home.path(), &["--root", root_arg, "new", "Demo"]).status.success());

    let out = hp(home.path(), &["--root", root_arg, "context", "demo", "--peek"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let prefix = text.lines().next().unwrap().strip_prefix("Commands: ").unwrap();

    // Execute the printed command, including quoted paths, in the native shell.
    #[cfg(unix)]
    let mut shell = {
        let mut command = Command::new("/bin/sh");
        command.env_clear().arg("-c");
        command
    };
    #[cfg(windows)]
    let mut shell = {
        let system_root = std::env::var_os("SystemRoot").expect("Windows has SystemRoot");
        let mut command = Command::new(
            Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
        );
        command
            .env_clear()
            .env("SystemRoot", system_root)
            // Without PATHEXT, PowerShell opens .exe files as documents instead
            // of capturing native-command output and an exit code.
            .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
            .env("TEMP", home.path())
            .args(["-NoProfile", "-NonInteractive", "-Command"]);
        command
    };
    let listed = shell
        .env("HOME", home.path())
        .arg(format!("{prefix} list"))
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "demo\tactive\tno threads\n");
}

#[test]
fn peek_records_nothing_and_context_records_seen_items() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("root");
    let root_arg = root.to_str().unwrap();
    assert!(hp(home.path(), &["--root", root_arg, "new", "demo"]).status.success());
    let item = "+++\nid = \"20260917T000000Z-routine-r-1\"\nkind = \"routine\"\nsubject = \"r\"\ncreated = \"x\"\nsummary = \"s\"\n+++\n";
    std::fs::write(root.join("demo/inbox/20260917T000000Z-routine-r-1.md"), item).unwrap();
    let seen = root.join("demo/.state/inbox-seen.json");

    assert!(hp(home.path(), &["--root", root_arg, "context", "demo", "--peek"]).status.success());
    assert!(!seen.exists());
    assert!(hp(home.path(), &["--root", root_arg, "context", "demo"]).status.success());
    assert!(std::fs::read_to_string(&seen).unwrap().contains("routine-r-1"));
}

#[test]
fn path_like_names_and_slugs_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("root");
    let root_arg = root.to_str().unwrap();
    assert!(!hp(home.path(), &["--root", root_arg, "new", "../x"]).status.success());
    assert!(!hp(home.path(), &["--root", root_arg, "open", "../x"]).status.success());
    assert!(!hp(home.path(), &["--root", root_arg, "context", "../x"]).status.success());
    assert!(!hp(home.path(), &["--root", root_arg, "thread", "list", "../x"]).status.success());
    assert!(!hp(home.path(), &["--root", root_arg, "delete", "../x", "--force"]).status.success());
    assert!(!root.exists());
    assert!(!home.path().join("x").exists());
}

#[test]
fn ticker_start_without_projects_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert!(hp(home.path(), &["ticker", "start"]).status.success());
    assert!(!home.path().join(".herdr-projects").exists());
    assert!(!home.path().join(".config").exists());
}

#[cfg(windows)]
#[test]
fn windows_uses_userprofile_when_home_is_unset() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(BIN)
            .env_clear()
            .env("USERPROFILE", home.path())
            .args(args)
            .output()
            .unwrap()
    };
    let created = run(&["new", "Native Home"]);
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
    assert!(home.path().join(".herdr-projects/native-home/PROJECT.md").is_file());
    let listed = run(&["list"]);
    assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "native-home\tactive\tno threads\n");
}

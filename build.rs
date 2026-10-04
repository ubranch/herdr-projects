use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// No rerun-if-changed lines on purpose: cargo then reruns this script whenever
// any file in the package changes, so a rebuilt binary always gets a new build id.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // Debug Clap command builders need over 1 MiB of live stack before parsing.
        // Reserve 2 MiB for this binary only; Windows commits pages as needed.
        println!("cargo:rustc-link-arg-bin=herdr-projects=/STACK:2097152");
    }

    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nogit".to_string());
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=HP_BUILD_ID={hash}.{secs}");
}

//! Embeds the git commit and the build time so the log header can say which build wrote it.
//! Std only; every failure falls back to "unknown" (a build from a source zip has no git).
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=COOP_GIT_HASH={hash}");
    println!("cargo:rustc-env=COOP_BUILD_EPOCH={secs}");
}

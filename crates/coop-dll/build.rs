//! Embeds the git commit and the build time so the log header can say which build wrote it,
//! and a Windows VERSIONINFO resource (product, author, version from `ui.rs` MOD_VERSION) so
//! the DLL's file properties say what it is.
//! Std only; every failure falls back to "unknown" (a build from a source zip has no git) or,
//! for the resource, to no resource (rc.exe comes with the vcvars64 environment of build.cmd).
use std::path::PathBuf;
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
    version_resource();
}

/// `pub const MOD_VERSION: &str = "1.0.2-beta";` -> "1.0.2-beta".
fn mod_version() -> Option<String> {
    let ui = std::fs::read_to_string("src/ui.rs").ok()?;
    let line = ui.lines().find(|l| l.starts_with("pub const MOD_VERSION"))?;
    Some(line.split('"').nth(1)?.to_owned())
}

/// "1.0.2-beta" -> "1,0,2,0" (the numeric part, padded to four fields).
fn numeric_version(v: &str) -> String {
    let mut parts: Vec<u16> = v
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .take(4)
        .collect();
    parts.resize(4, 0);
    parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

fn version_resource() {
    let (Some(version), Ok(out)) = (mod_version(), std::env::var("OUT_DIR")) else { return };
    let out = PathBuf::from(out);
    let num = numeric_version(&version);
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {num}
PRODUCTVERSION {num}
FILEOS 0x40004
FILETYPE 0x2
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Tihvert"
      VALUE "FileDescription", "Sekiro Seamless Co-op (me3 mod)"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "sekiro_coop"
      VALUE "LegalCopyright", "MIT, https://github.com/Tihvert/sekiro-coop"
      VALUE "OriginalFilename", "sekiro_coop.dll"
      VALUE "ProductName", "Sekiro Seamless Co-op"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out.join("version.rc");
    let res_path = out.join("version.res");
    if std::fs::write(&rc_path, rc).is_err() {
        return;
    }
    let ok = Command::new("rc.exe")
        .args(["/nologo", "/fo"])
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-cdylib={}", res_path.display());
    } else {
        println!("cargo:warning=rc.exe not found or failed: the DLL gets no version resource");
    }
}

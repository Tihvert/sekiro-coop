//! The launcher scripts next to the DLL, kept current by the DLL itself (v8.53).
//!
//! A friend's old update.ps1 may only know the DLL (the legacy path without a manifest), so the
//! DLL arrived but play.cmd / update.ps1 / upload-log.ps1 / laheta-lokit.cmd stayed old: no save
//! upload, no laheta-lokit.cmd (2026-10-27). The DLL always arrives, so it carries the current
//! copies (`ship\` in the repo, the same files release.cmd puts in the zip) and rewrites any that
//! differ in its own folder. Only a folder that holds `sekiro-coop.me3` (a mod folder) is touched.
//! play.cmd runs while the game does (cmd reads it by offset), so it goes to `play.cmd.new`, which
//! play.cmd or upload-log.ps1 swaps in after the game has closed.

use std::path::Path;

use crate::log;

/// (name in the folder, contents). play.cmd is written as play.cmd.new (see above).
const FILES: &[(&str, &[u8])] = &[
    ("update.ps1", include_bytes!("../../../ship/update.ps1")),
    ("upload-log.ps1", include_bytes!("../../../ship/upload-log.ps1")),
    ("laheta-lokit.cmd", include_bytes!("../../../ship/laheta-lokit.cmd")),
    ("stop-me3.ps1", include_bytes!("../../../ship/stop-me3.ps1")),
    ("sekiro-coop.me3", include_bytes!("../../../ship/sekiro-coop.me3")),
    ("OHJE.txt", include_bytes!("../../../ship/OHJE.txt")),
    ("play.cmd", include_bytes!("../../../ship/play.cmd")),
];

/// Where a file goes: play.cmd as play.cmd.new, the rest under their own names.
fn target_name(name: &str) -> String {
    if name == "play.cmd" { "play.cmd.new".to_string() } else { name.to_string() }
}

/// The files to write in `dir`: those whose current copy differs. play.cmd counts as current
/// when play.cmd itself (or a waiting play.cmd.new) already matches.
fn stale(dir: &Path) -> Vec<(&'static str, &'static [u8])> {
    FILES
        .iter()
        .filter(|(name, data)| {
            let same = |n: &str| std::fs::read(dir.join(n)).is_ok_and(|b| b == *data);
            if *name == "play.cmd" { !same("play.cmd") && !same("play.cmd.new") } else { !same(name) }
        })
        .copied()
        .collect()
}

/// A tester folder (release.cmd's dist or the friends' zip) holds update.ps1 or test_server.txt.
/// The public zip (v8.62) has neither: its own English play.cmd stays and no Finnish tester
/// scripts appear next to it.
fn is_tester_folder(dir: &Path) -> bool {
    dir.join("update.ps1").exists() || dir.join("test_server.txt").exists()
}

/// Rewrites the stale launcher files next to the DLL (own thread, never blocks the game).
pub fn start() {
    std::thread::spawn(|| {
        let Some(dir) = crate::settings::own_dll_path().and_then(|p| p.parent().map(Path::to_path_buf)) else {
            return;
        };
        if !dir.join("sekiro-coop.me3").exists() {
            log::line(&format!("shipfiles: {} is not a mod folder (no sekiro-coop.me3), nothing written", dir.display()));
            return;
        }
        if !is_tester_folder(&dir) {
            log::line("shipfiles: public build folder (no update.ps1 / test_server.txt), launcher files left alone");
            return;
        }
        let todo = stale(&dir);
        if todo.is_empty() {
            log::line(&format!("shipfiles: all {} launcher files are current", FILES.len()));
            return;
        }
        for (name, data) in todo {
            let target = dir.join(target_name(name));
            let tmp = dir.join(format!("{}.tmp", target_name(name)));
            let r = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &target));
            match r {
                Ok(()) => log::line(&format!("shipfiles: updated {} ({} B)", target_name(name), data.len())),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    log::line(&format!("shipfiles: could not write {}: {e}", target_name(name)));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_stale_files_and_play_cmd_as_new() {
        let dir = std::env::temp_dir().join(format!("shipfiles-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_tester_folder(&dir), "an empty (public) folder is not a tester folder");
        std::fs::write(dir.join("update.ps1"), b"old").unwrap();
        assert!(is_tester_folder(&dir));
        std::fs::write(dir.join("play.cmd"), b"old").unwrap();
        let names: Vec<_> = stale(&dir).iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), FILES.len(), "all differ or are missing");
        assert_eq!(target_name("play.cmd"), "play.cmd.new");
        // A matching play.cmd (or play.cmd.new) is current.
        let play = FILES.iter().find(|(n, _)| *n == "play.cmd").unwrap().1;
        std::fs::write(dir.join("play.cmd.new"), play).unwrap();
        assert!(!stale(&dir).iter().any(|(n, _)| *n == "play.cmd"));
        for (name, data) in FILES.iter().filter(|(n, _)| *n != "play.cmd") {
            std::fs::write(dir.join(name), data).unwrap();
        }
        assert!(stale(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

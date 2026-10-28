//! `coop-audit <host log> <friend log> [<friend log> ...] [-o report.txt] [--all] [--offset S]`
//!
//! Reads the logs and writes a short Finnish + English report ranked by severity (stdout
//! when no `-o`). `--all`: the whole host log (default: the friend logs' time span +-10 min).
//! `--offset S`: the first friend's clock minus the host clock in seconds (default: estimated).
//! One friend log: the 2-player report. More (3-6 players, PLAN-MULTIPLAYER §5.19): the first
//! is the reference; the others join when they overlap it in time (one per player, the name
//! from serve.ps1's `log-<name>-<time>.txt`), and the report covers host <-> each friend plus
//! client -> client grants.

use std::process::ExitCode;

fn read(path: &str) -> Result<String, String> {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("cannot read {path}: {e}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files = Vec::new();
    let mut out: Option<String> = None;
    let mut opt = coop_audit::Options::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                out = args.get(i).cloned();
            }
            "--all" => opt.all = true,
            "--offset" => {
                i += 1;
                opt.offset = args.get(i).and_then(|s| s.parse().ok());
            }
            a => files.push(a.to_owned()),
        }
        i += 1;
    }
    if files.len() < 2 {
        eprintln!("usage: coop-audit <host log> <friend log> [<friend log> ...] [-o report.txt] [--all] [--offset S]");
        return ExitCode::from(2);
    }
    let mut texts = Vec::new();
    for f in &files {
        match read(f) {
            Ok(t) => texts.push(t),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    }
    let (text, n) = if files.len() == 2 {
        let a = coop_audit::analyze(&texts[0], &texts[1], &opt);
        (coop_audit::render(&a, &files[0], &files[1]), a.items.len())
    } else {
        let friends: Vec<coop_audit::FriendLog> = files[1..]
            .iter()
            .zip(&texts[1..])
            .map(|(p, t)| coop_audit::FriendLog { label: coop_audit::friend_label(p), path: p.clone(), text: t })
            .collect();
        let s = coop_audit::analyze_session(&texts[0], &friends, &opt);
        let n = if let [p] = s.pairs.as_slice() { p.analysis.items.len() } else { s.items.len() };
        (coop_audit::render_session(&s, &files[0]), n)
    };
    match out {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, text.replace('\n', "\r\n")) {
                eprintln!("cannot write {p}: {e}");
                return ExitCode::from(1);
            }
            println!("{n} findings -> {p}");
        }
        None => print!("{text}"),
    }
    ExitCode::SUCCESS
}

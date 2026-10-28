//! Offline analyzer of one co-op session: the host's `log.txt` and the friend's uploaded log
//! (IMPLEMENTATION-NOTES §32). Finds crashes, panics, the in-mod auditor's SUSPECT lines,
//! reward hand-overs that were sent and never given, flag ping-pong, loss counters, version
//! mismatches, disconnects, long fps drops and boss records restored damaged, and writes a
//! short Finnish + English report ranked by severity, with timestamps and log line numbers.
//!
//! Only reads the files. Log lines look like `[unix seconds] module: text`.
//!
//! 3-6 players (PLAN-MULTIPLAYER §5.19): `analyze_session` takes the host log and every
//! friend log of the session (the first one = the upload that asked for the report; the
//! others are kept when their time span overlaps it, one per player), analyses each
//! host <-> friend pair as above and checks every client's grants against every other
//! client's log (the host relays them). One friend log: exactly the 2-player report.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Host,
    Friend,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Host => "host",
            Side::Friend => "friend",
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Host => Side::Friend,
            Side::Friend => Side::Host,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    /// 1-based line number in the file.
    pub no: usize,
    /// Unix seconds of the line's own clock (the last stamp seen for lines without one).
    pub t: u64,
    pub text: &'a str,
}

/// `[1792751473] key items: ...` -> lines with time and text.
pub fn parse(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut last = 0;
    for (i, raw) in text.lines().enumerate() {
        let raw = raw.trim_end_matches('\r');
        if raw.trim().is_empty() {
            continue;
        }
        let (t, body) = match raw.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((n, rest)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => (n.parse().unwrap_or(last), rest.trim_start()),
            _ => (last, raw),
        };
        last = t;
        out.push(Line { no: i + 1, t, text: body });
    }
    out
}

/// Report sections, in the order they are listed within one severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cat {
    Crash,
    Panic,
    Version,
    RewardLost,
    PingPong,
    AuditSuspect,
    KeyItems,
    Counter,
    Disconnect,
    Fps,
    FlagFlap,
    BossRestore,
    AuditInfo,
}

impl Cat {
    pub fn title(self) -> (&'static str, &'static str) {
        match self {
            Cat::Crash => ("Kaatuminen", "Crash"),
            Cat::Panic => ("Modin paniikki", "Mod panic"),
            Cat::Version => ("Versio / salasana ei tasmaa", "Version / password mismatch"),
            Cat::RewardLost => ("Palkinto ei mennyt perille", "Reward sent but not given"),
            Cat::PingPong => ("Lippu pomppii pelaajien valilla", "Flag ping-pong between players"),
            Cat::AuditSuspect => ("Auditorin epailyt", "Auditor SUSPECT"),
            Cat::KeyItems => ("Avainesine puuttuu toiselta", "Key item missing on one side"),
            Cat::Counter => ("Hukattuja / luovutettuja laskureita", "Lost / given-up counters"),
            Cat::Disconnect => ("Yhteyskatkot", "Disconnects"),
            Cat::Fps => ("Pitka FPS-pudotus", "Long fps drop"),
            Cat::FlagFlap => ("Lippu vaihtuu edestakaisin", "Flag flapping"),
            Cat::BossRestore => ("Pomo palautettiin vahingoittuneena", "Boss restored damaged"),
            Cat::AuditInfo => ("Auditorin huomiot", "Auditor info"),
        }
    }
}

pub fn sev_label(s: u8) -> &'static str {
    match s {
        5 => "KRIITTINEN / CRITICAL",
        4 => "VAKAVA / HIGH",
        3 => "EPAILYTTAVA / SUSPECT",
        2 => "HUOMIO / NOTE",
        _ => "TIETO / INFO",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub sev: u8,
    pub cat: Cat,
    pub side: Side,
    /// The line's own time (its log's clock).
    pub t: u64,
    pub line: usize,
    pub text: String,
    /// The same text seen this many times (dedupe).
    pub count: u32,
    /// Whose log the line is in: "host" / "friend" (2 players), "friend 'name'" (3+).
    pub who: String,
}

#[derive(Default)]
pub struct Options {
    /// Analyse the whole host log, not only the friend log's time span (+-10 min).
    pub all: bool,
    /// Friend clock minus host clock in seconds, if known (else estimated).
    pub offset: Option<i64>,
}

pub struct Analysis {
    pub items: Vec<Item>,
    /// Friend clock minus host clock (seconds).
    pub offset: i64,
    /// Flag write pairs the offset was estimated from (0 = assumed 0 or given).
    pub offset_pairs: usize,
    /// Per side: (lines analysed, first time, last time).
    pub spans: [(usize, u64, u64); 2],
}

fn find_num(s: &str, pat: &str) -> Option<i64> {
    let rest = &s[s.find(pat)? + pat.len()..];
    let end = rest.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-'))).map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { format!("{}...", s.chars().take(n).collect::<String>()) }
}

/// Unix seconds -> "2026-10-24 01:23:45 UTC".
pub fn utc(t: u64) -> String {
    let days = (t / 86_400) as i64;
    let s = t % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
}

/// `world: sent flag ID = V` (ours) / `world: friend set flag ID = V` (the friend's, applied).
fn flag_event(text: &str) -> Option<(u32, bool, bool)> {
    let (ours, rest) = if let Some(r) = text.strip_prefix("world: sent flag ") {
        (true, r)
    } else if let Some(r) = text.strip_prefix("world: friend set flag ") {
        (false, r)
    } else {
        return None;
    };
    let (id, v) = rest.split_once(" = ")?;
    let on = v.starts_with("true");
    Some((id.trim().parse().ok()?, on, ours))
}

/// Friend clock minus host clock from flag writes seen on both sides (median).
pub fn estimate_offset(host: &[Line], friend: &[Line]) -> (i64, usize) {
    // (id, value) -> times, per (side, ours)
    let index = |lines: &[Line], want_ours: bool| {
        let mut m: HashMap<(u32, bool), Vec<u64>> = HashMap::new();
        for l in lines {
            if let Some((id, on, ours)) = flag_event(l.text)
                && ours == want_ours
            {
                m.entry((id, on)).or_default().push(l.t);
            }
        }
        m
    };
    let mut diffs: Vec<i64> = Vec::new();
    // Host sends -> friend applies (friend later by offset + latency), and the reverse.
    for (sent, got, sign) in [(index(host, true), index(friend, false), 1i64), (index(friend, true), index(host, false), -1i64)] {
        for (k, ts) in &got {
            let Some(ss) = sent.get(k) else { continue };
            for &tg in ts {
                // friend - host
                let best = ss.iter().map(|&s| if sign > 0 { tg as i64 - s as i64 } else { s as i64 - tg as i64 }).min_by_key(|d| d.abs());
                if let Some(d) = best.filter(|d| d.abs() <= 120) {
                    diffs.push(d);
                }
            }
        }
    }
    if diffs.is_empty() {
        return (0, 0);
    }
    diffs.sort_unstable();
    (diffs[diffs.len() / 2], diffs.len())
}

struct Ctx<'a> {
    logs: [&'a [Line<'a>]; 2],
    offset: i64,
    items: Vec<Item>,
    /// What the texts call each side: "host", "friend" (or "friend 'name'" with 3+ players;
    /// client -> client checks: both clients' names).
    names: [String; 2],
}

impl<'a> Ctx<'a> {
    fn lines(&self, s: Side) -> &'a [Line<'a>] {
        self.logs[s as usize]
    }

    /// A time of `side`'s clock on the host clock.
    fn host_t(&self, side: Side, t: u64) -> i64 {
        if side == Side::Friend { t as i64 - self.offset } else { t as i64 }
    }

    /// The other side's log covers host-clock time `t` (+ `after` seconds).
    fn covers(&self, side: Side, t: i64, after: i64) -> bool {
        let l = self.lines(side);
        match (l.first(), l.last()) {
            (Some(a), Some(b)) => self.host_t(side, a.t) <= t && self.host_t(side, b.t) >= t + after,
            _ => false,
        }
    }

    fn push(&mut self, sev: u8, cat: Cat, side: Side, l: &Line, text: String) {
        let who = self.names[side as usize].clone();
        self.items.push(Item { sev, cat, side, t: l.t, line: l.no, text, count: 1, who });
    }
}

fn span(l: &[Line]) -> (usize, u64, u64) {
    (l.len(), l.first().map_or(0, |x| x.t), l.last().map_or(0, |x| x.t))
}

pub fn analyze(host: &str, friend: &str, opt: &Options) -> Analysis {
    analyze_pair(&parse(host), &parse(friend), opt, "friend")
}

/// One host <-> friend pair; `friend` = what the texts call the friend.
fn analyze_pair(host_all: &[Line], friend_lines: &[Line], opt: &Options, friend: &str) -> Analysis {
    let (offset, pairs) = match opt.offset {
        Some(o) => (o, 0),
        None => estimate_offset(host_all, friend_lines),
    };
    // The host log covers many sessions: keep the friend log's span (+-10 min).
    let host_lines: Vec<Line> = match (opt.all, friend_lines.first(), friend_lines.last()) {
        (false, Some(a), Some(b)) => {
            let (lo, hi) = (a.t as i64 - offset - 600, b.t as i64 - offset + 600);
            host_all.iter().copied().filter(|l| (l.t as i64) >= lo && (l.t as i64) <= hi).collect()
        }
        _ => host_all.to_vec(),
    };
    let spans = [span(&host_lines), span(friend_lines)];
    let mut c = Ctx { logs: [&host_lines, friend_lines], offset, items: Vec::new(), names: ["host".to_owned(), friend.to_owned()] };
    for side in [Side::Host, Side::Friend] {
        line_checks(&mut c, side);
        counters(&mut c, side);
        fps(&mut c, side);
        flags(&mut c, side);
        rewards(&mut c, side);
    }
    dedupe(&mut c.items);
    let mut items = c.items;
    items.sort_by(|a, b| b.sev.cmp(&a.sev).then(a.cat.cmp(&b.cat)).then(a.t.cmp(&b.t)).then(a.line.cmp(&b.line)));
    Analysis { items, offset, offset_pairs: pairs, spans }
}

/// The same (side, category, text) once, with a count; the first occurrence stays.
fn dedupe(items: &mut Vec<Item>) {
    let mut seen: HashMap<(Side, Cat, String), usize> = HashMap::new();
    let mut out: Vec<Item> = Vec::new();
    for it in items.drain(..) {
        match seen.get(&(it.side, it.cat, it.text.clone())) {
            Some(&i) => out[i].count += 1,
            None => {
                seen.insert((it.side, it.cat, it.text.clone()), out.len());
                out.push(it);
            }
        }
    }
    *items = out;
}

/// `audit: SUSPECT (sev N) text` (2 players) or `audit: SUSPECT [peer S 'name'] (sev N) text`
/// (3+ players; the tag stays in the text) -> (N, text).
fn audit_suspect(t: &str) -> Option<(u8, String)> {
    let rest = t.strip_prefix("audit: SUSPECT ")?;
    let (tag, rest) = match rest.strip_prefix("[peer ").and_then(|r| r.split_once("] ")) {
        Some((p, r)) => (format!("[peer {p}] "), r),
        None => (String::new(), rest),
    };
    let rest = rest.strip_prefix("(sev ")?;
    let sev = rest.chars().next().and_then(|d| d.to_digit(10)).unwrap_or(2) as u8;
    let text = rest.split_once(") ").map_or(rest, |x| x.1);
    Some((sev, format!("{tag}{text}")))
}

fn line_checks(c: &mut Ctx, side: Side) {
    let lines: Vec<Line> = c.lines(side).to_vec();
    for (i, l) in lines.iter().enumerate() {
        let t = l.text;
        if t.starts_with("CRASH?") {
            let regs = lines.get(i + 1).filter(|n| n.text.starts_with("CRASH regs")).map(|n| format!(" | {}", cut(n.text, 220))).unwrap_or_default();
            c.push(5, Cat::Crash, side, l, format!("{}{regs}", cut(t, 240)));
        } else if t.contains("frame panicked") || t.contains("INIT FAILED") || t.contains("too many panics") || t.contains("panicked at") {
            c.push(5, Cat::Panic, side, l, cut(t, 300));
        } else if let Some((sev, text)) = audit_suspect(t) {
            c.push((sev + 1).clamp(2, 4), Cat::AuditSuspect, side, l, cut(&text, 400));
        } else if let Some(rest) = t.strip_prefix("audit: info: ") {
            c.push(1, Cat::AuditInfo, side, l, cut(rest, 300));
        } else if t.contains("an older build's handshake") {
            c.push(4, Cat::Version, side, l, cut(t, 300));
        } else if t.starts_with("testsync: host has build") {
            // "asking for it" = the test build downloads the host's DLL itself (normal);
            // "run play.cmd" = the friend plays on a different build until it restarts.
            let sev = if t.contains("asking for it") { 2 } else { 3 };
            c.push(sev, Cat::Version, side, l, cut(t, 300));
        } else if t.starts_with("net: host rejected the password") {
            c.push(3, Cat::Version, side, l, t.to_owned());
        } else if (t.starts_with("session: ") && t.ends_with(" left"))
            || t.starts_with("net: peer silent")
            || t.starts_with("net: peer quit")
            || t.contains("closed their world")
        {
            c.push(2, Cat::Disconnect, side, l, t.to_owned());
        } else if t.starts_with("persist: boss ") && t.contains(" restored hp ") {
            // HP 1 = the get-up moment after a deathblow recorded as the boss's state (§35).
            if find_num(t, " restored hp ").is_some_and(|hp| hp <= 1) {
                c.push(3, Cat::BossRestore, side, l, format!("{} (restored at 1 HP: a get-up moment recorded as damage?)", cut(t, 260)));
            } else {
                c.push(1, Cat::BossRestore, side, l, format!("{} (verify it really was damaged this rest cycle)", cut(t, 260)));
            }
        } else if t.starts_with("key items: we own ")
            && let Some(rest) = t.split_once("; we miss [").map(|x| x.1)
            && let Some(list) = rest.split_once(']').map(|x| x.0)
        {
            for name in list.split("\", \"").map(|s| s.trim_matches('"').trim()).filter(|s| !s.is_empty()) {
                // (The line does not say which peer: no name even with 3+ players.)
                let text = format!("the {} misses key item {name} that the {} has", side.label(), side.other().label());
                c.push(3, Cat::KeyItems, side, l, text);
            }
        }
    }
}

/// `lost N`, `given up N`, `dropped N`, `dropped old N`, `gaps skipped N`, `too big N` > 0,
/// one item per (module, counter) with the largest value.
fn counters(c: &mut Ctx, side: Side) {
    const NAMES: [(&str, u8); 6] = [("dropped old", 1), ("given up", 3), ("gaps skipped", 2), ("too big", 2), ("dropped", 2), ("lost", 3)];
    let mut agg: BTreeMap<(String, &str), (u8, u64, usize, Line, u32)> = BTreeMap::new();
    for l in c.lines(side).iter() {
        let t = l.text;
        if t.starts_with("audit:") || t.starts_with("reward:") {
            continue;
        }
        let module = t.split(':').next().unwrap_or("").split(" (").next().unwrap_or("").trim().to_owned();
        if module.is_empty() || module.len() > 32 {
            continue;
        }
        let b = t.as_bytes();
        let mut taken: Vec<(usize, usize)> = Vec::new();
        for (name, sev) in NAMES {
            let mut from = 0;
            while let Some(p) = t[from..].find(name).map(|p| p + from) {
                from = p + name.len();
                let end = p + name.len();
                if p > 0 && b[p - 1].is_ascii_alphabetic() || taken.iter().any(|&(a, z)| p >= a && p < z) {
                    continue;
                }
                let rest = &t[end..];
                let Some(digits) = rest.strip_prefix(' ') else { continue };
                let n: String = digits.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                let Ok(v) = n.parse::<u64>() else { continue };
                taken.push((p, end));
                if v == 0 {
                    continue;
                }
                let e = agg.entry((module.clone(), name)).or_insert((sev, 0, 0, *l, 0));
                e.4 += 1;
                if v > e.1 {
                    e.1 = v;
                    e.2 = l.no;
                }
            }
        }
    }
    for ((module, name), (sev, max, max_line, first, n)) in agg {
        let text = format!(
            "{module}: '{name}' above 0 in {n} line(s), largest {max} (line {max_line}){}",
            if name == "dropped old" { " (old packets dropped on arrival: normal in small numbers)" } else { "" }
        );
        c.push(sev, Cat::Counter, side, &first, text);
    }
}

/// Runs of `coop: tick N fps F` with 5 < F < 20 lasting 10 s or more (loading screens drop
/// to a few fps and are not counted).
fn fps(c: &mut Ctx, side: Side) {
    let mut run: Vec<(Line, u32)> = Vec::new();
    let mut found = Vec::new();
    let lines = c.lines(side);
    fn flush<'a>(run: &mut Vec<(Line<'a>, u32)>, found: &mut Vec<(Line<'a>, u64, u32, u32)>) {
        if run.len() >= 5 {
            let secs = run.last().map_or(0, |x| x.0.t).saturating_sub(run[0].0.t);
            if secs >= 10 {
                let min = run.iter().map(|x| x.1).min().unwrap_or(0);
                let avg = run.iter().map(|x| x.1).sum::<u32>() / run.len() as u32;
                found.push((run[0].0, secs, min, avg));
            }
        }
        run.clear();
    }
    for l in lines {
        let Some(f) = l.text.strip_prefix("coop: tick ").and_then(|r| find_num(r, " fps ")) else { continue };
        if f > 5 && f < 20 {
            run.push((*l, f as u32));
        } else {
            flush(&mut run, &mut found);
        }
    }
    flush(&mut run, &mut found);
    for (l, secs, min, avg) in found {
        c.push(2, Cat::Fps, side, &l, format!("fps below 20 for {secs} s (min {min}, avg {avg})"));
    }
}

/// Flag ping-pong (written alternately by us and the friend, 3 runs within 60 s) and
/// flapping (one side changes it 4 times within 60 s). The world.rs lines are rate-limited
/// (first 30, then every 100th), so this sees only part of the writes.
fn flags(c: &mut Ctx, side: Side) {
    let mut runs: HashMap<u32, Vec<(bool, u64, bool, usize)>> = HashMap::new();
    let mut flips: HashMap<(u32, bool), Vec<(u64, bool)>> = HashMap::new();
    let mut done: HashSet<(u32, u8)> = HashSet::new();
    let lines: Vec<Line> = c.lines(side).to_vec();
    for l in &lines {
        let Some((id, on, ours)) = flag_event(l.text) else { continue };
        let v = runs.entry(id).or_default();
        v.retain(|r| l.t.saturating_sub(r.1) <= 60);
        match v.last_mut() {
            Some(r) if r.0 == ours => {
                r.1 = l.t;
                r.2 = on;
            }
            _ => v.push((ours, l.t, on, l.no)),
        }
        if v.len() >= 3 && done.insert((id, 0)) {
            let seq: Vec<String> = v.iter().map(|r| format!("{}={} (line {})", if r.0 { "us" } else { "friend" }, r.2, r.3)).collect();
            // 7xxxxxxx = NPC talk flags: npcintro.rs keeps some per player on purpose.
            let npc = id / 10_000_000 % 10 == 7;
            let text = format!(
                "flag {id} written alternately by both players within 60 s: {} ({})",
                seq.join(" -> "),
                if npc { "an NPC talk flag: npcintro.rs per-player intro, or a real fight over it?" } else { "a per-player flag?" }
            );
            c.push(if npc { 3 } else { 4 }, Cat::PingPong, side, l, text);
        }
        let f = flips.entry((id, ours)).or_default();
        if f.last().is_none_or(|x| x.1 != on) {
            f.push((l.t, on));
        }
        f.retain(|x| l.t.saturating_sub(x.0) <= 60);
        if f.len() >= 5 && done.insert((id, 1 + ours as u8)) {
            let who = if ours { "we" } else { "the friend" };
            c.push(2, Cat::FlagFlap, side, l, format!("{who} changed flag {id} {} times within 60 s", f.len() - 1));
        }
    }
}

#[derive(Default)]
struct Receiver {
    /// event -> arrival time (host clock)
    arrived: HashMap<i64, i64>,
    /// event -> (time, dropped)
    resolved: HashMap<i64, (i64, bool)>,
    /// lot -> [(time, dropped)]
    resolved_lot: HashMap<i64, Vec<(i64, bool)>>,
    /// Events the friend acked to us (§33 `BULK_GRANT_ACK`).
    acked: HashSet<i64>,
    /// (entity, lot) -> arrival / resolution time
    drop_arrived: HashMap<(i64, i64), i64>,
    drop_resolved: HashMap<(i64, i64), i64>,
    /// Boss reward bulks received, lots given / skipped (host clock).
    reward_arrived: Vec<i64>,
    reward_resolved: Vec<i64>,
    reward_dropped: Vec<i64>,
}

fn receiver(c: &Ctx, side: Side) -> Receiver {
    let mut r = Receiver::default();
    for l in c.lines(side) {
        let t = l.text;
        let ht = c.host_t(side, l.t);
        if !t.starts_with("reward:") {
            continue;
        }
        if t.contains("friend's script lot") && t.contains(" arrived (event ") {
            if let Some(e) = find_num(t, "(event ") {
                r.arrived.entry(e).or_insert(ht);
            }
        } else if t.starts_with("reward: gave the friend's script lot") {
            if let Some(e) = find_num(t, "(event ") {
                r.resolved.insert(e, (ht, false));
            }
        } else if t.starts_with("reward: event ") && t.contains("was already handled") {
            if let Some(e) = find_num(t, "reward: event ") {
                r.resolved.entry(e).or_insert((ht, false));
            }
        } else if let Some(lot) = find_num(t, "friend's lot ") {
            let dropped = t.contains(" dropped");
            r.resolved_lot.entry(lot).or_default().push((ht, dropped));
        } else if t.starts_with("reward: the friend acked lots") {
            if let Some(list) = t.split_once("(events [").and_then(|x| x.1.split_once(']')).map(|x| x.0) {
                r.acked.extend(list.split(',').filter_map(|s| s.trim().parse::<i64>().ok()));
            }
        } else if t.starts_with("reward: the friend's game dropped lot ") {
            if let (Some(lot), Some(e)) = (find_num(t, "dropped lot "), find_num(t, " for ")) {
                r.drop_arrived.entry((e, lot)).or_insert(ht);
            }
        } else if t.starts_with("reward: the friend's drop of ") || t.starts_with("reward: gave the friend's boss drop of ") {
            let e = find_num(t, "drop of ");
            let lot = find_num(t, "(lot ");
            if let (Some(e), Some(lot)) = (e, lot) {
                r.drop_resolved.entry((e, lot)).or_insert(ht);
            }
        } else if t.starts_with("reward: received ") && t.contains("giving them in") {
            r.reward_arrived.push(ht);
        } else if t.starts_with("reward: gave lot ")
            || (t.starts_with("reward: lot ") && (t.contains("already owned") || t.contains("already handled") || t.contains(" not given")))
        {
            r.reward_resolved.push(ht);
        } else if t.starts_with("reward: option off here, dropping the friend's lots") {
            r.reward_dropped.push(ht);
        }
    }
    r
}

/// Grants `side` sent, checked against the other side's log.
fn rewards(c: &mut Ctx, side: Side) {
    let other = side.other();
    let rx = receiver(c, other);
    let own = receiver(c, side);
    let lines: Vec<Line> = c.lines(side).to_vec();
    let to = c.names[other as usize].clone();
    for l in &lines {
        let t = l.text;
        let ht = c.host_t(side, l.t);
        if (t.starts_with("reward: sent our script lot ") || t.starts_with("reward: sent queued grant lot ")) && t.contains("(event ") {
            let (Some(lot), Some(e)) = (find_num(t, " lot "), find_num(t, "(event ")) else { continue };
            if !c.covers(other, ht, 60) {
                continue;
            }
            let by_event = rx.resolved.get(&e).copied();
            let arrived = rx.arrived.get(&e).copied();
            let since = arrived.unwrap_or(ht) - 5;
            let by_lot = rx.resolved_lot.get(&lot).and_then(|v| v.iter().find(|x| x.0 >= since).copied());
            let acked = own.acked.contains(&e);
            match (by_event.or(by_lot), arrived) {
                (Some((_, true)), _) => {
                    let text = format!("script lot {lot} (event {e}) sent to the {to}, dropped there (share_key_items = 0 or item manager)");
                    c.push(3, Cat::RewardLost, side, l, text);
                }
                (Some(_), _) => {}
                (None, _) if acked => {}
                (None, Some(a)) => {
                    let text = format!("script lot {lot} (event {e}) reached the {to} {} s later but its log never gives it (no gave / already line)", a - ht);
                    c.push(4, Cat::RewardLost, side, l, text);
                }
                (None, None) => {
                    let text = format!("script lot {lot} (event {e}) sent to the {to}, never arrived there (no 'arrived' line)");
                    c.push(4, Cat::RewardLost, side, l, text);
                }
            }
        } else if t.starts_with("reward: told the friend about the drop of ") {
            let (Some(e), Some(lot)) = (find_num(t, "drop of "), find_num(t, "(lot ")) else { continue };
            if !c.covers(other, ht, 60) {
                continue;
            }
            match (rx.drop_arrived.get(&(e, lot)), rx.drop_resolved.get(&(e, lot))) {
                (_, Some(_)) => {}
                (Some(a), None) => {
                    let text = format!("boss drop of {e} (lot {lot}) reached the {to} {} s later, never handled there", a - ht);
                    c.push(4, Cat::RewardLost, side, l, text);
                }
                (None, None) => {
                    let text = format!("boss drop of {e} (lot {lot}) announced to the {to}, never arrived there");
                    c.push(4, Cat::RewardLost, side, l, text);
                }
            }
        } else if t.starts_with("reward: sent ") && t.contains(" lots for boss ") {
            let (Some(n), Some(boss)) = (find_num(t, "reward: sent "), find_num(t, " lots for boss ")) else { continue };
            if !c.covers(other, ht, 120) {
                continue;
            }
            let arrival = rx.reward_arrived.iter().copied().find(|&a| a >= ht - 30 && a <= ht + 120);
            match arrival {
                None => {
                    let text = format!("{n} boss reward lot(s) for boss {boss} sent to the {to}, never arrived there (no 'reward: received' line)");
                    c.push(4, Cat::RewardLost, side, l, text);
                }
                Some(a) => {
                    if rx.reward_dropped.iter().any(|&d| d >= a && d <= a + 600) {
                        let text = format!("{n} boss reward lot(s) for boss {boss} dropped by the {to} (boss_reward = 0 there)");
                        c.push(3, Cat::RewardLost, side, l, text);
                    } else if !rx.reward_resolved.iter().any(|&d| d >= a && d <= a + 600) {
                        let text = format!("{n} boss reward lot(s) for boss {boss} reached the {to}, but no lot was given or skipped within 10 min");
                        c.push(4, Cat::RewardLost, side, l, text);
                    }
                }
            }
        }
    }
}

pub fn render(a: &Analysis, host_path: &str, friend_path: &str) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "Sekiro co-op: lokien analyysi / log analysis (coop-audit)");
    for (i, (who, path)) in [("Isanta / host", host_path), ("Kaveri / friend", friend_path)].iter().enumerate() {
        let (n, a, b) = a.spans[i];
        let span = if n == 0 { "tyhja / empty".to_owned() } else { format!("{n} lines, {} .. {}", utc(a), utc(b)) };
        let _ = writeln!(o, "{who}: {path} ({span})");
    }
    let _ = writeln!(
        o,
        "Kellojen ero / clock offset (friend - host): {:+} s{}",
        a.offset,
        if a.offset_pairs > 0 { format!(" (estimated from {} flag pairs)", a.offset_pairs) } else { String::new() }
    );
    render_items(&mut o, &a.items);
    o
}

/// Summary, the items by severity and category, and the footer.
fn render_items(o: &mut String, items: &[Item]) {
    let mut per: BTreeMap<u8, usize> = BTreeMap::new();
    for it in items {
        *per.entry(it.sev).or_insert(0) += 1;
    }
    let summary: Vec<String> = per.iter().rev().map(|(s, n)| format!("{n} x {}", sev_label(*s))).collect();
    let _ = writeln!(
        o,
        "\nYhteenveto / Summary: {}",
        if summary.is_empty() { "ei loydoksia / no findings".to_owned() } else { summary.join(", ") }
    );
    let _ = writeln!(o, "Vakavin ensin; rivinumero = lokitiedoston rivi. / Most severe first; line = line number in that log.");
    let mut group: Option<(u8, Cat)> = None;
    let mut shown = 0;
    let mut hidden = 0;
    for it in items {
        if group != Some((it.sev, it.cat)) {
            if hidden > 0 {
                let _ = writeln!(o, "  ... +{hidden} lisaa / more");
            }
            group = Some((it.sev, it.cat));
            shown = 0;
            hidden = 0;
            let (fi, en) = it.cat.title();
            let _ = writeln!(o, "\n== [{}] {} / {} ({}) ==", it.sev, fi, en, sev_label(it.sev));
        }
        if shown >= 25 {
            hidden += 1;
            continue;
        }
        shown += 1;
        let times = if it.count > 1 { format!(" (x{})", it.count) } else { String::new() };
        let _ = writeln!(o, "  {} {} ({}:{}){times}: {}", it.who, utc(it.t), it.who, it.line, it.text);
    }
    if hidden > 0 {
        let _ = writeln!(o, "  ... +{hidden} lisaa / more");
    }
    let _ = writeln!(
        o,
        "\nEi nae / cannot see: what is not in the logs (rate-limited world.rs flag lines, a crash without a log line), and the friend's clock is only estimated."
    );
}

// --- 3-6 players -------------------------------------------------------------------------

/// One friend log of the session.
pub struct FriendLog<'a> {
    /// The player's name (`friend_label`).
    pub label: String,
    pub path: String,
    pub text: &'a str,
}

/// One host <-> friend pair of the session.
pub struct PairReport {
    pub label: String,
    pub path: String,
    pub analysis: Analysis,
}

pub struct SessionAnalysis {
    /// Host <-> each friend log of the session, the reference (first) log first.
    pub pairs: Vec<PairReport>,
    /// (path, why) of friend logs that are not this session's.
    pub skipped: Vec<(String, String)>,
    /// Every pair's findings (host lines once) and the client -> client grant checks, sorted.
    pub items: Vec<Item>,
    /// The host lines looked at (every pair's window).
    pub host_span: (usize, u64, u64),
}

/// `...\log-Friend2-20261024-213000.txt` -> `Friend2` (serve.ps1 names uploads like that).
pub fn friend_label(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = file.strip_suffix(".txt").unwrap_or(file);
    let stem = stem.strip_prefix("log-").unwrap_or(stem);
    let b = stem.as_bytes();
    // A trailing -yyyyMMdd-HHmmss upload stamp.
    let stamped = b.len() > 16
        && b[b.len() - 16] == b'-'
        && b[b.len() - 7] == b'-'
        && b[b.len() - 15..b.len() - 7].iter().all(u8::is_ascii_digit)
        && b[b.len() - 6..].iter().all(u8::is_ascii_digit);
    let name = if stamped { &stem[..stem.len() - 16] } else { stem };
    if name.is_empty() { "friend".to_owned() } else { name.to_owned() }
}

/// The host log against every friend log of one session. `friends[0]` is the reference (the
/// upload the report is for); another log joins when its time span overlaps the reference's
/// on the host clock and no log of the same player joined before it. `opt.offset` is the
/// reference's; the others' are estimated.
pub fn analyze_session(host: &str, friends: &[FriendLog], opt: &Options) -> SessionAnalysis {
    let host_all = parse(host);
    let parsed: Vec<Vec<Line>> = friends.iter().map(|f| parse(f.text)).collect();
    let offsets: Vec<i64> = parsed
        .iter()
        .enumerate()
        .map(|(i, l)| match (i, opt.offset) {
            (0, Some(o)) => o,
            _ => estimate_offset(&host_all, l).0,
        })
        .collect();
    // Host-clock span of each friend log.
    let host_span_of = |i: usize| parsed[i].first().zip(parsed[i].last()).map(|(a, b)| (a.t as i64 - offsets[i], b.t as i64 - offsets[i]));
    let mut kept: Vec<usize> = Vec::new();
    let mut skipped = Vec::new();
    for i in 0..friends.len() {
        if i > 0 {
            let why = if kept.iter().any(|&k| friends[k].label == friends[i].label) {
                Some("the same player's log is in already".to_owned())
            } else {
                match (host_span_of(0), host_span_of(i)) {
                    (Some((a, b)), Some((c, d))) if a <= d && c <= b => None,
                    (_, None) => Some("empty".to_owned()),
                    _ => Some(format!("no time overlap with {}", friends[0].path)),
                }
            };
            if let Some(why) = why {
                skipped.push((friends[i].path.clone(), why));
                continue;
            }
        }
        kept.push(i);
    }
    let multi = kept.len() > 1;
    let name = |i: usize| if multi { format!("friend '{}'", friends[i].label) } else { "friend".to_owned() };
    let mut pairs = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut seen: HashSet<(String, Cat, String, usize)> = HashSet::new();
    for &i in &kept {
        // (The same offsets as above: given for the reference, else estimated the same way.)
        let o = Options { all: opt.all, offset: if i == 0 { opt.offset } else { None } };
        let a = analyze_pair(&host_all, &parsed[i], &o, &name(i));
        // Host lines are in every pair: once each.
        for it in &a.items {
            if seen.insert((it.who.clone(), it.cat, it.text.clone(), it.line)) {
                items.push(it.clone());
            }
        }
        pairs.push(PairReport { label: friends[i].label.clone(), path: friends[i].path.clone(), analysis: a });
    }
    // Client -> client: every client's grants against every other client's log.
    let mut cross = Vec::new();
    for &i in &kept {
        for &j in kept.iter().filter(|&&j| j != i) {
            let mut c = Ctx { logs: [&parsed[i], &parsed[j]], offset: offsets[j] - offsets[i], items: Vec::new(), names: [name(i), name(j)] };
            rewards(&mut c, Side::Host);
            cross.append(&mut c.items);
        }
    }
    dedupe(&mut cross);
    items.append(&mut cross);
    items.sort_by(|a, b| b.sev.cmp(&a.sev).then(a.cat.cmp(&b.cat)).then(a.t.cmp(&b.t)).then(a.line.cmp(&b.line)));
    let window = kept.iter().filter_map(|&i| host_span_of(i)).fold(None, |w: Option<(i64, i64)>, (a, b)| {
        Some(w.map_or((a - 600, b + 600), |(lo, hi)| (lo.min(a - 600), hi.max(b + 600))))
    });
    let host_lines: Vec<Line> = match (opt.all, window) {
        (false, Some((lo, hi))) => host_all.iter().copied().filter(|l| (l.t as i64) >= lo && (l.t as i64) <= hi).collect(),
        _ => host_all,
    };
    SessionAnalysis { pairs, skipped, items, host_span: span(&host_lines) }
}

/// The session report: one friend log = the 2-player report (+ the skipped logs, if any).
pub fn render_session(s: &SessionAnalysis, host_path: &str) -> String {
    let skipped = |o: &mut String| {
        for (path, why) in &s.skipped {
            let _ = writeln!(o, "Ohitettu / skipped (ei tama sessio / not this session): {path} ({why})");
        }
    };
    if let [p] = s.pairs.as_slice() {
        let mut o = render(&p.analysis, host_path, &p.path);
        if !s.skipped.is_empty() {
            o.push('\n');
            skipped(&mut o);
        }
        return o;
    }
    let mut o = String::new();
    let _ = writeln!(o, "Sekiro co-op: lokien analyysi / log analysis (coop-audit), {} pelaajaa / players", s.pairs.len() + 1);
    let (n, a, b) = s.host_span;
    let span = |n: usize, a: u64, b: u64| if n == 0 { "tyhja / empty".to_owned() } else { format!("{n} lines, {} .. {}", utc(a), utc(b)) };
    let _ = writeln!(o, "Isanta / host: {host_path} ({})", span(n, a, b));
    for p in &s.pairs {
        let (n, a, b) = p.analysis.spans[1];
        let est = if p.analysis.offset_pairs > 0 { format!(", estimated from {} flag pairs", p.analysis.offset_pairs) } else { String::new() };
        let _ = writeln!(
            o,
            "Kaveri / friend '{}': {} ({}; clock offset (friend - host) {:+} s{est})",
            p.label,
            p.path,
            span(n, a, b),
            p.analysis.offset
        );
    }
    skipped(&mut o);
    let _ = writeln!(o, "Parit / pairs: host <-> each friend; client -> client grants checked in the other client's log.");
    render_items(&mut o, &s.items);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = include_str!("../tests/fixtures/host.txt");
    const FRIEND: &str = include_str!("../tests/fixtures/friend.txt");

    fn run() -> Analysis {
        analyze(HOST, FRIEND, &Options::default())
    }

    fn has(a: &Analysis, cat: Cat, side: Side, needle: &str) -> Option<Item> {
        a.items.iter().find(|i| i.cat == cat && i.side == side && i.text.contains(needle)).cloned()
    }

    #[test]
    fn parses_stamps_and_utc() {
        let l = parse("[1792751473] key items: x\nno stamp here\n\n[12] y");
        assert_eq!(l.len(), 3);
        assert_eq!((l[0].no, l[0].t, l[0].text), (1, 1792751473, "key items: x"));
        assert_eq!((l[1].no, l[1].t), (2, 1792751473));
        assert_eq!(l[2].no, 4);
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_792_800_000), "2026-10-24 00:00:00 UTC");
        assert_eq!(find_num("lot -1, x", "lot "), Some(-1));
        assert_eq!(find_num("tick 5 fps 19 mod", " fps "), Some(19));
    }

    #[test]
    fn the_clock_offset_comes_from_flag_pairs() {
        let a = run();
        assert_eq!(a.offset, 5);
        assert!(a.offset_pairs >= 2);
    }

    #[test]
    fn crashes_panics_versions_and_audit_lines() {
        let a = run();
        let crash = has(&a, Cat::Crash, Side::Host, "sekiro.exe+0xbc2c87").expect("crash");
        assert_eq!(crash.sev, 5);
        assert!(crash.text.contains("CRASH regs"));
        assert_eq!(has(&a, Cat::Panic, Side::Friend, "frame panicked").map(|i| i.sev), Some(5));
        assert_eq!(has(&a, Cat::Version, Side::Friend, "host has build").map(|i| i.sev), Some(3));
        let s = has(&a, Cat::AuditSuspect, Side::Host, "Shinobi Esoteric Text").expect("audit suspect");
        assert_eq!(s.sev, 4);
        assert_eq!(s.count, 2);
        assert_eq!(has(&a, Cat::KeyItems, Side::Host, "Shinobi Esoteric Text").map(|i| i.sev), Some(3));
        assert_eq!(has(&a, Cat::BossRestore, Side::Friend, "1120300").map(|i| i.sev), Some(3), "restored at 1 HP");
        // Sorted: the critical ones first.
        assert_eq!(a.items[0].sev, 5);
        assert!(a.items.windows(2).all(|w| w[0].sev >= w[1].sev));
    }

    #[test]
    fn rewards_sent_and_never_given_are_found() {
        let a = run();
        // 61000 was given, 13604000 arrived but was never given, 50201000 never arrived.
        assert!(has(&a, Cat::RewardLost, Side::Host, "lot 61000 ").is_none());
        assert_eq!(has(&a, Cat::RewardLost, Side::Host, "lot 13604000 (event 1750319414) reached").map(|i| i.sev), Some(4));
        assert!(has(&a, Cat::RewardLost, Side::Host, "lot 50201000 (event 1750319415) sent to the friend, never arrived").is_some());
        // The friend's script lot was given by lot (no event id in the line).
        assert!(has(&a, Cat::RewardLost, Side::Friend, "lot 2000").is_none());
        // Boss reward: received, nothing given.
        assert!(has(&a, Cat::RewardLost, Side::Host, "boss 1100800 reached the friend, but no lot").is_some());
        // Boss drop announced, handled there.
        assert!(has(&a, Cat::RewardLost, Side::Host, "drop of 1100300").is_none());
        // The friend's boss drop never arrived here.
        assert!(has(&a, Cat::RewardLost, Side::Friend, "boss drop of 1120300 (lot 12000000) announced").is_some());
    }

    #[test]
    fn ping_pong_counters_disconnects_and_fps() {
        let a = run();
        let pp = has(&a, Cat::PingPong, Side::Host, "flag 9100").expect("ping-pong");
        assert_eq!(pp.sev, 4);
        assert!(has(&a, Cat::PingPong, Side::Host, "flag 11105").is_none());
        assert_eq!(has(&a, Cat::Counter, Side::Host, "world: 'lost'").map(|i| i.sev), Some(3));
        assert!(has(&a, Cat::Counter, Side::Host, "largest 71").is_some());
        assert_eq!(has(&a, Cat::Counter, Side::Host, "'dropped old'").map(|i| i.sev), Some(1));
        assert!(has(&a, Cat::Counter, Side::Host, "'given up'").is_none(), "given up 0 is not a finding");
        assert!(has(&a, Cat::Disconnect, Side::Host, "HostPlayer left").is_some());
        assert!(has(&a, Cat::Fps, Side::Host, "fps below 20 for 12 s").is_some());
        // A loading screen (fps 3) is not a drop.
        assert_eq!(a.items.iter().filter(|i| i.cat == Cat::Fps).count(), 1);
    }

    #[test]
    fn host_lines_outside_the_friend_session_are_ignored() {
        let a = run();
        assert!(has(&a, Cat::Crash, Side::Host, "0x12486e").is_none(), "an older session's crash");
        let all = analyze(HOST, FRIEND, &Options { all: true, offset: None });
        assert!(has(&all, Cat::Crash, Side::Host, "0x12486e").is_some());
        let text = render(&a, "log.txt", "friend.txt");
        assert!(text.contains("KRIITTINEN / CRITICAL"));
        assert!(text.contains("clock offset (friend - host): +5 s"));
        assert!(text.contains("(host:"));
    }

    /// A second client: its clock 1 s ahead of the host's, one script lot sent to everyone.
    const FRIEND2: &str = "[1792751400] coop: task system ready, registering frame task\n\
        [1792751411] world: friend set flag 11105 = true (#1)\n\
        [1792751450] audit: SUSPECT [peer 0 'Host'] (sev 2) skills differ\n\
        [1792751550] reward: sent our script lot 777 to the friend (event 4242, ok = true): [\"goods 1 'X' x1\"], lot flags [1]\n\
        [1792751800] coop: tick 9000 fps 60 mod avg 0.10 ms max 1.00 ms\n";
    const OLD: &str = "[1792000000] coop: task system ready, registering frame task\n[1792000100] coop: frame panicked (1/3)\n";

    fn log<'a>(path: &str, text: &'a str) -> FriendLog<'a> {
        FriendLog { label: friend_label(path), path: path.to_owned(), text }
    }

    #[test]
    fn friend_labels_and_tagged_audit_lines() {
        assert_eq!(friend_label(r"C:\x\friend-uploads\log-Friend2-20261024-213000.txt"), "Friend2");
        assert_eq!(friend_label("log-A-B-20261024-213000.txt"), "A-B");
        assert_eq!(friend_label("friend.txt"), "friend");
        assert_eq!(friend_label("log-.txt"), "friend");
        assert_eq!(audit_suspect("audit: SUSPECT (sev 3) the friend has x"), Some((3, "the friend has x".to_owned())));
        assert_eq!(audit_suspect("audit: SUSPECT [peer 2 'Friend2'] (sev 1) y"), Some((1, "[peer 2 'Friend2'] y".to_owned())));
        assert_eq!(audit_suspect("audit: SUSPECT later"), None);
    }

    #[test]
    fn one_friend_log_is_the_two_player_report() {
        let s = analyze_session(HOST, &[log("friend.txt", FRIEND)], &Options::default());
        assert_eq!(render_session(&s, "log.txt"), render(&run(), "log.txt", "friend.txt"));
        // Logs of another session or a second upload of the same player are left out.
        let s = analyze_session(
            HOST,
            &[log("friend.txt", FRIEND), log("log-Old-20261001-120000.txt", OLD), log("log-friend-20261024-000000.txt", FRIEND2)],
            &Options::default(),
        );
        assert_eq!(s.pairs.len(), 1);
        assert_eq!(s.skipped.len(), 2);
        assert!(s.skipped[0].1.contains("no time overlap"));
        assert!(s.skipped[1].1.contains("same player"));
        let text = render_session(&s, "log.txt");
        assert!(text.starts_with(&render(&run(), "log.txt", "friend.txt")));
        assert!(text.contains("skipped (ei tama sessio / not this session): log-Old-20261001-120000.txt"));
    }

    #[test]
    fn a_session_covers_each_pair_and_client_to_client_grants() {
        let friends = [log("log-HostPlayer-20261024-010000.txt", FRIEND), log("log-Friend2-20261024-010500.txt", FRIEND2), log("log-Old-20261001-120000.txt", OLD)];
        let s = analyze_session(HOST, &friends, &Options::default());
        assert_eq!(s.pairs.iter().map(|p| (p.label.as_str(), p.analysis.offset)).collect::<Vec<_>>(), vec![("HostPlayer", 5), ("Friend2", 1)]);
        assert_eq!(s.skipped.len(), 1);
        let find = |who: &str, cat: Cat, needle: &str| s.items.iter().filter(|i| i.who == who && i.cat == cat && i.text.contains(needle)).count();
        // Host lines once, though both pairs read them.
        assert_eq!(find("host", Cat::Crash, "0xbc2c87"), 1);
        // The host's grants per friend, named.
        assert_eq!(find("host", Cat::RewardLost, "lot 50201000 (event 1750319415) sent to the friend 'HostPlayer', never arrived"), 1);
        assert_eq!(find("host", Cat::RewardLost, "lot 50201000 (event 1750319415) sent to the friend 'Friend2', never arrived"), 1);
        // Friend2's grant reached neither the host nor HostPlayer (relayed by the host).
        assert_eq!(find("friend 'Friend2'", Cat::RewardLost, "lot 777 (event 4242) sent to the host, never arrived"), 1);
        assert_eq!(find("friend 'Friend2'", Cat::RewardLost, "lot 777 (event 4242) sent to the friend 'HostPlayer', never arrived"), 1);
        // HostPlayer's script lot reached the host (given by lot there), not Friend2.
        assert_eq!(find("friend 'HostPlayer'", Cat::RewardLost, "lot 2000 (event 1794667962) sent to the friend 'Friend2', never arrived"), 1);
        assert_eq!(find("friend 'HostPlayer'", Cat::RewardLost, "lot 2000 (event 1794667962) sent to the host"), 0);
        // A tagged auditor line keeps its peer.
        assert_eq!(find("friend 'Friend2'", Cat::AuditSuspect, "[peer 0 'Host'] skills differ"), 1);
        assert!(s.items.windows(2).all(|w| w[0].sev >= w[1].sev));
        let text = render_session(&s, "log.txt");
        assert!(text.contains("3 pelaajaa / players"));
        assert!(text.contains("Kaveri / friend 'Friend2': log-Friend2-20261024-010500.txt"));
        assert!(text.contains("clock offset (friend - host) +1 s, estimated from 1 flag pairs"));
        assert!(text.contains("(friend 'Friend2':4)"));
    }
}

//! Phase 0: prove the toolchain. The DLL is loaded by me3 into the original
//! Sekiro v1.06, waits for the game's task system, registers a per-frame task
//! and writes one log line per second to
//! `%LOCALAPPDATA%\sekiro-coop\log.txt`.
//!
//! Acceptance (FEASIBILITY.md §8.3, Phase 0): `coop: tick N` lines keep
//! appearing while playing, and the game runs 10 minutes without crashing.

mod activation;
mod artfx;
mod audit;
mod backups;
mod bossgate;
mod bosses;
mod bossrules;
mod anim;
mod crash;
mod cutscene;
mod debuff;
mod dialog;
mod doors;
mod doctor;
mod dump;
mod enemies;
mod fps;
mod game;
mod ghost;
mod gpuguard;
mod hkbsync;
mod hook;
mod icons;
mod idols;
mod invguard;
mod items;
mod join;
mod keyitems;
mod leave;
mod livesync;
mod log;
mod mode4;
mod motion;
mod msfunc;
mod nativetext;
mod native;
mod net;
#[cfg(feature = "testsync")]
mod observe;
mod netai;
mod nopause;
mod npcintro;
mod nsession;
mod ownworld;
mod peers;
mod persist;
mod posture;
mod menu;
mod optmenu;
mod optmenu_model;
mod puppet;
mod puppetguard;
mod ngblock;
mod optprobe;
mod quitblock;
mod restsync;
mod resurrect;
mod resync;
mod rtti;
mod scaling;
mod settings;
mod shipfiles;
mod speffect;
mod statelog;
mod talklock;
mod spectate_cam;
mod spectate_input;
mod steam;
#[cfg(feature = "testsync")]
mod testsync;
mod tuning;
mod ui;
mod world;

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fromsoftware_shared::SharedTaskImpExt;
use sekiro::sprj::{SprjTaskGroupIndex, SprjTaskImp};

const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;

/// False once the frame task gave up (3 panics). Detours that change game
/// behaviour (bossrules reload / CSDeadStep, spectator camera, artfx) then act
/// like the original code.
static MOD_ALIVE: AtomicBool = AtomicBool::new(true);

pub(crate) fn mod_alive() -> bool {
    MOD_ALIVE.load(Ordering::Acquire)
}

/// # Safety
/// Called by the Windows loader. Do not call this yourself.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DllMain(hmodule: usize, reason: u32, reserved: usize) -> bool {
    // v8.52: the game process ends normally (Quit Game, Alt+F4, the window closed: ExitProcess,
    // `reserved` != 0). A crash writes `SESSION_END crash` instead (crash.rs); no line at all =
    // killed (Task Manager, TerminateProcess) or a crash that ran no code.
    if reason == DLL_PROCESS_DETACH && reserved != 0 {
        log::line_nowait("EVENT SESSION_END exit: the game process is exiting normally (ExitProcess)");
        return true;
    }
    if reason != DLL_PROCESS_ATTACH {
        // Never tear down on detach: game tasks may still reference our code.
        return true;
    }

    std::thread::spawn(move || {
        log::session_header();
        log::line("coop: dll attached, waiting for game task system");
        crash::install();
        // No hook or patch of any kind (icons included) on an unsupported exe.
        if let Err(why) = game::check_exe() {
            log::line(&format!("coop: UNSUPPORTED GAME VERSION ({why}): the mod does nothing this run (needs Steam Sekiro 1.06, unmodified)"));
            let _ = panic::catch_unwind(move || {
                // Only the overlay (a DX11 Present hook, no game memory is written) to say why.
                ui::publish(ui::Status {
                    net: "off".to_owned(),
                    notice: Some("Wrong Sekiro version: co-op needs Steam Sekiro 1.06 (unmodified sekiro.exe). The mod is off.".to_owned()),
                    ..Default::default()
                });
                ui::install(hmodule);
            });
            return;
        }
        // The menu textures load at boot, before the task system: hook their creation now.
        std::thread::spawn(|| {
            let _ = panic::catch_unwind(icons::install_early);
        });
        // fromsoftware-rs panics if the exe is not v1.6.0.0; log that instead
        // of dying silently.
        if let Err(e) = panic::catch_unwind(move || init(hmodule)) {
            let msg = e
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| e.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            log::line(&format!("coop: INIT FAILED: {msg}"));
        }
    });

    true
}

fn init(hmodule: usize) {
    // 1.0-beta gave up on the first NotFound (too early on some PCs); it is retried now.
    let waited = std::time::Instant::now();
    let task_imp = match SprjTaskImp::wait_for_instance(Duration::MAX) {
        Ok(t) => t,
        Err(e) => {
            log::line(&format!(
                "coop: SprjTaskImp not found: {e:?} after {} ms",
                waited.elapsed().as_millis()
            ));
            return;
        }
    };
    log::line(&format!(
        "coop: task system ready after {} ms, registering frame task",
        waited.elapsed().as_millis()
    ));
    // The settings file first: fps and observe read it when they install.
    settings::init();
    // Launcher scripts next to the DLL kept current (a friend's old update.ps1 only fetches the DLL).
    shipfiles::start();
    // Installed here, at the title screen, before any character physics runs.
    hook::install();
    hook::install_flag_hook();
    hook::install_hkb_hook();
    hook::install_move_hook();
    activation::install();
    bossrules::install();
    items::install();
    dialog::install();
    quitblock::install();
    optprobe::install();
    optmenu::install();
    ngblock::install();
    nopause::install();
    talklock::install();
    speffect::install_hook();
    // v8.60: the owner's type-5 HP does not kill a Puppeteer puppet here (puppetguard.rs).
    puppetguard::install();
    fps::install();
    // The material constant-buffer over-read behind the nvwgf2umx.dll crash (gpuguard.rs).
    gpuguard::install(settings::gpu_guard_at_start());
    #[cfg(feature = "testsync")]
    if settings::observe_at_start() {
        observe::install();
    }
    // Experimental native session: its pass-through hooks only with native_session = 1.
    if tuning::native_session() {
        nsession::install_at_start();
    }
    // overlay_hook = 0 ([DEBUG]): no hudhook at all, for A/B tests of the driver crash.
    if settings::overlay_hook_at_start() {
        ui::install(hmodule);
    } else {
        log::line("ui: overlay NOT installed (overlay_hook = 0): no status line, names or menu this run");
    }
    // Where every stub / fake page went (one shared near arena, hook.rs).
    hook::log_arena();

    let mut frames: u64 = 0;
    let mut last_frames: u64 = 0;
    let mut last_frames_ms: u64 = 0;
    let mut mod_total = Duration::ZERO;
    let mut mod_max = Duration::ZERO;
    let mut last_log = Instant::now();
    let mut last_mem = Instant::now();
    // Frame pacing (v8.6): stutter shows as single long frames that a 2 s fps average hides.
    let mut frame_at: Option<Instant> = None;
    let mut last_mod = Duration::ZERO;
    let mut hitch = Hitches::default();
    let mut puppet = puppet::Puppet::default();
    let mut panics: u32 = 0;
    if settings::stall_watchdog_at_start() {
        stall::start();
    }
    let handle = task_imp.run_recurring(
        move |_: &usize| {
            frames += 1;
            stall::beat();
            let now = Instant::now();
            if let Some(prev) = frame_at.replace(now) {
                hitch.add(now.duration_since(prev), last_mod);
            }
            let elapsed = last_log.elapsed();
            if elapsed >= Duration::from_secs(2) {
                last_log = Instant::now();
                let fps = (frames - last_frames) as f64 / elapsed.as_secs_f64();
                last_frames = frames;
                // Our own per-frame cost, to tell mod overhead from game-side cost.
                let n = (frames - last_frames_ms).max(1) as f64;
                // v8.61: name the slowest parts when a mod frame went over 5 ms.
                let spans = crash::prof_take();
                let parts = if mod_max > Duration::from_millis(5) { crash::prof_line(spans, 3) } else { String::new() };
                log::line(&format!(
                    "coop: tick {frames} fps {fps:.0} mod avg {:.2} ms max {:.2} ms{}{parts}",
                    mod_total.as_secs_f64() * 1000.0 / n,
                    mod_max.as_secs_f64() * 1000.0,
                    hitch.take()
                ));
                mod_total = Duration::ZERO;
                mod_max = Duration::ZERO;
                last_frames_ms = frames;
                if last_mem.elapsed() >= Duration::from_secs(30) {
                    last_mem = Instant::now();
                    if let Some(l) = memory_line() {
                        log::line(&l);
                    }
                }
            }
            let t0 = Instant::now();
            crash::prof_frame_start();
            // The game calls this task through fromsoftware-rs' `extern "C"`
            // execute(): a panic unwinding out of it aborts the whole game.
            // Catch it, drop all mod state (pins released, socket reopened)
            // and carry on; after 3 panics the mod stops for this run.
            if panics < 3 {
                // Co-op items first: an item used this frame restarts the session in
                // the puppet's frame right after.
                let r = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    items::on_frame();
                    optmenu::on_frame();
                    quitblock::tick();
                    ngblock::tick();
                    nopause::tick();
                    gpuguard::tick();
                    puppet.on_frame()
                }));
                if r.is_err() {
                    panics += 1;
                    hook::clear_all_targets();
                    log::line(&format!("coop: frame panicked ({panics}/3), mod state reset"));
                    let old = std::mem::take(&mut puppet);
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(move || drop(old)));
                    // Native session (no-op unless native_session = 1): every patch back.
                    let _ = panic::catch_unwind(|| nsession::on_world_reset("frame panic"));
                    if panics >= 3 {
                        MOD_ALIVE.store(false, Ordering::Release);
                        log::line("coop: too many panics, co-op disabled until restart");
                        let _ = panic::catch_unwind(artfx::shutdown);
                        let _ = panic::catch_unwind(nsession::shutdown);
                    }
                    // Dropping the puppet drops BossRules, but the statics its detours
                    // read (held death reload, phase B, free cam) would stay set:
                    // hand everything back to the game now (review finding 1).
                    let why = if panics >= 3 { "mod disabled" } else { "frame panic" };
                    let _ = panic::catch_unwind(move || bossrules::emergency_reset(why));
                }
            }
            crash::prof_frame_end();
            // A crash outside our code (game, driver) then reports this.
            crash::breadcrumb("between mod frames (game/driver code)");
            let dt = t0.elapsed();
            last_mod = dt;
            mod_total += dt;
            mod_max = mod_max.max(dt);
        },
        SprjTaskGroupIndex::FrameBegin,
    );
    // Dropping the handle would cancel the task; keep it for the process lifetime.
    std::mem::forget(handle);
}

/// Long frames since the last tick line: frame time (start to start) over 25 / 50 ms, the
/// worst, and our own share of the frame before it (mod ms of that frame).
#[derive(Default)]
struct Hitches {
    over25: u32,
    over50: u32,
    worst: Duration,
    worst_mod: Duration,
}

impl Hitches {
    fn add(&mut self, frame: Duration, mod_prev: Duration) {
        if frame > Duration::from_millis(25) {
            self.over25 += 1;
        }
        if frame > Duration::from_millis(50) {
            self.over50 += 1;
        }
        if frame > self.worst {
            self.worst = frame;
            self.worst_mod = mod_prev;
        }
    }

    /// "" when no frame went over 25 ms; resets.
    fn take(&mut self) -> String {
        let h = std::mem::take(self);
        if h.over25 == 0 {
            return String::new();
        }
        format!(
            "; long frames >25 ms {} >50 ms {}, worst {:.0} ms (mod {:.1} ms of it)",
            h.over25,
            h.over50,
            h.worst.as_secs_f64() * 1000.0,
            h.worst_mod.as_secs_f64() * 1000.0
        )
    }
}

/// Stall watchdog (diagnostic only, `[DEBUG] stall_watchdog`): the frame task bumps a counter;
/// a thread polling it every 100 ms logs ONCE per stall where the frame thread sits
/// (2026-10-27 logs: 0.5-5 s freezes with the mod at ~0 ms, so the cause was unnamed).
mod stall {
    use crate::{hook, log};
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
    use std::time::{Duration, Instant};

    static FRAMES: AtomicU64 = AtomicU64::new(0);
    /// Thread id of the thread that runs the frame task (0 = no frame yet).
    static FRAME_TID: AtomicU32 = AtomicU32::new(0);
    /// No frame for longer than this is a stall worth a line.
    const STALL_MS: u64 = 400;
    const POLL_MS: u64 = 100;

    /// Frame task, once per frame.
    pub fn beat() {
        if FRAME_TID.load(Relaxed) == 0 {
            FRAME_TID.store(hook::freeze::current_thread_id(), Relaxed);
        }
        FRAMES.fetch_add(1, Relaxed);
    }

    /// One line per stall: not again until a frame ran.
    pub fn should_log(already_logged: bool, stalled_ms: u64) -> bool {
        !already_logged && stalled_ms > STALL_MS
    }

    pub fn start() {
        let _ = std::thread::Builder::new().name("coop-stall-watchdog".into()).spawn(|| {
            let mut seen = FRAMES.load(Relaxed);
            let mut since = Instant::now();
            let mut logged = false;
            while crate::mod_alive() {
                std::thread::sleep(Duration::from_millis(POLL_MS));
                let now = FRAMES.load(Relaxed);
                if now != seen {
                    if logged {
                        log::line(&format!("stall: frames running again after {} ms", since.elapsed().as_millis()));
                    }
                    (seen, since, logged) = (now, Instant::now(), false);
                    continue;
                }
                let tid = FRAME_TID.load(Relaxed);
                let ms = since.elapsed().as_millis() as u64;
                if tid == 0 || !should_log(logged, ms) {
                    continue;
                }
                logged = true;
                // Stopped for as short a time as possible; nothing is allocated meanwhile.
                let rip = hook::freeze::thread_rip(tid);
                let state = match rip {
                    Some((rip, ctx_ms)) => format!("{}, context read in {ctx_ms} ms", hook::freeze::module_offset(rip)),
                    None => "RIP unreadable (thread gone or suspend failed)".to_owned(),
                };
                log::line(&format!("stall: no frame for {ms} ms; main thread tid {tid} RIP {state}"));
            }
        });
    }
}

#[cfg(test)]
mod stall_tests {
    use super::stall::should_log;

    #[test]
    fn logs_once_after_threshold() {
        assert!(!should_log(false, 400));
        assert!(should_log(false, 401));
        assert!(!should_log(true, 5000));
    }
}

#[cfg(test)]
mod hitch_tests {
    use super::Hitches;
    use std::time::Duration;

    #[test]
    fn long_frames_counted_worst_kept_and_reset() {
        let mut h = Hitches::default();
        h.add(Duration::from_millis(16), Duration::from_millis(1));
        assert_eq!(h.take(), "");
        h.add(Duration::from_millis(30), Duration::from_millis(1));
        h.add(Duration::from_millis(80), Duration::from_millis(2));
        h.add(Duration::from_millis(40), Duration::from_millis(9));
        assert_eq!(h.take(), "; long frames >25 ms 3 >50 ms 1, worst 80 ms (mod 2.0 ms of it)");
        assert_eq!(h.take(), "");
    }
}

/// `mem:` log line (v8.41): private bytes / working set / peak commit of the game process, so a slow-down or an
/// allocation failure after repeated reloads can be matched with growing memory use.
fn memory_line() -> Option<String> {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_faults: u32,
        peak_ws: usize,
        ws: usize,
        quota: [usize; 4],
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(process: *mut std::ffi::c_void, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters { cb: 0, page_faults: 0, peak_ws: 0, ws: 0, quota: [0; 4], pagefile: 0, peak_pagefile: 0, private: 0 };
    c.cb = std::mem::size_of::<Counters>() as u32;
    // SAFETY: plain kernel32 query into a correctly sized local struct.
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } == 0 {
        return None;
    }
    let mb = |v: usize| v / (1024 * 1024);
    Some(format!(
        "mem: private {} MB, working set {} MB, commit {} MB (peak {} MB), page faults {}",
        mb(c.private), mb(c.ws), mb(c.pagefile), mb(c.peak_pagefile), c.page_faults
    ))
}

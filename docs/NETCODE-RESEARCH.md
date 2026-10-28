# Netcode research: lighter, lower-latency, less desync (2026-10-22)

Scope: public sources only. No closed mod binaries were opened. Findings are in my own words.
"Verified" = I read the page this session. "Summary only" = I only saw a search-result summary.
"Blocked" = the site returned 403, so I could not read it. Claims from memory are marked `[memory]`.

## 0. What the code does today (read from the repo)

| Part | Current behaviour | Where |
|---|---|---|
| Player snapshot | 40-byte payload (pos, angle, anim, hp, block, hidden), sent when `now - last_send >= 33 ms`, checked once per frame | `net.rs::on_frame`, `protocol.rs` |
| Enemies | host sends up to 48 x 28 bytes every `>= 50 ms`, within 100 m of either player | `enemies.rs::host_frame` |
| hkb events | every event is re-sent in each packet for 300 ms, gate `>= 33 ms`; client applies on arrival | `hkbsync.rs` |
| Ghost | interpolated 100 ms behind **local receive time** (`Instant` taken in `poll`, once per frame) | `net.rs::remote_smoothed` |
| Enemy motion (client) | each frame `pos += (target - pos) * 0.20`, target = latest packet, no timestamp, no velocity | `enemies.rs::client_frame`, `tuning.rs` |
| Enemy AI | still runs on the client (docs comment: "so enemies can still hit the client") | `enemies.rs` header |
| Packets | each kind is its own datagram, 7-byte header each, all state absolute f32/i32 | `protocol.rs`, `session.rs` |

### Defects I can see from reading the code (these explain the tester reports)

1. **Send cadence is irregular.** The send gate is evaluated once per rendered frame. At 60 fps (16.7 ms frames) a 33 ms gate fires on every 2nd frame only if two frames sum to >= 33 ms; a single 16.0 ms frame pair (32 ms) misses and the send slips to frame 3 (48-50 ms). The 50 ms enemy gate is worse: three frames are about 50.0 ms, so it flips between 50 and 67 ms. Also `last_send = now` instead of `+= interval`, so the error accumulates. Real rates are roughly 20-30 Hz (player) and 15-20 Hz (enemies), with 33/50 ms and 50/67 ms alternation. A render buffer built on those arrival times shows "pulsing".
2. **Arrival time is quantized to the game frame and polluted by game hitches.** `poll()` runs once per frame on the game thread, so every packet that arrived in a 16.7 ms window gets the same timestamp. Jitter measured and interpolated against this is partly the game's own frame time, not the network's.
3. **Enemy smoothing is a frame-rate-dependent exponential chase of a staircase.** 20 %/frame at 60 fps is a time constant of about 75 ms; at 30 fps it doubles. A target that jumps every 50-67 ms and a follower with 75 ms lag produces a sawtooth speed (a runner at 6 m/s jumps 0.3-0.4 m per update). That is the "jerky/warp" look. It also trails by v x 75 ms in steady state.
4. **Two AIs fight over the same enemy.** The client's AI chases the client player, the host's AI chases the host player (or whoever it picked). The hook then pins the client enemy to the host's position. Result: the client enemy runs in place (AI says "walk to me", position says "stay") or slides (position moves, animation graph idles). This is a design cause, not a tuning problem.
5. **Positions are delayed 100 ms, animation events are not.** hkb events are raised on arrival (`hkbsync.rs`), while the position they belong to is rendered 100 ms later. The attack animation starts before the body gets there. Same for enemies.
6. **No empty-buffer fallback.** `remote_smoothed` returns the newest snapshot when the bracket is missing, i.e. it snaps. Any gap longer than the 100 ms buffer shows as a teleport.
7. **hkb redundancy has a hard 300 ms horizon.** After a 0.5 s stall (UPnP hiccup, a loading hitch) events are gone for good and `applied_n` just jumps forward, so the graph can stay in the wrong state with no recovery.

Bandwidth is NOT the problem. Estimated total: player 68 B x 30 = 2 KB/s, enemies about 28 B x 30 + 35 B overhead = about 900 B x 20 = 18 KB/s, hkb small. Under 25 KB/s (about 200 kbit/s). Compression is a nice-to-have; regularity and timeline are what matter.

## 1. Sources

Verified (read this session):
- Gaffer On Games, Snapshot Interpolation: https://gafferongames.com/post/snapshot_interpolation/
- Gaffer On Games, Snapshot Compression: https://gafferongames.com/post/snapshot_compression/
- Gaffer On Games, State Synchronization: https://gafferongames.com/post/state_synchronization/
- Gaffer On Games, Reliable Ordered Messages: https://gafferongames.com/post/reliable_ordered_messages/
- Gabriel Gambetta, Entity Interpolation: https://www.gabrielgambetta.com/entity-interpolation.html
- Fabien Sanglard, Quake 3 network model: https://fabiensanglard.net/quake3/network.php
- Edgegap, Overwatch netcode deep dive (summarises Tim Ford's GDC 2017 talk): https://edgegap.com/blog/game-backend-deep-dive-overwatch-2016-netcode-architecture-rollback
- GGPO readme (rollback): https://github.com/pond3r/ggpo/blob/master/doc/README.md
- netfox-net issue #39, visual error offset with decay: https://github.com/matelq/netfox-net/issues/39
- Valve GameNetworkingSockets header (Nagle, send rate, buffers): https://raw.githubusercontent.com/ValveSoftware/GameNetworkingSockets/master/include/steam/steamnetworkingtypes.h
- Steamworks networking overview: https://partner.steamgames.com/doc/features/multiplayer/networking
- DualShockers on Seamless Co-op: https://www.dualshockers.com/elden-ring-seamless-how-does-co-op-mod-work/
- Multiplayguide, snapshot buffer feel: https://multiplayguide.com/blog/multiplayer-snapshot-interpolation-feel-slider/
- Mirror docs, snapshot interpolation: https://mirror-networking.gitbook.io/docs/manual/components/network-transform/snapshot-interpolation

Summary only (search-result text, not the page):
- Valve Source Multiplayer Networking (official page was **blocked, 403**): https://developer.valvesoftware.com/wiki/Source_Multiplayer_Networking ; numbers below come from a secondary write-up: https://www.programmersought.com/article/13314576692/
- Unity Netcode for Entities interpolation: https://docs.unity3d.com/Packages/com.unity.netcode@1.9/manual/interpolation.html
- Mirror NetworkTime and timestamp batching: https://mirror-networking.gitbook.io/docs/manual/general/timestamp-batching
- Dead reckoning with convergence ("Believable Dead Reckoning for Networked Games", projective velocity blending): https://www.researchgate.net/publication/293809946_Believable_Dead_Reckoning_for_Networked_Games
- Photon Fusion shared-mode authority: https://doc.photonengine.com/fusion/2-shared/game-samples/pirate-adventure/tech-design
- Halo: Reach networking talk "I Shot You First" (slides/video not read, only summaries): https://edgegap.com/blog/game-backend-deep-dive-halo-reach-netcode-host-migration , https://ng.cs.ucl.ac.uk/2011/05/13/halo-reach/
- Seamless Co-op (Elden Ring) changelog and description (Nexus page itself **blocked, 403**; text seen via search): https://www.nexusmods.com/eldenring/mods/510?tab=logs
- Microsoft, timeBeginPeriod: https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod
- Rust forum on Windows UDP ConnectionReset: https://users.rust-lang.org/t/connectionreset-on-udpsocket-in-windows/125683

Not found / thin: no public technical write-up of Dark Souls 3 or Elden Ring netcode internals; no public ERSC architecture doc (the mod is closed). Forum claims about DS3 being "P2P, you see what you get" are anecdotal. I did not look at any other mod's binaries.

## 2. Techniques

### 2a. Interpolate on the sender's timeline, adapt the delay to measured jitter

- **What.** Each state carries the sender's clock. The receiver estimates the offset between clocks and renders at `remote_now - delay`. The delay is `k x send_interval + safety x jitter`, recomputed slowly.
- **Why.** Receive-time interpolation bakes network and game-frame jitter into motion. Sender time makes spacing between samples exact; only the *offset* must be tracked.
- **Numbers from sources.**
  - Gaffer: a buffer of 3 send intervals survives 2 consecutive lost packets at 2-5 % loss; that is 300 ms at 10 Hz, about 150 ms at 30 Hz, about 85 ms at 60 Hz for the same protection. Raising the send rate is the main lever on delay.
  - Valve Source (secondary): effective interp = max(cl_interp, ratio / updaterate), default 100 ms, ratio 2 recommended for clean links (about 30 ms at 66 Hz), ratio 3 for lossy ones.
  - Overwatch (via Edgegap/GDC): after moving to 60 Hz, interpolation delay dropped from about 60 ms to about 20 ms for many players.
  - Mirror: buffer time = send interval x multiplier, 3 usual; the buffer is allowed to catch up when it grows too large. Unity Netcode exposes the buffer in ms and recommends sizing it from measured jitter (that last point is from a secondary glossary).
  - Overwatch time dilation: the client's tick shrinks from 16 ms to about 15.2 ms (5 %) to build a buffer; that is the same trick used to move an interpolation delay without a visible jump.
- **Recipe `[mine, standard NTP/RTP-style math, not copied from one source]`.**
  - Each packet: `ts` (u32 milliseconds since the sender's session start).
  - On arrival: `d = recv_ms - ts`. `offset = sliding-window minimum of d over the last 5-10 s` (the minimum approximates true offset + minimum one-way delay), slewed by at most about 0.1 ms per packet so it never jumps; this tolerates 20-50 ppm clock drift.
  - Jitter: `J = J + (|d - d_prev| - J) / 16` (RFC 3550 style `[memory]`).
  - `delay = clamp(2 x interval + 3 x J, 70, 250 ms)`; grow fast (immediately), shrink slowly (about 1 ms per 100 ms) or by 3-5 % playback speed, never by jumping.
  - Render clock: `render_remote = (now - offset) - delay`; find the two samples around it.
- **Receive thread for better arrival times.** Block on `recv_from` in a small thread, stamp with `Instant::now()` at actual arrival, push into a preallocated queue the game thread drains. This removes the 16.7 ms frame quantization (defect 2). A blocking receive is woken by the network stack, so it does not depend on timer resolution (see 2g).
- **This mod.**
  - `protocol.rs`: add `ts: u32` to `Snapshot` and to the enemy/hkb bundle header (VERSION 5).
  - `net.rs`: replace `history: VecDeque<(Instant, Snapshot)>` + `remote_smoothed` with a small `Timeline { offset, jitter, delay }` struct and a sample ring keyed by `ts`.
  - Effort: M (about half a day). Cost: negligible CPU (a few float ops per packet). Bandwidth: +4 bytes per bundle.
- **Test hook.** Add fake jitter and loss to the loopback mode (random +-30 ms, 5 % drop) so adaptive delay can be tuned without testers; Gaffer and Multiplayguide both stress tuning under loss and jitter, not on clean links.

### 2b. Extrapolation / dead reckoning and soft correction

- **What.** When the buffer runs dry, continue along the last velocity for a short, capped time instead of snapping to the newest sample. When real data arrives, do not pop; fold the difference into a decaying visual offset.
- **Numbers.**
  - Gaffer: simple extrapolation is fine for about 50-250 ms of mostly linear motion and bad around collisions. Valve: linear extrapolation only up to 0.25 s of continuous loss (secondary). Unity Netcode: default cap 20 ticks (about 330 ms at 60 Hz).
  - Gaffer (State Sync) and the netfox issue: error offset decays by about 0.95 per tick for errors <= 25 cm and 0.85 per tick for errors >= 1 m, blended in between, recomputed on each correction so nothing visibly moves at the instant of the update.
  - Hermite (position + velocity per sample) removes the pulsing seen with plain linear interpolation at low send rates, at the cost of a little bandwidth.
- **This mod.**
  - Velocity: derive on the receiver from the two newest samples (free), or send `i8 x 3` at 0.1 m/s steps (+3 B). Derived is enough for walking; sent is better for dodge/jump starts.
  - Ghost: extrapolate at most about 120 ms with velocity damped by 0.9 per 33 ms; beyond that, hold position (a player standing still looks better than a player flying off).
  - Enemies: replace the `pos += (target - pos) * 0.20` line in `client_frame` with "render position = interpolated buffer position + decaying error offset"; make the decay frame-rate independent: `factor_per_frame = base ^ (dt x 60)`.
  - Keep the `SNAP_DISTANCE_M = 8` teleport.
  - Effort: S once 2a exists. CPU: trivial.

### 2c. Send rates, bundling, quantization

- **Send rate.** Valve's 100 ms default and Halo: Reach's roughly 10 Hz minimum are for large scenes; for two players on a character-scale scene, 60 Hz for the player is cheap: 68 B x 60 = 4 KB/s. Gaffer measured that raising rate shortens the needed buffer proportionally (150 ms at 30 Hz, 85 ms at 60 Hz). Practical: send the player **every rendered frame** (cadence then follows the game's own timing, which fixes defect 1 for free) and enemies every 2nd frame (30 Hz). Use a fixed-step accumulator `next_send += interval` if you need a non-frame rate.
- **Bundling.** One datagram per tick with sections: `[hdr: magic/ver/kind, ts, seq, ack, ack_bits] [player] [enemies] [hkb]`. Wins: one `sendto` instead of 3-4; one consistent timestamp across player, enemies and events; fewer independent loss events. Keep every datagram <= about 1200 bytes so IP fragmentation never turns one lost fragment into a lost packet (Quake 3 chunks at 1400; 1200 is the safe internet value `[memory]`). The 7-byte per-packet header can shrink to 4 (2-byte magic, 1 version, 1 kind) but this is cosmetic.
- **Quantization (Gaffer).** Position at 512 steps/m (about 2 mm) in bounded ranges; smallest-three quaternion 29 bits instead of 128. For this mod:
  - Enemy positions relative to the host player: `i16 x 3` at 1/128 m (+-256 m range covers the 100 m radius; 8 mm precision).
  - Angle `u8` for enemies (1.4 degrees), `u16` for the player.
  - hp `u16`, id `u16`, anim `u32`/varint.
  - Per enemy: 2 + 6 + 1 + 2 + 4 = 15 B (was 28), about 46 % smaller.
  - Add per-enemy priority accumulator (Gaffer State Sync): near/aggroed/moving enemies every tick, idle/far ones at 2-5 Hz. Typical scene drops from about 20 KB/s to about 6 KB/s.
  - No delta coding needed: absolute quantized snapshots are self-contained and loss-proof, and Gaffer's baseline/ack machinery only pays off with hundreds of objects.
- **Effort.** Bundle + timestamp: M (touches `protocol.rs`, `session.rs`, three senders). Quantization: S-M (encode/decode only, tests already round-trip every packet). Not urgent.

### 2d. Enemies whose AI runs on both machines

What public sources say:
- Halo: Reach (summaries): the host is authoritative; object state flows host to client as "eventual latest state"; events are unreliable notifications in both directions. Same shape as this mod's design.
- Photon Fusion shared mode: each object has one state authority; authority can be released/requested and transferred, and the sample game that has enemies and a boss uses a master client for NPCs. Nothing public gives a ready "nearest player owns the AI" recipe; the usual approach is to build it with hysteresis so ownership does not flip-flop `[mine]`.
- Seamless Co-op for Elden Ring (closed): public text only says a rest at a Site of Grace resets the world "to prevent enemy desync", enemy HP scales with player count, and one changelog line says chariots target the host exclusively "to prevent conflicting AI goals". That last point is the useful one: the author hit the same dual-AI conflict and resolved it by giving one machine the decision. I could not find a public description of ERSC's enemy replication; a forum user claims the base game mirrors the host's enemies on clients (unverified), so ERSC may simply ride the native replication. Sekiro has no such native layer, which is why this mod must build it.

Options for this mod, in order of cost:
1. **Freeze the non-owner AI** (best first step). On the client, put synced enemies into the same harmless/non-thinking state `make_puppet` already uses for the ghost (`puppet.rs`) so the position pin and animation come only from the host. Fixes run-in-place and sliding because animation and position now come from one source. Requires the client-side hit path to stay local (it already is: HP loss is detected locally and reported). Risk: enemies cannot damage the client; add a host-to-client reliable "you were hit" event derived from the host's collision with the client's *ghost*, or keep the client's own hit detection only for enemies in a short "engaged" window.
2. **Single target / aggro sync.** Host sends the AI target as `0 = host, 1 = client, 255 = none` per enemy (1 byte) in the enemy section; on the client, aim the (still running) AI at the ghost-of-host or the local player to match. Needs RE of the target field; cheaper to run than full authority but still two brains.
3. **Per-enemy authority with handoff to the nearer player.** Owner = player with the smaller distance, switch only if the other is closer by > 10 m for > 2 s and the enemy is not mid-attack (hysteresis; the cooldown idea comes from the Photon notes above). Owner runs AI and broadcasts state; non-owner freezes AI and interpolates. Handoff message is a reliable `Event{kind=OWNER, id, owner}`. This cuts the host's one-way latency penalty for the client's fights (the client fighting a far-from-host enemy would feel host RTT on every enemy reaction). Largest change; do after 1.
4. Always keep the cumulative-damage packets; they are already loss-proof.

Lag-compensation note (Valve, Overwatch): Sekiro hits are decided on the machine of the attacker (client hits locally, reports a cumulative total). That is "favor the shooter" for free and needs no rewind buffer; keep it.

### 2e. Reliability for hkb events

- Today: 300 ms of redundancy, applied on arrival by counter `n`.
- Gaffer (Reliable Ordered Messages): put `seq`, `ack` and a 32-bit `ack_bits` in every packet header (+10 bytes). A message stays in the send queue until a packet that carried it is acked; re-include only if not sent for roughly one RTT (Gaffer used 0.1 s). That gives reliable delivery with about 1.5 sends per event instead of about 9 now, and no 300 ms cliff.
- Cheaper variant that fits the current code: every bundle carries `hkb_ack = highest contiguous n applied` (4 bytes). Sender keeps events until `n <= hkb_ack`, repeats unacked ones at most once per bundle, cap the queue at 128 and 3 s. After overflow send a one-shot resync (`anim` id per entity, already in `Snapshot`/`EnemyState`) so a missed event cannot stick forever. Log a counter when `n` skips (gap = lost event).
- **Schedule events on the render timeline.** Add `ts` to each event (or use the bundle's) and hold incoming events in a queue until `render_remote >= ts`, so animation and position stay aligned (defect 5).
- Effort: S for ack + gap detection (`hkbsync.rs`, `session.rs`), S more for scheduling. CPU: a small VecDeque.

### 2f. Keeping the injected DLL cheap

Measured budget is already small (0.15-0.3 ms); these keep it from growing as enemy counts rise:
- **No per-frame allocations.** Today's hot path allocates: `Session::poll` returns a new `Vec<Event>`; `encode` allocates a `Vec`; `send_enemies` does `chunk.to_vec()`; `host_frame` builds `Vec<EnemyState>`; `client_frame` builds `stale` and `ids` vectors and iterates `HashMap`s (SipHash). Replace with: an `encode_into(&mut [u8; 1400]) -> usize` writer, `Vec::clear()` reuse (capacity is kept), `poll(&mut self, out: &mut Vec<Event>)`, and a fixed array indexed by hook slot (slots are already bounded by `hook::TARGETS`) instead of `HashMap<i32, _>`, or at least `FxHashMap`-style hashing.
- **Batch memory reads.** For each synced character read the base chain once (module pointer, physics pointer) into a small struct `{pos, angle, hp}` and cache the `Chr` for `entity id` for ~1 s instead of calling `chr_by_entity_id` every frame per enemy (the dominant native cost on the client). Revalidate by re-reading the id stored in the Chr or by the existing `rtti::class_name` check; drop the cache on area load (`reset`).
- **Do work only on ticks.** Host reads positions only at send ticks (already so); client interpolation math is per frame but allocation-free.
- Call `Instant::now()` once per frame and pass it down (QPC is cheap, about tens of ns, but it is called in many places now).
- Never log in these paths (the repo already learned this on 2026-10-22).
- Receive on a separate thread (2a): socket drain and `decode` (with its `Vec` allocations) leave the game thread entirely.
- Effort: M overall, mostly mechanical; measure with the existing frame-cost counter before and after, do last.

### 2g. Windows UDP notes

- **Nagle:** irrelevant for raw UDP. (Steam Networking Sockets does apply a 5 ms Nagle to small messages by default and offers `NoNagle`, per the header; only matters if this mod ever moves to that library. Default send rate there is 256 KB/s and send buffer 512 KB.)
- **Non-blocking:** already set. Keep draining until `WouldBlock`.
- **`SIO_UDP_CONNRESET`:** Windows turns an ICMP "port unreachable" into a `ConnectionReset` error on the next `recv_from`. `session.rs` handles it with `continue`, which works. Cleaner is to disable it once with `WSAIoctl(SIO_UDP_CONNRESET, false)` right after `bind` (needs the `windows-sys` call in the DLL; std does not expose it). Secondary sources: Rust forum thread above.
- **Buffers:** Microsoft's default receive buffer is transport-specific (about 64 KB `[memory]`; read it back with `getsockopt` and log it). At 20-25 KB/s that fills in about 3 s, which is exactly a game loading hitch; after the stall the burst of old snapshots is harmless (sequence check) but could also overflow. Set `SO_RCVBUF` to 256 KB. Keep `SO_SNDBUF` default. Do not shrink buffers "for latency": a UDP receive buffer adds delay only when the reader is slow, and the reader polls every frame.
- **Timer resolution:** `timeBeginPeriod` is per process since Windows 10 2004 and is ignored for occluded/minimized windows on Windows 11 (Microsoft docs). A blocking `recv_from` thread is woken by the network stack, so no timer tweak is needed; do not add `Sleep(1)` loops. Do not call `timeBeginPeriod` from the DLL (power cost and Sekiro may already set it).
- **QoS / DSCP:** setting `IP_TOS` does not make Windows mark packets by default; real marking needs `QOSAddSocketToFlow` or a group-policy rule `[memory]`, and home routers/ISPs usually ignore it. Skip.
- **Path:** the config mentions Tailscale. If `tailscale ping <host>` reports "via DERP", traffic is relayed and adds tens of ms; check this before blaming the code `[mine/memory]`. Direct UPnP/UDP is what the mod uses by default.
- **MTU:** keep datagrams <= 1200 bytes.

### 2h. Things that do NOT apply

- **Rollback / GGPO.** Needs a deterministic simulation and the ability to save/load full game state and re-simulate frames. Sekiro's AI, physics (Havok) and animation are neither accessible nor deterministic across machines. Input delay + rollback cannot be bolted onto an injected DLL. What transfers: render remote things slightly in the past; predict only what you own.
- **Delta compression against acked baselines** (Quake 3 style): worthwhile only with hundreds of entities. With at most 48 small records, absolute quantized state is simpler and loss-immune.
- **Client-side prediction of the local player:** the local player already runs natively on its own machine; nothing to predict.
- **Lockstep:** no.

## 3. Ranked changes (lag/desync reduction divided by effort)

See the final report in the hand-off message; summary:

1. Regular send cadence: per-frame player send + fixed-step accumulator for enemies/hkb, sender `ts` in every packet, one bundled datagram per tick. (S-M, biggest cheap win.)
2. Enemy snapshot interpolation from a timestamped per-enemy buffer, replacing the 20 %/frame chase. (M)
3. Shared render timeline: queue hkb events by `ts` and apply them when the interpolated positions reach that time. (S)
4. Adaptive delay (offset min-filter + jitter EWMA) and a receive thread for true arrival times; loopback fake-jitter test mode. (S-M)
5. Velocity extrapolation capped at about 120 ms plus decaying visual error offset instead of snapping. (S)
6. Remove the dual-AI conflict: freeze the client's AI for host-owned enemies (reuse the puppet "harmless" state), then per-enemy ownership with hysteresis later. (M, depends on RE)

Next tier: ack-based hkb delivery with resync; 15-byte quantized enemies with priority accumulator; no-alloc hot paths and cached `Chr` lookups; `SO_RCVBUF` 256 KB and `SIO_UDP_CONNRESET`.

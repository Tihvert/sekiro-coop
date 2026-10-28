# Sekiro co-op: siirtomuistio (2026-10-24, v7.15)

> **Tila-osio vanhentunut.** Ajantasainen tila ja TODO: `docs\JATKA-TASTA-MULTIPLAYER.md` (v8.4, 2026-10-25). Polut, tallennustyökalut ja säännöt pätevät yhä.

Omistaja (Tihvert/Tihvert) lukee suomea, vastaa lyhyesti. Kaveri: HostPlayer. Pelaavat UDP:llä (ei Steam).

## Missä mikäkin
- Repo `C:\Users\<user>\Documents\Default Project\projects\sekiro-seamless`, haara `coop-v7.1`, uusin `dd7ca55` (v7.15).
- Moninpeli (3-6 pelaajaa): worktree `...\projects\sekiro-seamless-multi`, haara `coop-multi`, uusin `2d11585` (v7.7 yhdistetty). Suunnitelma `docs\PLAN-MULTIPLAYER.md` §5.x.
- Suunnitteluloki: `docs\IMPLEMENTATION-NOTES.md` §1-§35 (+ v7.8 statelog/ini-kytkimet).
- Build `cmd //c ".\\build-dist.cmd"`, testit `cmd //c ".\\test.cmd"` + lib-testit erillisellä CARGO_TARGET_DIR:llä (`cargo test -p sekiro_coop --lib` vcvars-shellissä).
- Julkaisu `release.cmd` -> `dist\sekiro-coop\` (host pelaa tästä; pelin ollessa auki DLL menee `update\`-kansioon) + `dist\sekiro-coop.zip`. Commitoi julkaisut (omistaja hyväksynyt), ei pusheja.
- Testiservu `test-server.cmd` (serve.ps1, TCP 28101): kaverin play.cmd hakee DLL:n ja lähettää lokin -> `%LOCALAPPDATA%\sekiro-coop\friend-uploads\` + automaattinen raportti `report-*.txt` (coop-audit).
- Lokit: host `%LOCALAPPDATA%\sekiro-coop\log.txt`, kaveri `friend-log.txt` (kaverin kello ~5-6 s edellä). Valvojan löydökset `audit.txt` ja rivit `audit: SUSPECT`. Tilaloggeri rivit `state:`; F11 = `state: F11 FULL`.
- Tallennukset: host `%APPDATA%\Sekiro\<SteamID64>\S0000.coop.sl2`, hahmo = USER_DATA003 (4. rivi). Varmuuskopiot `%LOCALAPPDATA%\sekiro-coop\save-backups\`. Tallennustyökalut (py -I): `F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects\93896903-2e43-4f26-94f1-2099d5e65003\scratchpad\savesync\tools\` (sl2lib.py, setflags.py, check_live.py, sync.py; liput slot+0x34, 1660 x 128 B).
- Hostin IP vaihtuu: kaveri päivittää `sekiro_coop_settings.ini` `udp_host_address = <ip>:28100` ja `test_server.txt` `<ip>:28101`.

## Säännöt (omistaja)
Ei Anthropic API -avainta, ei maksullisia palveluja. Ei Steamin/pelin käynnistystä Claudesta. Ei mod-sisäisiä esineiden antomekanismeja (huijaus). Hostin kuolema ei nollaa; vain lepo tai wipe. Natiivisessio oletuksena. Ilmoitukset ilman OK-nappeja. Mallijako: Sonnet kirjoittaa koodin, Opus tarkistaa/suunnittelee, Haiku triviaalit.

## Hostin testiasetukset nyt
`dist\sekiro-coop\sekiro_coop_settings.ini`: `block_quit_in_coop = 1`, `native_notices = 0` (liittymisjumin korjaus, toimi), `ns_force_distribution = 1`, `audit = 0`. Muut v7.8 [DEBUG]-kytkimet: `scale_all_loaded 0`, `ns_throw_claims 0`, `ns_throw_collide_drop 0`, `posture_rest_guard 1`, `reward_reconcile 1`, `state_log 1`.

## v7.14-v7.15 (uusin dd7ca55), ei testattu pelissä
- GitHub: yksityinen https://github.com/Tihvert/sekiro-seamless-coop (omistaja hyväksyi 2026-10-24; pushataan sinne, ei julkiseksi ilman lupaa). MIT, README englanniksi, `docs\NEXUS-DRAFT.md`. Ennen julkaisua: vendor/sekiro NON-AI-lisenssitiedostot mukaan + kysy ylläpitäjältä, Steam ID pois tästä tiedostosta, testityökalut pois zipistä.
- v7.14 (1b001ef): deathblow-lukko (nsession dblock, gate 0xBE3810, §39), HP-palkki ini-asetuksin (`party_hp_position/content/scale`), statelog lukee 1000 valikkotilaa + pelaajan heittotilan. Uusi syöttöjumi (hyökkäys + Start estetty, blokkaus toimii) klo 12:07: EI valikkomaski, EI tila 102 (= dialogin vastausrekisteri). Lepo vapautti todennäköisesti. Seuraava F11 näyttää `throw (node, running)`.
- v7.15 (dd7ca55): spectate-jumi (pidätetty latauspyyntö -> kuolemalataus; vartijat 15/45 s; lepo/wipe/linkki/5 min vapauttaa; wipe 5 s tai EV_DEATH; §40), spectate-kamera orbit + lukitus (keskinappi/R3, §41), bossin nousu (pudotus ennen dead-testiä, 6 s HP-pito, pallot eivät nouse, yhden pallon bossit; persist palauttaa vain oman kuolemalatauksen jälkeen; wipe vain kommitoiduista kuolemista; §42). Videot (Genichiro/Tomoe) kuvattu v7.13:lla.
- Moninpelihaara coop-multi `3c5ade0`: v7.8-v7.13 yhdistetty (protokolla 11) + valvoja kaikille. v7.14-v7.15 EI vielä yhdistetty.

## v7.11-v7.13 (uusin 6b63da2), ei testattu pelissä
- v7.11 (a347419): doors.rs walker hyväksyy tilat 1/2/4 (ennen aina 0 ovea), toggle-suojat, kaverin OFF ovilipulle ohitetaan (§37). Loki `doors: scan: ...` kertoo luokat; jos one-state dominoi ja ovia 0 -> OneState-ObjActit EMEVD-lippuihin. Heittopaketti (type 18) vapauttaa native-pinnin 4 s (`throw: type 18 ...`, `native: remote in a throw`), pomput liukuvat. Automaattipäivitys kaikille tiedostoille (serve.ps1 /manifest; kaveri tarvitsee uuden zipin KERRAN; test-server.cmd uudelleen).
- v7.12 (1b73906): bosses.rs vaihetaulukko 15 bossia (`docs\SPEC-BOSS-PHASES.md`). LIVE-tarkistukset: Guardian Ape valekuolema, Sword Saint palkin väli, Monkeys, Headless Ape.
- v7.13 (6b63da2): ngblock.rs NG+-esto (YES-lambda 0x1411DADA0, commit 0x140A8D130 -> No-polku 0x140A8D030), liput 20-30 eivät synkkaa (HUOM: koko-record-snapshot voi yhä kantaa ne), cross-cycle-esto doctor.rs (`allow_cross_cycle`). §38.
- Taustatutkimus `docs\GAP-ANALYSIS-2026-10-24.md` (top 10). Seuraavat: tarinavaiheet kokonaisuuksina, Dragonrot-NPC-liput, välivideotila, NPC-puhelukko, Bell Demon doctoriin, exe-versiotarkistus ennen patcheja, liittymisilmoitus pysyvästi overlayhin, idolisynkka (RE event 4086).
- Korjattu: dist\update.ps1 oli kopio upload-log.ps1:stä; upload-log.ps1 sisälsi pelkän IP:n.

## v7.10 (9023d8a), ei testattu pelissä
- Liittymisjumi VAHVISTETTU poistuneeksi native_notices = 0:lla (1 testi). TODO: liittymisilmoitus aina overlayhin koodissa.
- Party HP -palkit (ui.rs draw_party, `[DISPLAY] party_hp_bars`), protokolla VERSION 10 (Snapshot.max_hp; +0x134 pelaajalle [ARV], loki `party: our max HP reads`).
- Asento: livesync own_getup_due (oma deathblow vei pallon mutta kopio ei noussut -> refill). Minibossit = bossitaistelu (bossrules mini_scan, spectate, vapautus tapossa).
- Hostin Severed Thread -> lataus idolille (leave.rs host_closing). Leave+rejoin = uusi join (net.rs session_gen).
- hook.rs freeze: kaikki säikeet ensin suspend, sitten GetThreadContext; >100 ms -> `hook: freeze slow`. Atomiset qword-patchit eivät käy (16/31 kohtaa ylittää 8 B rajan).
- Ovi (Estate Path, Hirata m10_00, idoli 1001951): EI 11125xxx (ne ovat Ashina Reservoir, lepo nollaa normaalisti; palautus oli turha). Ehdokas 21005024 (OFF hostilla), 11000670 ON. doors.rs ei ole koskaan löytänyt yhtään ovea ("0 doors loaded" 32/32) -> WorldObjActMan-layout väärin, korjattava IDA:lla + host-puolen suoja (kaverin OFF ovilipulle ei sovelleta). Tallennuskorjaus pelin ollessa kiinni: setflags.py ... USER_DATA003 21005024 11000670.
- Idolisynkka: liput kulkevat; peli lukee "sytyttämätön" kartan latauksessa. Tarvitsee RE:n (event 4086 / 0x665B50).

## v7.9 (4a447a1) ja tämän päivän löydökset
- Liittymisjumi: maski jäi `0x800008`:aan SAMASSA ruudussa kun "A companion has entered your world." näytettiin pelin tilaviestinä (dialog.rs STATUS_MESSAGE 0xE13C30). Hostin iniin `native_notices = 0` (overlay) testiksi. Jos jumi poistuu -> liittymisilmoitus aina overlayhin. Kuolema vapauttaa maskin.
- Uudelleenyhdistys <120 s ei enää warppaa kaveria (join.rs RECONNECT_RESUME). Syy: hostin kuolemalataus 13,7 s jäädytti modin -> kaverin linkki katkesi.
- Quit Game harmaa + teksti yhteyden aikana (quitblock.rs, §36, ei testattu pelissä). Hostin ini `block_quit_in_coop = 1`.
- `ns_force_distribution` oletus 1 (deflect-ajoitus huono ilman). Kaverin ini: `[DEBUG] ns_force_distribution = 1`.
- play.cmd tarkistaa Steamin (dist, kaveri saa vain zipistä).
- Ovi (DLC-alue, idoli 1121951): kaikki 45 lippua 11125xxx katosivat hostin tallennuksesta 08:04-08:14 (välissä wipe + peilattu lepo 1121951 klo ~08:06). Palautettu 08:04-varmuuskopiosta (ennen: `save-backups\S0000.coop.20261024-before-door-restore.sl2`). Juurisyy auki: mikä nollasi koko kartan lippulohkon? Lue kaverin loki samalta ajalta. Lisäksi oven kääntöbugi (doors.rs, ei näyttöä toiminnasta).

## AVOINNA (prioriteettijärjestys)
1. Liittymisjumi: liittymisen jälkeen host voi vain kävellä (ei Start, ei hyökkäys), joskus kesken pelin; kuolema vapauttaa. Alkoi v7.7:n JA tallennusmuokkausten kanssa (6056/6057, 724/725/728, 11115615/55, 71110157, 6723/6760, 50006160, taidot; 6207/6290 avasi combat art -paikan). Ei vielä eroteltu. Seuraava jumi: lue `state:`-rivit ja F11-dumppi. Jos v7.8:lla (v7.6-oletukset) yhä jumi -> epäile tallennusmuokkausta, poista liput yksi kerrallaan setflags.py:n kaltaisella työkalulla. Varmuuskopio ennen muokkauksia: `save-backups\S0000.coop.20261024-062221-session-start.sl2`. v7.6 DLL: `F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects\62924d84-0e3d-4955-afee-e79d52d45db3\scratchpad\sekiro_coop_v7.6_45d0c92.dll`.
2. FPS-nykiminen v7.7:ssä (mod-tick ~1 ms, eli pelin puolen työ). Epäilty pakotettu 22/23-jako -> v7.8:ssa pois (`ns_force_distribution 0`). Tarkista v7.8-lokista.
3. Asento ei nollaudu deathblow'n jälkeen (kaveri). Epäillyt `ns_throw_claims`/`ns_throw_collide_drop` pois v7.8:ssa; jos auttaa, tee deathblow-kiistakorjaus uudelleen ilman niitä (alkuperäinen bugi: hostin deathblow ei toiminut kaverin omistamaan minibossiin, ja samanaikaiset deathblowt kumosivat toisensa, §34).
4. Severed Thread (leave world): Yes/No-kysymyksen jälkeen ei tule efektiä eikä latausruutua, maailma ei vaihdu. Tutki leave.rs / items.rs.
5. Maailman lippusynkka hukkaa lippuja (hostilta puuttui 724/725/728/11115615/11115655/71110157, vaikka loki näytti "friend set flag 724"). world.rs stats "lost 2745".
6. ownworld-palautus jättää lippuja palauttamatta (kaverilta puuttui ~40 tarina-/NPC-lippua + Resurrection). Kaveri sai korjatun tallennuksen (KAVERI-S0000.coop.sl2) - varmista asensiko.
7. Liput 21114178/21114179/21115312/21115313/11125671 pomppivat pelaajien välillä joka liittymisessä (valvoja) -> tee pelaajakohtaisiksi kuten PER_PLAYER_STATE (world.rs).
8. Testaamatta pelissä: esinevertailu + jono (§33, Flame Barrel Memo hostille), HP/lepo/wipe (§35), valvoja (§32), Quit-esto.
9. Kaverin ini: `share_key_items = 1` tarkistettava.
10. Moninpeli: yhdistä v7.8 coop-multiin, sitten testiversio (3 pelaajaa) vasta kun jumi ratkaistu. Tunnetut: valvoja vain 2 pelaajalle.
11. Myöhemmin julkaisua varten: testityökalujen poisto (testsync, serve, upload-log), lisenssi, englanninkielinen ohje.

## 2026-10-27 v8.47 (coop-v7.1, committed, pushed with coop-multi)
- Lokianalyysi 05:48-06:04 (v8.46): Resurrect-umpikuja hostilla (hybrid vaihe B odotti signaalia jota ei tule), bossin desync (kaverin throw_window-luovutus bossille + hostin boss-jasenet kaverin type-22-listalla), jaatyminen (ENGAGE/DISENGAGE sailojen pysaytys 1,4-17,5 s). Korjaukset ks. JATKA-TASTA v8.47. Ei testattu pelissa.
- Seuraavaksi: pelitesti (Resurrect-pallo bossissa, `claimed by both`, `throw: handoff`, `stall:`), sitten P5 (handshake-generaatiot), P3, spectate-hiiriohjaus.

## 2026-10-27 v8.48 (coop-v7.1, committed + pushed)
- P5: vanhentuneet handshake-generaatiot ohitetaan, ENGAGED vain tarkalla generaatioparilla. Spectate: hiiri + nuolinappaimet ohjaavat kameraa (ini spectate_mouse, spectate_mouse_sens). Ei testattu pelissa. Ks. JATKA-TASTA v8.48.
- Seuraavaksi: pelitesti (v8.47 + v8.48 yhdessa), sitten P3 jos `claimed by both` jaa.

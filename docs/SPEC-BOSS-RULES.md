# SPEC: bossisäännöt ERSC:n tapaan (Sekiro 1.06, coop-dll) – 2026-10-23

Toteutusspeksi neljälle säännölle: katsojatila bossissa (1), bossin aloitusilmoitus (2), lepo
areenalla + turvallinen warp (3) ja bossipalkkio kaverille (4). Pohjana RE-ERSC.md §3–§4,
RE-DEATH-RESET.md, RE-FINDINGS-2.md §2–§4 ja tämän kierroksen IDA-ajot.

- IDA-kanta: `F:\Temp\re_boss\db.i64` (kopio `quick_re7.i64`), ajuri `F:\Temp\re_boss\job.py`,
  tulokset `F:\Temp\re_boss\o1 … o7` (`summary.txt` + `f_<ea>.c`).
- RVA = IDA-osoite − 0x140000000. Prologitavut on luettu kannasta (purettu muistidumppi).
- Merkinnät: **[VAR]** = dekompiloitu/luettu, **[ARV]** = päätelmä, **[WEB]** = julkinen lähde,
  **[LIVE]** = pitää todeta pelissä ennen kuin mikään riippuu siitä.
- Käyttäjän rajaus (2026-10-23): bossin saa aloittaa yksin. Kohta 2 on pelkkä ilmoitus, ei estoa.

## 0. Yhteiset palikat

### 0.1 "Olen bossitaistelussa" [VAR, RE-FINDINGS-2 §4]
```
MenuMan = *(base+0x3D67408)
in_boss = any s in 0..2: *(i32*)(MenuMan + 0x2E10 + 0xA8*s + 4) != -1
boss_entity = *(i32*)(*(base+0x3D5AAC0) + 0xC0)      // GameDataMan+0xC0, viimeisin bossi
boss_chr = 0x6D5B20(boss_entity, 0, null)            // GetChrInsByEntityId-kääre, null jos ei ladattu
```
- `0x6D5B20(int id, i64, i32* out)` palauttaa ChrIns:n tai 0 (ja `*out = -1`) [VAR].
- Kysely joka ruutu pelisäikeessä (`Puppet::on_frame`). Uusi funktio `game::boss_state() ->
  Option<BossState{entity, name_id, chr}>`, jossa `name_id = *(i32*)(slot+0)`.
- Reunat lokiin: `boss: gauge on entity E name N block B` / `boss: gauge off`.

### 0.2 Uudet verkkotapahtumat (world.rs, luotettava kanava)
| koodi | nimi | a | b |
|---|---|---|---|
| 6 | `EV_BOSS` | tila: 1 = alkoi, 2 = boss kaatui (HandleBossDefeat), 3 = palkki pois ilman kaatoa | boss entity id |
| 7 | `EV_DOWN` | 1 = kuollut ja katsojana (ei sitoutunut), 0 = katsojatila loppui | 0 |
| bulk 6 | `BULK_BOSS_REWARD` | `{i32 boss, u16 n, n × {i32 lot, u16 m, m × u32 lippu}}` | – |

Protokollaversio nousee yhdellä (molemmille sama build, kuten nyt).

### 0.3 Uudet hookit (yhteenveto, tarkemmin kohdissa)
| funktio | RVA | prologi | 5-tavuinen jmp siirtää | RIP-rel |
|---|---|---|---|---|
| kuoleman uudelleenlataus | `0xA8F7E0` | `40 53 55 56 57 41 56 48 83 EC 30` | 5 (`40 53 55 56 57`) | ei |
| CSDeadStep-predikaatti | `0xA740D0` | `40 53 48 83 EC 20 83 3D <rel32> 00` | 6 (`40 53 48 83 EC 20`) | **kyllä +06** (ei saa siirtää) |
| GameRend-kamerapäivitys | `0x83B150` | `48 8B C4 55 57 41 54 41 56 41 57` | 5 (`48 8B C4 55 57`) | ei |
| järjestelmätapahtuma (boss defeat) | `0x689260` | `48 83 EC 38 44 89 4C 24 20` | 9 (koko kaksi käskyä) | ei |
| item lot -anto | `0x918810` | `48 8B C4 55 56 57 41 54 …` | 5 (`48 8B C4 55 56`) | ei |

Kaikki tehdään `observe.rs`:n entry-hook-tyylillä: relocated prologi + `jmp` takaisin, detourin
sisään `jmp`:llä (ei `call`-patchia, koska `mov rax,rsp` -prologit tallentavat rsp:n). Tarkista
tavut ennen kirjoitusta kuten `redirect_function`; väärät tavut = ei asennusta + lokirivi.

## 1. Katsojatila bossissa

### 1.1 Mitä peli tekee kuolemassa [VAR]
1. Kuolema: `0x9EE7E0` → HP 0, `chr+0x1A11 |= 0x80`.
2. InGame-step `0xA71190` kysyy joka ruutu `0xA740D0(step)`: tosi, jos `g_deadLatch (base+0x3EA0D74) == 0`,
   `step+0x320 == 0` ja pääpelaajalla (`WCM+0x88`) on `f32 +0x212C > 0` tai `f32 +0x2138 >= 0`
   (elvytysikkunat, `0xA2B5F0`/`0xA2B610`) tai kuolematila `0x9E5C00(*(p+0x18))`. Silloin
   `0xA72A70` luo `CSDeadStep`in (vtable `0x2BD5BE8`) kohtaan `step+0x320`.
3. `CSDeadStep`in tilataulu `0x3EA0D80`: 0 = `0x11D4110` (valinta: `+0x2138 >= 0` → tila 1,
   `+0x212C > 0` → tila 2, kuollut → tila 5, muuten 7), 1/2 = `0x11D44F0`/`0x11D4730`
   (Ylösnouse/Kuole-MenuJobit), 3 = `0x11D42F0`, 4/5 = `0x11D4C80(a,4/5)`. Rangaistus
   `0x11D3430` (lambda `0x11D5460`) asettaa lopuksi `g_deadLatch = 1`.
4. Itse paluu idolille on **aina** `0xA8F7E0` (ei häivytystä sen sisällä: idoli `0x831940()`,
   GameMan-warpkentät, `0x6D7070` → `GameMan(0x3D5C0D0)+0x10 = 1` = lataus). Kutsujat [VAR]:
   EMEVD 2003[95] (`0x9076AB`, EMEDF-nimi `Unknown200395` [WEB soulsmods]), ESD-komento 131
   (`0x11C8FF7`), `0x11D34A9` (vain Remnant) ja matkavalikko `0x67971C` (vain Remnant).

Johtopäätös: `0xA8F7E0` on ainoa kuristuskohta, jonka kautta kuolema lataa kartan. Sen
viivästäminen ei estä kuolemaa, rangaistusta tai ruutua, vaan ainoastaan latauksen.

### 1.2 Kaksivaiheinen toteutus
**Vaihe A (turvallinen versio, tehdään ensin): "odota kuolinruudussa".**
- Hook `0xA8F7E0`. Detour:
  ```
  if spectate.state == Waiting { log "spectate: reload deferred (again) from {ret:x}"; return 0 }
  if spectate_wanted() { spectate.state = Waiting; spectate.since = now; send EV_DOWN(1); return 0 }
  return original()
  ```
  Paluuosoite (`[rsp]` detourin alussa) lokiin aina: `0x9076AB` = EMEVD, `0x11C8FF7` = ESD.
- `spectate_wanted()` = kaikki totta:
  - `!remnant` (`*(u32*)(EventFlagMan(0x3D55FE8)+0x220) == 0`; Remnantissa ohitetaan aina);
  - `in_boss` (§0.1) **tai** kaveri on ilmoittanut `EV_BOSS(1)` eikä vielä 2/3, ja olemme samassa blokissa;
  - linkki ylhäällä, kaverin snapshot `hp > 0`, kaveri ei `EV_DOWN(1)`- eikä `EV_DEATH`-tilassa;
  - kaveri samassa blokissa (`remote.block == current_block()`).
- Rangaistus (sen/XP, Unseen Aid, tauti) on jo tehty, kun tänne tullaan, ja `0x3EA0D74 == 1`.
  Nykyinen `EV_DEATH` lähtee siis jo silloin. Vastaanottaja tulkitsee: `EV_DEATH` + `EV_DOWN(1)` =
  kaveri katsojana, ei wipe-laskuria vielä (§1.5).
- Ruudulle `ui.rs`: "Katsojana: <nimi> taistelee – boss <hp %>. [F8] herää idolilla". Overlay
  piirretään Present-hookissa, joten se näkyy myös mustan päällä.
- **[LIVE]** Mitä ruudulla on viivästyksen aikana: "死"-ruutu, musta häivytys vai maailma.
  Jos maailma näkyy, kytke §1.3:n kamera päälle myös vaiheessa A.

**Vaihe B (täysi katsojatila): ei kuolinruutua lainkaan.**
- Hook `0xA740D0` (5-tavuinen jmp, siirrä 6 tavua, jatka `+6`:sta, älä siirrä RIP-relatiivista
  `cmp`:tä). Detour: jos `spectate_wanted()` ja pääpelaaja on kuollut **eikä** elvytysikkunaa ole
  (`f32 p+0x212C <= 0` ja `f32 p+0x2138 < 0` [ARV: `0xA26120` nollaa ne näin]), palauta 0
  ja aseta `spectate.state = Spectating` (+ `EV_DOWN(1)`). Muuten alkuperäinen.
- Vaikutus [ARV]: CSDeadStep ei synny: ei valikkoa, ei häivytystä, ei rangaistusta, `0x3EA0D74`
  pysyy 0. Ruumis jää maahan, HUD normaali (oma HP 0). Jos elvytys on tarjolla, peli näyttää
  normaalin Ylösnouse/Kuole-valinnan; "Kuole" vie vaiheen A polkuun.
- Turvaverkko: vaiheen A hook on aina päällä. Jos EMEVD/ESD kutsuu `0xA8F7E0`:aa itsenäisesti
  (ilman CSDeadStepiä), lataus viivästyy silti.
- **[LIVE]** Kuuleeko oma EMEVD pelaajan kuoleman muuta kautta (esim. bossi-event "pelaaja kuoli →
  ClearBossGauge"). Loki: `boss: gauge off` heti kuoleman jälkeen = oma bossi-event reagoi. Silloin
  `in_boss` luetaan kaverin `EV_BOSS`-tilasta, ei omasta palkista.

### 1.3 Kamera kaveriin: free cam -tila [VAR mekanismi]
`GameRend = *(*(base+0x3D5C0A0)+0x20)`. `0x83B150` (GameRend::Update) tekee:
- jos `*(i32*)(GameRend+0xE0) != 0`: kopioi `DebugCam = *(GameRend+0xE8)` → renderöity kamera
  `*(GameRend+0x18)` (matriisi `0x734240(DebugCam)` +0x10..+0x4F, fov/aspect/near/far +0x50..+0x5C)
  ja työntää sen `SprjCamera`-singletoniin `0xB22770(*(base+0x3D85008), cam)`.
- ChrCam (pelaajan oma kamera, `FieldArea+0x30`, iso päivitys `0x734790`) lukee kohteen
  WCM:stä suoraan (`0x739AF0` palauttaa vain WorldChrManin). Kohteen vaihto ChrCamissa olisi
  syvä muutos, joten **ei tehdä**.

Toteutus:
1. Spectate alkaa: tallenna `old = *(i32*)(GameRend+0xE0)`. Jos `*(GameRend+0xE8) == 0`, ei kameraa
   (vain overlay), loki `spectate: no debug cam`.
2. Hook `0x83B150` (entry). Ennen alkuperäistä, jos spectate on päällä:
   - `t = kaverin sijainti`: natiivi remote `native.net_remote()?.position()`, muuten
     `net.remote_smoothed(130)`. Lisää `+1.4 m` Y:hyn (rintakehä).
   - Kiertopiste: `yaw` = kaverin `angle`, pehmennetään (`yaw += (target-yaw)*0.08`, kulma kiedottuna).
     Kamera `c = t - 4.5·f(yaw) + (0, 1.6, 0)`, missä `f(yaw) = (sin yaw, 0, cos yaw)` **[LIVE: merkki;
     throw_window-koodin Facing-oppija antaa oikean konvention]**. Sijaintia pehmennetään 0.15/ruutu.
   - Matriisi rivivektoreina (RE-FINDINGS-2 §3): `fwd = norm(t - c)`, `right = norm(cross((0,1,0), fwd))`,
     `up = cross(fwd, right)`. Kirjoita DebugCam `+0x10 right, +0x20 up, +0x30 fwd (w = 0),
     +0x40 c (w = 1), +0x50 = 0.75` (fov rad). Aspect, near ja far jätetään ennalleen.
   - Aseta `*(i32*)(GameRend+0xE0) = 1`.
3. Spectate loppuu tai mikä tahansa lataus alkaa (`on_loading`): palauta `+0xE0 = old` **ennen**
   `0xA8F7E0`:n alkuperäistä kutsua. GameRend voi rakentua latauksessa uudelleen, joten
   osoitinta ei säilytetä latauksen yli.
- Riskit: kamera menee seinien läpi (ei törmäystä). Pad saattaa liikuttaa DebugCamia (DebugCamPad),
  mutta kirjoitamme joka ruutu juuri ennen kopiota. Elävällä pelaajalla tätä ei saa koskaan
  kytkeä päälle.

### 1.4 Lopetus (yksi funktio `spectate_release(reason)`)
| syy | ehto | toiminto |
|---|---|---|
| boss kaatui | kaverilta `EV_BOSS(2)` tai oma `0x689260`-hook (§4) | vapauta |
| wipe | kaverilta `EV_DEATH` / `EV_DOWN(1)`, tai snapshot `hp <= 0` > 1 s | vapauta heti, wipe-tila (§1.5) |
| kaveri lepää | `EV_REST` saapuu | vapauta (ERSC: "until party wipeout or bonfire rest") |
| palkki pois ilman kaatoa | `EV_BOSS(3)` | vapauta |
| linkki poikki | `!connected` > 5 s | vapauta |
| kaveri lähti blokista | `remote.block != here` > 5 s | vapauta |
| käyttäjä | F8 tai valikkonappi "Herää idolilla" | vapauta |
| aikaraja | 15 min | vapauta |

Vapautus pelisäikeessä:
1. Palauta kamera (§1.3 kohta 3).
2. Vaihe A (`Waiting`): `state = Releasing`, kutsu **trampoliinin kautta** alkuperäinen `0xA8F7E0()`.
   Peli latautuu idolille normaalisti.
3. Vaihe B (`Spectating`): `state = Releasing`. `0xA740D0`-hook palauttaa taas alkuperäisen,
   CSDeadStep syntyy seuraavassa ruudussa (rangaistus + kuolinruutu), ja sen myöhempi `0xA8F7E0`
   menee läpi, koska tila on `Releasing`.
4. Lähetä `EV_DOWN(0)`. Tila nollataan `on_loading`issa.

Bossin kaaduttua herätään siis idolilla (sama kuin ERSC:ssä grace). Kaatolippu on jo synkattu,
joten oma EMEVD pitää bossin kuolleena. "Herää paikallaan" (`0xA2CCF0` + teleport) olisi P4-luokan
riski (RE-DEATH-RESET), eikä sitä tehdä tässä.

### 1.5 Yhteensovitus P3:n (puppet.rs `death_sync`) kanssa
- Nyt wipe = toisen kuolema 90 s sisällä. Katsojatila voi kestää minuutteja, joten lisätään
  `World::friend_down: bool` (`EV_DOWN`) ja `World::spectating`. Wipe = `own_death_at` ja (kaveri
  `EV_DEATH` 90 s sisällä **tai** `friend_down`, kun oma kuolema sitoutui, **tai** me olimme
  katsojana, kun kaveri kuoli).
- Isäntä katsojana ja asiakas kaataa bossin: isäntä latautuu vapautuksessa, ja P3:n
  `EV_BACK`/`BULK_DEAD_LIST` palauttaa asiakkaan kuolleet viholliset. Boss pysyy kuolleena lipun takia.
- Isäntä katsojana: isännän maailma jatkuu ilman latausta, joten isännän bossin AI (auktoriteetti)
  jatkaa taistelua kaverin natiivia remotea vastaan [ARV]. **[LIVE]** Kohdistaako boss remoten,
  kun isäntä on kuollut (RE-ENEMY-TARGETING).

Lokit: `spectate: start (phase A|B) boss E friend hp H`, `spectate: release <syy> after N s`,
`spectate: reload deferred from <ret>`, `spectate: camera on/off (old E0 = X)`.

## 2. Bossin aloitus: ilmoitus kaverille (ei estoa)

- Lähetä `EV_BOSS(1, entity)`, kun `in_boss` vaihtuu epätodesta todeksi (§0.1). Palkki pois:
  jos 10 s sisällä tuli `0x689260`-osuma (§4), lähetä `EV_BOSS(2)`, muuten `EV_BOSS(3)`.
- Vastaanotto: overlay 8 s "<nimi> aloitti bossitaistelun" + etäisyys, jos sama blokki. Lisäksi
  vihje "Mene sumuportista perään", jos olemme > 30 m päässä ja elossa. Tieto tallennetaan
  `World::friend_boss = Some((entity, since))` (§1.2 käyttää tätä).
- Myöhempi liittyminen tapahtuu pelin omin keinoin: kaveri käyttää omassa maailmassaan sumuportin
  (EMEVD 3[13] `ActionButtonBossFog` [WEB soulsmods sekiro-emedf]), ja hänen oma EMEVD:nsä
  käynnistää taistelun.
  - **[LIVE]** Onko portti vielä käytettävissä, jos aloitusliput tulivat jo synkassa (oma EMEVD
    voi pitää taistelua "käynnissä" ilman pelaajaa).
  - **[LIVE]** Jos **asiakas** aloittaa yksin, isännän bossin AI voi olla passiivinen, kunnes
    isännän EMEVD aktivoi sen, ja NetAI seuraa isäntää [ARV].
- **Aloituslippujen nauhuri** (diagnostiikka, ei käytetä päätöksiin): kun `in_boss` nousee,
  kirjaa lokiin flag ringin (`hook::flag_ring_entry`) kirjoitukset viimeisiltä 10 s:lta ja
  seuraavilta 3 s:lta: `boss-flags: E: [(id, on, ms)…]`. Kerätään lista per bossi ilman
  EMEVD-arkistoja (BDT salattu). Käytetään myöhemmin, jos esto halutaan joskus vaihtoehdoksi.

## 3. Lepo areenalla: karkotus ja turvallinen warp

### 3.1 Sääntö
`World::apply_rest(idol, block)` saa `EV_REST`in. Uusi haara ennen nykyistä logiikkaa:
- **elossa ja `in_boss`** → *karkotus*: älä tee mailboxia äläkä `respawn_all`ia (ne resetoisivat
  bossin kesken EMEVD-tilan). Sen sijaan warp omalle viimeisimmälle idolille. Lataus luo maailman
  uudelleen, mikä vastaa kaverin lepoa.
  Overlay: "Kaverin lepo karkotti sinut areenalta".
- **katsojana** → `spectate_release("rest")` (§1.4).
- muuten nykyinen käytös (P2).

### 3.2 Kohde [VAR]
```
v = 0x831940()                       // GameMan+0xB50 jos >= 0, muuten GameMan+0xB4C (idol + 1000)
idol = v - 1000; if idol < 0 { idol = 1101950 }   // sama kuin matkavalikko 0x6795F0 case 1/2
```
- Param-indeksi kuten `0x6795F0`: käy läpi taulu `[0x10B7270(), 0x10B7340())`, 32 B/rivi.
  Jos `*(i32*)(row+4) == idol`, niin `idx = *(i16*)(row+22)`, muuten `idx = -1`.
- Kutsu `0x68E870(0, idol, idx)`. Kun `a3 < 0`, haara ohittaa param-asetuksen [VAR].
- Huom: join.rs antaa nyt `r8 = 0` (SekiroTool-tyyli). Se toimii, mutta `0` on oikea indeksi
  vain yhdelle idolille. Vaihda samaan hakuun.
- `0x68E870` asettaa GameManiin `+0x1C = idol-980`, `+0xB50 = idol+1000` ja kutsuu `0x6D7070` →
  `GameMan+0x10 = 1` (lataus pyydetty, `0x832EE0`) [VAR].
- Vaihtoehto ilman latausta (ERSC: delta karttablokkien välillä): tallenna oma sijainti 3 s ennen
  `in_boss`-nousua ("areenan suu") ja teleporttaa sinne. **Ei suositella**: bossin EMEVD-tila,
  palkki ja sumuseinä jäisivät päälle [ARV].

### 3.3 Turvallinen warp-tila: `game::safe_to_warp() -> Result<(), &'static str>`
Live-kaatuminen 2026-10-23: `sekiro.exe+0x11BAB3E`. Se on `0x11BAAF0`:ssä
`mov r8d,[rdi+rbx*4]`, kun `rdi` (talk-ESD-komennon argumenttiolio) = 0 [VAR]. Kutsujat ovat
ESD-talk-komentoja `0x11C9E90 … 0x11CD6A0` (dispatcher `0x11C63A0`/`0x11C1A20`) [VAR]. Tulkinta
[ARV, korkea]: peilattu lepo istutti isännän idolille (idolin talk-ESD-valikko auki), ja warp
latasi kartan talkin alta. Seuraavassa ruudussa talk-komento luki vapautetun kontekstin.
Ehdot (kaikki pelisäikeessä, samassa ruudussa juuri ennen warpia):
1. `player()` on olemassa ja `in_world_for >= 3 s`.
2. Ei latausta tulossa: `*(u8*)(*(base+0x3D5C0D0) + 0x10) == 0` [VAR: `0x832EE0` kirjoittaa sen].
3. Ei kuolemaa: `*(u32*)(base+0x3EA0D74) == 0` ja oma HP > 0.
4. Ei Remnantia: `*(u32*)(*(base+0x3D55FE8)+0x220) == 0` (`0xA90030`).
5. **Ei valikkoja:** `menu_mask = 0x8C58E0()` == 0 [VAR]. Funktio kokoaa MenuManin
   (`0x3D67408`) kentistä bittimaskin, eikä siinä näy sivuvaikutuksia. Bitti `0x80`
   (`0x8B61E0`: ikkuna 9 tila 4) on talk/valikko. Maski 0 = vapaa liikkumaan.
6. **Ei talkia:** `T = *(base+0x3EA0CE0)` (talk-ESD-manageri [ARV], sama singleton kuin
   kaatumisfunktiossa). Jos `T != 0`: vaaditaan
   `(*(i32*)(T+0x20EC) == -1 || *(u8*)(T+0x20E8) != 0) && *(f32*)(T+0x210C) <= 0 && *(f32*)(T+0x2110) <= 0`.
   Tämä on pelin oma ehto (`0x668CC0`) ennen tavaranoston kehotetta, kun maski = 0x80 [VAR].
7. Ei edellistä warpia 15 s sisällä (nykyinen `last_move`).

Jos jokin ehto ei täyty: odota ja yritä joka ruutu enintään 20 s. Loki
`warp: waiting (<syy>)` kerran sekunnissa. Aikakatkaisun jälkeen karkotus perutaan, ja loki
`warp: gave up (<syy>)`. **Kaikki** modin warpit (join, go_to_friend, auto_follow, karkotus)
kulkevat tämän portin läpi.

## 4. Bossipalkkio kaverille

### 4.1 Mekanismi [VAR]
- EMEVD 2003[12] `HandleBossDefeat(entity)` → `0x689260(*(EventMan(0x3D55600)+8), 0xFD2, entity, 1)`.
  2003[74] `HandleBossDefeatAndDisplayBanner` → sama, `a4 = banner`. `0x689260` → `0x68CC90(…, 54, …)`
  (järjestelmätapahtuma 54, id 4050; moninpelissä myös `0x8D9E80`-viesti).
- EMEVD 2003[4] `AwardItemLot(lot)` → `0x689FF0` → **`0x918810(*(base+0x3D6CDC0), lot, 1)`**.
  2003[36] `AwardItemsIncludingClients` → sama, `a3 = 0`. Muut kutsujat: `0x688360`, `0x68A030`
  ja `0x68D240` (ESD/Lua). `0x918810` kutsuu `0x91C970`:aa (GiveItem, A17 Sekiro Onlinessa) ja
  `0x6AAAB0`:tä (SetEventFlag-kääre: tavaran lippu).

### 4.2 Toteutus
**Tappajan puoli** (se, jonka maailmassa `0x689260` ajetaan, `edx == 0xFD2`):
1. Hook `0x689260` (entry, siirrä 9 tavua). Jos `edx == 0xFD2`: `boss_defeat = Some((r8d, now))`,
   loki `boss: defeated E (banner r9d)`, lähetä `EV_BOSS(2, E)`.
2. Hook `0x918810` (entry, siirrä 5 tavua). Jos `boss_defeat` on alle 15 s vanha: ota `lot = edx`,
   merkitse flag ringin nykyinen laskuri, ja kutsun jälkeen (detour kutsuu trampoliinin ja palaa)
   kerää ringistä tämän kutsun aikana asetetut liput. Ne ovat lotin hankintaliput.
3. 15 s kuluttua kaadosta lähetä `BULK_BOSS_REWARD {E, lots}` ja tyhjennä.
**Saajan puoli:**
1. Odota 10 s saapumisesta (oma EMEVD ehtii antaa luonnollisesti, jos lippusynkka tai bossin HP-peili
   laukaisi oman eventin) ja tarkista samat `safe_to_warp`-ehdot 1, 3, 4 ja 5 (ei valikkoa,
   elossa, ei Remnantia).
2. Jokaiselle lotille: jos **kaikki** sen liput ovat jo päällä (`GetEventFlag 0x6C3E60`), ohita
   (`reward: lot L already owned`). Muuten `0x918810(*(base+0x3D6CDC0), L, 1)` pelisäikeessä.
   Peli näyttää oman "esine saatu" -ikkunansa ja asettaa liput.
3. Bossin kaatolippu tulee nykyisestä lippusynkasta (k = 0/5 jaettu). Siihen ei tarvita mitään uutta.
- Kaiku: `0x918810`-hook ei kerää, kun kutsu on omamme (`reward_applying`-lippu).
- **XP/kokemus:** ei toteuteta oletuksena. **[LIVE]** Mittaa `pgd+0x160` (`*(GameDataMan+8)+0x160`)
  saajalla ennen ja jälkeen bossin kuoleman. Jos XP:tä ei tule, myöhempi lisäys tarvitsee pelin
  oman XP-lisäysfunktion (ei selvitetty). Suora kirjoitus ohittaisi taitopisteiden laskennan [ARV].
- Lokit: `reward: sent N lots for boss E`, `reward: gave lot L (flags …)`, `reward: lot L already owned`.

## 5. Tiedostot ja järjestys

1. `game.rs`: `boss_state()`, `menu_mask()`, `talk_idle()`, `safe_to_warp()`, `last_idol()`,
   `idol_param_index()`, `warp_to_idol(idol)`.
2. `hook.rs`/uusi `bossrules.rs`: hookit `0xA8F7E0`, `0xA740D0`, `0x83B150`, `0x689260`, `0x918810`
   (§0.3) ja tilakone `Spectate {Off, Waiting, Spectating, Releasing}`.
3. `world.rs`: `EV_BOSS`, `EV_DOWN`, `BULK_BOSS_REWARD`, `apply_rest`-haara (§3.1), `friend_boss`,
   `friend_down`.
4. `puppet.rs`: kutsut `on_frame`issa, `death_sync`-wipe (§1.5), `on_loading` → kamera pois + tila nolla.
5. `join.rs`: warp `warp_to_idol` + `safe_to_warp` -portin kautta.
6. `ui.rs`: overlay-tekstit, F8/"Herää idolilla".
7. `tuning.rs`: `BOSS_SPECTATE` (off/A/B, oletus A), `BOSS_EXPEL` (on), `BOSS_REWARD` (on).

Toteutusjärjestys: §3.3 (korjaa nykyisen kaatumisen) → §0.1 + §2 → §1 vaihe A → §4 → §3.1 → §1 vaihe B + kamera.

## 6. Testit pelissä (hyväksyntä)
- **Warp-portti:** idolilla istuessa "Siirry kaverin luo" → loki `warp: waiting (talk)`, ei kaatumista.
  Valikosta poistuttaessa warp lähtee.
- **Palkki:** Gyoubu: `boss: gauge on …`, sitten kaato → `boss: defeated …`, `gauge off`.
- **Vaihe A:** kuole Gyoubulle kaverin eläessä → `spectate: reload deferred from 0x…` (kirjaa
  EMEVD/ESD), ei latausta. Kaveri kaataa → lataus idolille ≤ 2 s. Kaveri kuolee → molemmat idolille,
  ja P3-loki "wipe".
- **Vaihe B:** ei kuolinvalikkoa, kamera kaverissa, `GameRend+0xE0` palautuu, ja seuraava kuolema
  bossin ulkopuolella toimii normaalisti.
- **Karkotus:** kaveri lepää, kun olen bossissa → warp omalle idolille, boss täysillä HP:illä
  latauksen jälkeen.
- **Palkkio:** kaatajalla `reward: sent`. Toisella muisto/esine inventaariossa tasan kerran (ei tuplana).

## Lähteet
- [WEB] https://soulsmods.github.io/emedf/sekiro-emedf.html (2003[4] AwardItemLot, 2003[11]
  DisplayBossHealthBar, 2003[12] HandleBossDefeat, 2003[36] AwardItemsIncludingClients,
  2003[74] HandleBossDefeatAndDisplayBanner, 2003[95] Unknown200395, 3[13] ActionButtonBossFog)
- [WEB] ERSC-käytös: RE-ERSC.md:n lähteet (pcgamer, gamepressure, ersc-docs).
- Sekiron EMEVD-lippulistoja ei löytynyt julkisesti tällä haulla. Aloitusliput kerätään
  ajonaikaisesti (§2).

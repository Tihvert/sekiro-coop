# Avoin lähdekoodi, osa 2: vihollis-/pelaajasynkka, auktoriteetti, kuolema (2026-10-23)

Jatkaa `WEB-OPEN-SOURCE-COOP.md`. Lähteet luettu raakatiedostoina (curl raw.githubusercontent.com tekstinä scratchpadiin, ei binäärejä, ei asennuksia). Mitään ei kopioitu AGPL/GPL-koodista; vain ideat ja offsetit.
Merkinnät: **[VAR]** = luettu suoraan lähteestä. **[ARV]** = oma päätelmä/arvaus. **ER-kenttä ≠ Sekiro-kenttä**: kaikki Elden Ring (ER) -offsetit ovat ER:n, ellei toisin sanota; Sekiro-vastine on aina [ARV], kunnes tarkistat sen elävästä pelistä.
Oma aiempi RE (`RE-FINDINGS-2.md`, `IMPLEMENTATION-NOTES.md`) on merkitty [OMA].

## Lähdeluettelo (viitataan tunnuksilla)

- U1 https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/docs/GAPS.md
- U2 https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/docs/HANDOFF.md
- U3 https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/crates/sekiro-sdk-sys/src/live.rs
- U4 https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/crates/sekiro-sdk-core/src/enums.rs
- U5 https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/crates/sekiro-coop-emevd/src/catalog.rs
- U6 https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/net_chr_sync.rs
- U7 https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/chr_ins.rs
- U8 https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/chr_manipulator.rs
- U9 https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/darksouls3/src/sprj/chr_ins.rs
- U10 https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/darksouls3/src/sprj/chr_module.rs
- U11 https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/CHANGELOG.md
- U12 https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/WORLD-SYNC.md
- U13 https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/KNOWN-LIMITATIONS.md
- U14 https://raw.githubusercontent.com/micthiesen/unseamless-coop/HEAD/docs/ARCHITECTURE.md
- U15 https://raw.githubusercontent.com/micthiesen/unseamless-coop/HEAD/docs/SDK-COVERAGE.md
- U16 https://soulsmods.github.io/emedf/sekiro-emedf.html (EMEDF, WebFetch-tiivistelmä, sivu katkesi ~100 kB kohdalla)
- U17 https://soulsmodding.com/doku.php?id=topic%3Adsr_gamedata_communication (DS1 Remastered, WebFetch-tiivistelmä)
- U18 https://raw.githubusercontent.com/ElaDiDu/Sekiro-Practice-CT/main/Ela_Sekiro_Table.CT (Sekiro 1.06 CE-taulu, XML-teksti)
- U19 https://github.com/TLeonardUK/ds3os ja https://raw.githubusercontent.com/TLeonardUK/ds3os/HEAD/Protobuf/DarkSouls3/DS3_Frpg2RequestMessage.proto
- U20 https://github.com/garyttierney/ds3-open-re (puun lista)
- U21 https://github.com/scheissgeist/Seamless/blob/main/PROCESS.md
- U22 https://www.nexusmods.com/sekiro/mods/577 (Sekiro Online, hakutulos, ei koodia)
- U23 https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/RESEARCH-NOTES.md

## 0. Tärkein löytö: sinun Sekiro-taulusi on ER:n NetChrSetSync

ER:n `NetChrSetSync` (0x50 B, yksi per ChrSet) [VAR U6]:
`+0x10 capacity(u32)`, `+0x18 unk18_readback`, `+0x20 placement_readback_values`, `+0x28 unk28_readback`, **`+0x30 health_readback_values`**, **`+0x38 update_flags (u16[])`**, `+0x40 unk40_readback`, `+0x48 unk48_readback`.
Docstring: "P2P update tasks will populate the arrays with received values and toggle the readback flag".
`ChrSyncUpdateFlags` (u16): **bitti 0 = placement-päivitys odottaa, bitti 4 = health-päivitys odottaa** [VAR U6].
`ChrSyncHealthUpdate` = `{current_hp: i32, damage_taken: u32}` (8 B) [VAR U6].

Sinun Sekiro-lohkosi [OMA, RE-FINDINGS-2 § Enemy AI authority]: `+0x10 count`, `+0x18 omistus i16[]`, `+0x20 movement (28 B)`, `+0x28 behavior (344 B)`, `+0x38 u16 flags`, `+0x40 action (48 B)`; liput `0x01/0x02` movement, `0x04/0x08` behavior, `0x40/0x80` action (pending/written).

Täsmäys [ARV, vahva]: `+0x10`, `+0x20` (placement=movement), `+0x38` (update_flags) ja `+0x40` osuvat täsmälleen ER:ään. Sekiron lohkon `+0x30` on siis tuntematon osoitin, joka on ER:ssä **health-readback ({hp, damage_taken})**. Liputusparit (0x01/0x02, 0x04/0x08, 0x40/0x80) viittaavat siihen, että **0x10 = health pending (client) ja 0x20 = health written (host)** on kirjaamatta. ER:ssä health on juuri bitti 4 = 0x10. Tämä on todennettavissa 2 minuutissa: dumppaa lohkon `+0x30`-osoitin ja `flags[i]` ennen/jälkeen sen, kun hyökkäät viholliseen hostilla.
Seuraus: pelin oma natiivi HP-/kuolemapolku voidaan ajaa samalla tavalla kuin movement/behavior: kirjoita `{hp, damage_taken}` + pending-bitti, ja peli soveltaa sen NetAI-puolella itse.

## 1. Kysymys (a): miksi manipulaattorin valinta palauttaa paikallisen tekoälyn

Ulkoisista lähteistä ei löydy dokumentoitua ER/Sekiro-funktiota, joka vastaisi `0xA14B50`:ää. Rehellinen tulos: **mikään avoin lähde ei selitä valintaehtoa**; selitys on sinun omassa dekompilaatiossasi.

Mitä lähteet antavat:
- ER:n `ManipulatorType`: Default=0, Pad=1, **Network=2, Replay=3, NetAi=4**, Com=5, Ride=6, Follow=7. Vtable slot 2 = `manipulator_type()` [VAR U8]. Tarkista Sekirossa, että `0x2000`/`0x2008`-manipulaattorin vtable slot 2 palauttaa 5 / 4 [ARV].
- ER `EnemyIns`: `com_manipulator`, **heti perässä `net_ai_manipulator`**, sitten `ride_manipulator` [VAR U7]. Sekirossa `+0x2000` Com, `+0x2008` NetAI [OMA]: sama järjestys, joten rakenne on sama suku [ARV].
- ER `ChrManipulator` (0xC0 B): `network_warp_distance` f32 (+0xA0, "TODO fact-check"), `owning_chr` +0xA8 [VAR U8]. Eli NetAI-/Network-manipulaattorissa on oma etäisyysraja, jonka yli se teleporttaa; sama kuin sinun "snap speed/2 s/90 deg" -havainto [ARV].
- DS3-rakenne (fromsoftware-rs): ChrIns `+0x50` = player/ChrCtrl, **`+0x58` = manipulator-osoitin**, `+0x1F90` = moduulit; PlayerIns `+0x1FB0` = `_net_ai_manipulator` [VAR U9, nimet DS3-kaivajalta]. Sekiro: `chr+0x50` ChrCtrl ja `chr+0x58` aktiivinen manipulaattori [OMA] täsmäävät DS3:een. Moduulisäiliö (`modules`): `+0x00 action_flag, +0x08 behavior_script, +0x10 time_act, +0x18 data (HP), +0x28 behavior, +0x30 behavior_sync, +0x38 ai, +0x68 physics` [VAR U10, "copied from ER"]. Sekiro-ketjut `+0x1FF8 -> +0x18 -> +0x130` (HP), `-> +0x68 -> +0x80` (sijainti), `-> +0x10 -> +0x20` (anim-id), `-> +0x28 -> +0xD00` (play speed) täsmäävät tähän indeksöintiin [VAR U3; U18 sisältää samat ketjut].
- Kenttä, jota lipuksi epäillään: ER:n `ChrIns` sisältää `network_authority: u32` (+0x1E4), `NetChrSyncFlags` (u8, +0x1C9: bitti 5 `distance_based_network_update_authority`, bitti 3 `replay_recorder_enabled` = päähahmo), `ChrInsActivationFlags.is_left_behind` (bitti 0), `ChrLoadState` bitti 10 `host_inactive`, ja `net_position_synchronized` (+0x364, "set by NetAIManipulator after receiving a position update") [VAR U7].
- Hypoteesi [ARV]: sinun `0x9F33F0(chr)` ("jokin chr-lippu", slot 37:n `forceLocal`) on yksi näistä: todennäköisimmin `is_left_behind` tai `distance_based_network_update_authority`-ehto. Peli pakottaa viholliset paikalliseksi, kun ne ovat "liian kaukana muista pelaajista" (left behind) tai kun etäisyysauktoriteetti sanoo, että tämä kone omistaa ne. Offline-sessiossa tämä on aina tosi, koska "muita pelaajia" ei ole. Näin **40 %:n ruutuosuus ei ole sattumaa**: se seuraa etäisyyttä pelaajaan. Tarkistus: dekompiloi `0x9F33F0` ja katso, lukeeko se etäisyyskenttää (ER-kentät: `distance_to_player_sqr`, `chr_activate_threshold`, `update_priority`; Sekiro-offsetit tuntemattomia) [VAR U7 ER-kentille].
- Sinun slot 37 -ohitus (client kutsuu `0xA14B50(chr,0)` rekisteröidyille) ohittaa `forceLocal`:in; jos `0x9F33F0` silti vaikuttaa muualle (esim. AI-päivitys, ChrCtrl `+0x120` rebind), pidä sen lähdekenttä nollattuna samalla tavalla kuin `+0x2040` [ARV].
- U-lähteiden `u16 flag`: ER:ssä u16-liput (`update_flags`) ovat **vastaanotettu päivitys odottaa** -bittejä, ei auktoriteettibittejä [VAR U6]. Jos valinta riippuu jostain u16-liputuksesta, se on tulkittava "onko tältä netId:ltä saapunut tuore päivitys" -ehtona; omistus on erillinen `i16[]` lohkossa `+0x18` [OMA].

## 2. Kysymys (b): auktoriteetin vaihtuminen etäisyyden mukaan

- ER: `NetChrSyncFlags` bitti 5 "When set, the character will use distance-based network update authority" [VAR U7]. `ChrInsActivationFlags` bitti 0 `is_left_behind`: "too far from other players" [VAR U7]. Lisäksi `distance_based_update_priority_sqr`, `update_priority`, `omission_mode` (NoUpdate=-2, Normal=0, 1/5/20/30 FPS) ja `ChrInsFlags1c4` bitti 0 `skip_omission_mode_updates`, bitti 1 `force_update` ("reset every frame") [VAR U7]. Päivitystiheyden harvennus (omission) on siis auktoriteetista erillinen mekanismi, jonka voi sekoittaa auktoriteettiin [ARV].
- EMEVD (Sekiro): `2004[28] SetNetworkUpdateAuthority(int entityId, int<AuthorityLevel>)` ja `2004[34] SetNetworkUpdateRate(int entityId, bool useFixedFrequency, sbyte<CharacterUpdateFrequency>)`, `2000[02] SetNetworkSyncState(enabled)`, `2003[62] SetNetworkConnectedEventFlag`. `AuthorityLevel`: **Normal=0, Forced=4095**; `CharacterUpdateFrequency`: NoUpdate=-1, AlwaysUpdate=0, 2, 5 [VAR U16 (instruktiot), U4 (enum-arvot, kommentti viittaa DarkScript3-EMEDF:iin)]. Sekiron EMEDF merkitsee 28:n "Unused" (ei käytetä vanilja-eventeissä) [VAR U16, hakutiivistelmä].
- Tulkinta [ARV]: Normal = etäisyys-/oletusauktoriteetti, **Forced = tämä vertaisko omistaa entiteetin pakolla**. ER-wikin esimerkissä pomolle kutsutaan `SetNetworkUpdateRate(..., AlwaysUpdate)` AI:n käynnistyksen jälkeen (hakutulos soulsmodding.wikidot.com ER EMEVD -opas). Sama pari voi pitää clientin NetAI-vihollisen päivitettynä joka ruutu ja estää "harvennuksen" [ARV]. Kutsu natiivifunktiota, jonka EMEVD-instruktiotaulu osoittaa luokan 2004 indeksille 28/34 (sekiro-coop ei ole löytänyt AOB:ta, GAPS #14 [VAR U1]).
- DS1R (DSR): vihollisen sijaintipaketti `WorldChrSyncLocation` lähetetään jatkuvasti **jokaiselta pelaajalta** ja lähettäjä vaihtuu "tuntemattomien muuttujien" mukaan; AI-tila kulkee molempiin suuntiin (`..RemoteToHostEnemyAI`, `..HostToRemoteEnemyAI`) [VAR U17]. Eli alkuperäinen suunnittelu ei ole puhdas host-auktoriteetti; sinun host-only-malli on yksinkertaistus, joka on turvallinen koska ownership-taulu on `i16[]` ja voit pitää sen vakiona [ARV].
- BBSC: "Enemy / boss AI: Host, HYPOTHESIS"; kun vieras jäi client-tilaan ja host-hahmoa ei luotu uudelleen, vieraan viholliset jäätyivät [VAR U12, U23]. Tukee: client odottaa isännän tilaa, kun ownership ei ole paikallinen.
- unseamless-coop ja ds3os: ei mitään tästä aiheesta. unseamless-coop delegoi kaiken pelin natiiville synkalle ja kirjoittaa vain `session_player_limit`-kentän joka ruutu [VAR U14]; sen STATE-dokissa ER:n kaukopelaaja ei vielä edes spawnaa (`join_wait=true` jumissa) [VAR https://raw.githubusercontent.com/micthiesen/unseamless-coop/HEAD/docs/STATE.md]. ds3os on pelkkä palvelinemulaattori: protobuf sisältää vain `RequestNotifyKillEnemy`-tyyppisiä ilmoituksia, ei pelinsisäistä P2P-synkkaa [VAR U19]. ds3-open-re: vain proto + palvelin [VAR U20].

## 3. Kysymys (c): vihollisen kuolema ja HP luotettavasti

Mekanismit, jotka toimivat muissa:
1. **ER/natiivi health-kanava**: `{current_hp, damage_taken}` per entiteetti, pending-bitti, peli kuluttaa sen seuraavassa tikissä [VAR U6]. `damage_taken` on kumulatiivinen-vahinko-kenttä (ER:ssä "damage taken since last sync" [VAR U6]); sen avulla kahden koneen vahingot voi laskea yhteen ilman että HP vain ylikirjoitetaan. Ks. kohta 0 (Sekirossa lohko `+0x30`, lippu 0x10/0x20 [ARV]).
2. **Kuolema on lippu-/tilakone, ei pelkkä HP**. BBSC: kuoleman käsittelijä asettaa HP 0 **ja** kuolemalippuja; revive ilman lippujen nollausta jätti hahmon "näkymättömäksi ja jäätyneeksi" [VAR U11, 2026-09-16 "Host death kick found"]. ER: `death_flag` ChrIns+0x1C5 bitti 7 ("Controls whether the character is dead or not"), `has_dropped_item` (+0x1C6 bitti 0) ja `has_dropped_runes` (bitti 1) estävät tuplapudotuksen [VAR U7]. Sekiro-ankkuri: CE-taulun "Draw" = ChrIns+0x1A11 bitti 3 [VAR U18], ER:ssä `enable_render` on samassa tavussa bitti 3 (`ChrInsFlags1c5`), jossa bitti 4 = invincible ja **bitti 7 = death_flag**. Joten Sekiron `ChrIns+0x1A11` bitti 7 on todennäköisesti kuolemalippu [ARV, keskitasoa; offset-siirtymä ER:n ja Sekiron välillä ei ole vakio, ankkuri on vain bittiryhmä]. Tarkistus: diffaa `chr+0x1A10..0x1A20` elävän ja kuolleen vihollisen välillä.
3. **BBSC:n kuolemanrelay (reliable-lippu)**: "dead-lippu" kulkee omassa paketissa: `{u32 satunnainen pelaaja-id, u8 dead, u32 sequence}`, **2 Hz, jatkuvasti**; vastaanottaja pitää tilan voimassa 3 s ja kaikkien tuoreiden raporttien "dead" ratkaisee. Syy: toisella koneella kuolleen hahmon kopion HP pysyi > 0, joten HP-vertailu näytti sen elossa [VAR U11, 2026-10-04 (b) "mod 10"]. Sama idea sopii vihollisille: 2 Hz idempotentti `dead(enemy_id)`-lista isännältä, ei kertaluonteinen tapahtuma.
4. **DSR**: `WorldChrSyncHealth` lähetetään jatkuvasti; liittyessä vieras pyytää `RequestWorldChrSyncInitInfo`, ja isäntä lähettää `EnemyInit` (sijainti, toiminta, HP per vihollinen) sekä `WorldInit` (isäntä listaa kuolleet viholliset) [VAR U17]. Eli: jatkuva HP-tila + liittymissnapshot + erillinen "kuolleet"-lista. Sinulla on liittymissnapshot (flags); lisää kuolleiden listan uudelleenlähetys.
5. **Sekiro Online -muistiinpano**: v1.01-julkaisutiedoissa viholliskuolemat synkkautuvat "apart from bosses and minibosses (WIP)", ja varhaisessa versiossa vihollinen saattoi olla kuollut yhdellä ja elossa toisella [VAR U22, hakutiivistelmä, ei koodia]. Eli ongelma on tunnettu myös Yuin Sekiro-modissa; pomot ja minibossit ovat hankalin tapaus.
6. **sekiro-coop**: HP-kirjoitus ketjulla `+0x1FF8 -> +0x18 -> +0x130`, vain vähennys (decrement-only), isäntä ei hyväksy clientin vihollistilaa [VAR U2, U3]. Ei kuolemalogiikkaa (GAPS #6 "Death event wiring: not wired" [VAR U1]).
7. **BBSC pomokuolema**: pomon kuolemaskripti nollaa HP-palkin ENNEN kuolemaa, joten "onko pomotaistelu" pitää muistaa 10 s [VAR U11, 2026-09-30 (b)]. Jos käytät pomopalkkia kuolemantunnistukseen, tee sama.

## 4. Kysymys (d): pelaajan haamuhahmon animaatiosynkka

- **DSR (DS1R)**: pelaajapaketti `PlayerFrameUpdate` (tyyppi 1, 32 tai 40 B) sisältää sijainnin, kierron, HP:n, maksimi-HP:n ja **ezstate-animaatiotiedon**; lähetetään ajastimella **10 ruudun välein** (~6 Hz) [VAR U17]. Eli sijainti ja animaatiotila kulkevat saman absoluuttisen tilapaketin mukana, ei erillisinä tapahtumina. DS1R:n ezstate ≠ Sekiron hkb (Sekiro käyttää behavior-synk-moduulia, kts. alla) [ARV].
- **DS3-suku (BBSC, DS3 → Sekiro)**: animaatiot/hyökkäykset/väistöt kulkevat natiivisti `SprjChrBehaviorSyncModule`:n kautta (STRONGLY INDICATED, ei täysin varmistettu) [VAR U12]. Sekirossa vastaava on `modules+0x30` (BehaviorSync) [VAR U10 + OMA]. Tämä sopii sinun "Path B" -ratkaisuun (hkb-tapahtumat + muuttujalohko, tilaero-toisto) [OMA].
- Mikään avoin lähde ei dokumentoi TAE-ajan tai anim-id:n vastaanottoa NetworkManipulatorissa. ER:n `CSChrBehaviorModule` (0x19C0 B) paljastaa vain `animation_speed` (+0x17C8) ja `root_motion`; `CSChrActionRequestModule` `npc_action_id` +0xF4 ja `queue_tae_id_override` +0x134 [VAR; ER, WEB-OPEN-SOURCE-COOP.md §4.1]. Anim-ID-polku on ER-erityinen eikä ole todettu Sekirossa [ARV].
- Sekiro-CE-taulu tarjoaa lukupuolen: nykyinen animaatio `ChrIns->+0x1FF8->+0x10->+0x20` (id), `->+0x28->+0x878` ("Current Animation Name"), play speed `->+0x28->+0xD00` [VAR U18]. Nimi (+0x878) voi auttaa vertaamaan koneiden hkb-tilaa lokissa [ARV].
- hkb-muuttujat `MoveSpeedLevel`/`MoveAngle`: avoimista lähteistä ei löydy; sinun oma RE kuvaa jo, että replay-ghost ajaa ne `ReplayManipulator+0x10`-suuntavektorista (|delta|·30 >= 2.5 -> 1.0 muuten 0.5) [OMA]. Se on paras tunnettu tapa ja täsmää BBSC:n havaintoon, että natiivi synkka ajaa liikkeen manipulaattorin kautta [ARV].
- DS3-suvun `PlayerIns` sisältää `_net_ai_manipulator` (+0x1FB0 DS3:ssa, heti `player_game_data`:n jälkeen) [VAR U9]. Sekiron PlayerIns alkaa `+0x2000`:sta (`PlayerGameData`, CE: "Sen/Level" ketjussa `+0x2000`) [VAR U18], joten PlayerIns:n NetAI/Network-manipulaattori on todennäköisesti `+0x2010` tienoilla [ARV, tarkista skannaamalla `0x2008..0x2030` osoittimet ja kutsumalla vtable slot 2].
- BBSC:n huomio kuolleen pelaajan kopiosta: etäkopio voi näyttää kuolleelta, vaikka paikallinen on elossa; BBSC "partner revive" ajaa kuolemaliput nollaavan revive-polun etäkopiolle kun synkattu HP > 0 yli 1 s ja kuolemalippu on yhä päällä [VAR U11, 2026-09-16 "Pre-test verification pass"]. Sama koskee sinun ghostiasi respawnin jälkeen.

## 5. Sijainnin kirjoitus "ei vaikuta": vihje

- sekiro-coop: kirjoitus `ChrIns +0x1FF8 -> +0x68 -> +0x80` onnistuu mutta renderöinti/fysiikka ei seuraa [VAR U2].
- ER `ChrCtrl.chr_proxy_flags` bitti 0 `position_sync_requested`: "When 1, underlying havok character position will be updated with the position from the physics module", bitti 1 vastaavasti kierrolle [VAR U7]. Siis: physics-moduuliin kirjoitettu arvo ei siirrä havok-hahmoa, ellei tämä lippu ole päällä. Sekirossa ChrCtrl on `chr+0x50` [OMA], joten etsi sieltä vastaava u32-lippukenttä (ER:ssä ChrCtrl `+0xFC`; Sekiron offset voi poiketa) [ARV, matala]. NetAI-polussa tätä ei tarvita, koska manipulaattori liikuttaa hahmoa `vtable slot 41` -deltalla [OMA]; hyödyllinen vain jos haamu ajetaan suoraan sijainnilla.
- BBSC: sisäinen teleportti ilman latausta tehdään `chr->vtable[0x5F8](chr, &pos, &rot, 1)` ja sitten `chr+0x1E1 |= 2`; sijainti `[[[[chr+0x58]+0x08]+0x3B0]+0x68]+0x1E0` [VAR U11, 2026-09-16 "Seamless Co-op death rules"]. Bloodborne-osoitteita, mutta mekanismi (virtuaalinen warp-funktio + "päivitä"-lippu, ei raaka kirjoitus) on sama oppi kuin sekiro-coopin [ARV].

## 6. Mitä sekiro-coop (AGPL) oikeasti sisältää (kysytty erikseen)

Rust-lähteet luettu; paljon rakennetta, vähän RE-dataa:
- `worldchrman.rs`: array-of-pointers -kävelijä, offsetit `UNRESOLVED`; `worldchrman_scan.rs` sokea skannaus (kaatuu) [VAR U1, U2]. **Ei WorldChrMan-offsetteja.**
- `chrins.rs`: `ChrInsLayout`, kaikki kentät `UNRESOLVED` (entity_id, hp, anim, `network_authority`, `ai_command`...). Käytännössä käytetään vain `live.rs`:n tilamoduuliketjuja [VAR U3].
- `offsets.rs`: vain `libsekiro`-versiokohtaiset RVA:t (quitout, igt, player_position, fps...), v1.06: `player_position 0x3d7a1e0`, `igt 0x3d5aac0` [VAR]. CE-taulun v1.06 `WorldChrMan staticOffset 0x3d7a1e0`, `WorldChrManDbg 0x3d7a388` [VAR U18].
- Ketjut `live.rs`:ssä: `WorldChrMan->+0x88 (pelaaja)->+0x1FF8->+0x18->{+0x130 HP,+0x134 maxHP,+0x148 posture,+0x14C maxPosture}`, `->+0x68->+0x80 pos`, `->+0x10->{+0x20 anim, +0x24 kulunut aika}`, `->+0x28->+0xD00 play speed`; ChrIns `+0x08` handle, `+0x68` char id, `+0x74` team type [VAR U3]. CE-taulu vahvistaa saman ja lisää `+0x228` (data-moduulin debug-liput: No Dead/No Damage), ChrIns `+0x1F40` bitit 5/6/7 = No Hit/No Attack/No Move, `+0x1F41` bitti 4 = "No Update", `+0x1F42` bitti 4, `+0x1A11` bitti 3 = Draw [VAR U18].
- `catalog.rs`: 386 EMEVD-instruktiota ja argumenttispesifikaatiot (esim. 2003[01], [18], 2004[01], [03], [04], [15], [28], [34]) [VAR U5]. `enums.rs`: TeamType (Host 19, Coop 20, Hostile 21, Enemy 6, CoopNpc 28...), AuthorityLevel, UpdateFrequency [VAR U4].
- `docs/GAPS.md`: sama lista avoimia kohtia, joita sinä olet ratkonut (NetAI, authority-AOB, kuolema); mitään ei ole ratkaistu. EMEVD-reitti (UXM + Yabber -> `common.emevd.dcx` -> lippu laukaisee animaation) on suunniteltu, "not started" HANDOFF:ssa (päivätty 2026-04-19) [VAR U2]. **Ei todiste siitä, että EMEVD-reitti toimii.**
- `crates/sekiro-coop-rollback`, `-authority`: protokolla- ja rollback-runko (handoff-hystereesi, 200 ms timeout, deterministinen RNG), ei kytketty peliin [VAR U1, U2]. Idea: omistussiirto kuittauksella (HandoffAck) on hyvä malli, jos joskus siirrät auktoriteettia asiakkaalle.

## 7. Negatiiviset tulokset

- Ei yhtään avointa dokumenttia ER/Sekiro-`NetAIManipulator`-luokasta tai manipulaattorin valintaehdosta (haettu nimillä NetAiManipulator, ChrNetworkManipulator, SetNetworkUpdateAuthority + ChrIns).
- unseamless-coop: ei vihollis-/pelaajatilasynkkaa (ER:n "planned, the game's job; rig-gated") [VAR U14].
- ds3os / ds3-open-re: vain palvelinprotokolla; Tim Leonardin sarja ei kata pelinsisäistä P2P-vihollissynkkaa (kts. ensimmäinen raportti).
- Souls Modding Wikissä verkkodokumentit ovat vain DS1R:lle; DS3/ER/Sekiro-sivuilla pelkkä NetworkParam-lista (haku).
- BBSC: pelipakettien sisältö dokumentoimaton ("blocks any approach that edits or replays game state") [VAR U13].
- Souls Modding Wikin Sekiro-sivusto (SekiroResurrection/modding-wiki) sisältää vain README:n ja kuvia, ei RE-dataa.
- Lance McDonaldin twiitti (käyttämätön vihollisen ohjausominaisuus ei replikoitunut muille) ei auennut (HTTP 402); hakutiivistelmän mukaan peli ei replikoinut vihollisen ohjausta [ARV, toissijainen lähde].

## 8. Ehdotukset Sekiro-modin korjaamiseksi (tärkeysjärjestys)

1. **Etsi lohkon `+0x30` health-readback ja liput 0x10/0x20, ja aja HP/kuolema pelin omalla polulla** (kohta 0, 3). Dumppaa host-puolella osoitin ja `flags[i]` ennen/jälkeen osuman; jos 0x20 asettuu hostin damage-polussa, lue `{hp, damage_taken}` suoraan sieltä (ei oma HP-kysely) ja kirjoita clientille `{hp, damage_taken}` + 0x10, kuten movement/behavior. Tämä korvaa decrement-only-kirjoituksen ja kuoleman pakottamisen. [ARV, vahva: rakenne täsmää ER:ään kenttä kentältä]
2. **Kuolema reliable-relayna 2 Hz idempotenttina listana** (BBSC mod 10): `dead(netId)`-lista isännältä joka 500 ms + liittymissnapshotissa; client pakottaa natiivin kuoleman vain jos oma kopio on elossa, ja nollaa/asettaa kuolemaliput pelin polun kautta eikä pelkällä HP:llä (kohta 3). Älä jätä pomoja ja minibosseja pois (Sekiro Online -ongelma). [VAR vertailutapaukset; toteutus ARV]
3. **Selvitä `0x9F33F0(chr)` ja ER-lippuvastineet** (`is_left_behind`, `distance_based_network_update_authority`, `host_inactive`, `network_authority`): dekompiloi, logita sen lähdekenttä engaged-vihollisille ja pidä se arvossa, jossa `forceLocal=0`. Lisäksi kokeile natiivia EMEVD `SetNetworkUpdateRate(entity, true, AlwaysUpdate)` ja `SetNetworkUpdateAuthority(Forced=4095)` engaged-vihollisille (kohta 1, 2). [ARV; ER-kentät VAR]
4. **Haamupelaajalle absoluuttinen tilapaketti 10-15 Hz** (DSR 10 ruutua, replay 15 Hz): sijainti + kierto + hkb-tilaero + muuttujalohko (MoveSpeedLevel/MoveAngle `ReplayManipulator+0x10`-vektorin kautta); respawnin jälkeen ajoitettu "partner revive" -tyyppinen liputuksen nollaus (BBSC) (kohta 4). [VAR vertailut; toteutus OMA/ARV]
5. **Diagnostiikkadumppi ChrIns-lippulohkosta ja ChrCtrl:stä**: diffaa `chr+0x1A10..0x1A20` (elävä / kuollut / kaukana / engaged), etsi `death_flag` (bitti 7 @ +0x1A11?), `NetChrSyncFlags` ja `is_left_behind`; etsi ChrCtrl:stä `position_sync_requested` (ER ChrCtrl+0xFC) ja skannaa PlayerIns `+0x2008..+0x2030` NetAI/Network-manipulaattoria varten (vtable slot 2 palauttaa 2 tai 4) (kohdat 1, 2, 5). [ARV, edullinen ja ratkaisee 1 ja 3:n epävarmuuden]

Lisensseistä: fromsoftware-rs (MIT OR Apache-2.0), ds3os (MIT) vapaasti käytettävissä attribuutiolla; sekiro-coop (AGPL) ja BBSC (GPL) vain ideat.

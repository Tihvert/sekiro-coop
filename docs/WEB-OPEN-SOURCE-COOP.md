# Avoin lähdekoodi: FromSoftware-co-op-modit ja mitä niistä voi oppia Sekiro-modille

Tutkittu 2026-10-23 verkkohaulla (WebSearch + WebFetch, GitHub raw/API). Mitään ei asennettu eikä ladattu.
Merkinnät: **[VAR]** = luettu suoraan lähteestä (URL annettu). **[ARV]** = oma päätelmä/arvaus, ei varmistettu.
Huom: WebFetch tiivistää sivun pienellä mallilla, joten offsetit on laskettu/poimittu tiivistelmistä. Tarkista kriittiset luvut avaamalla lähdetiedosto itse ennen kuin rakennat niiden varaan.

## 1. Lyhyt vastaus

- Bloodborne Seamless Co-op (BBSC) on AVOINTA lähdekoodia (GPL-2.0-or-later), mutta se on shadPS4-emulaattorimodi. Se EI toteuta omaa vihollis- tai animaatiosynkkaa, vaan pitää pelin OMAN verkkokoodin istunnon hengissä. Hyöty Sekirolle on pieni ja epäsuora.
- Kaikki isot "seamless"-modit (ER, DS3, DS2, Sekiro Online, Nightreign) ajavat pelin natiivia verkkokoodia (host-auktoriteetti, natiivit sync-moduulit) ja estävät vain istunnon katkaisun. Yksikään ei julkaise omaa vihollissynkkaa avoimena.
- Hyödyllisin avoin aineisto on `fromsoftware-rs` (MIT OR Apache-2.0): Elden Ringin struct-määritykset sisältävät `ManipulatorType`-enumin (NetAi=4), `NetChrSync`-puskurit, `EnemyIns.net_ai_manipulator` ja ChrIns-verkkokentät. Sekiro-cratesta ChrIns/WorldChrMan puuttuu, mutta sama enginesuku -> hyvä vihjelähde.

## 2. Lisenssi- ja saatavuustaulukko

| Projekti | Lähdekoodi julkaistu? | Lisenssi | Huomio | Lähde |
|---|---|---|---|---|
| BBSC (Bloodborne Seamless Co-op, CLSAMG22) | KYLLÄ, koko repo (emulaattorifork, mod-DLL, launcher, Python-pelipalvelin) | GPL-2.0-or-later [VAR] | shadPS4/shadp2p-pohjainen; 0.0.2 sisältää `BBSC-0.0.2-source.zip`. Kopioitu koodi tekisi omasta modista GPL:n alaisen. | https://github.com/CLSAMG22/BBSC , https://github.com/CLSAMG22/BBSC/blob/main/NOTICE.md |
| Elden Ring Seamless Co-op (LukeYui) | EI. Repo `EldenRingSeamlessCoopRelease` on release-repo | ei lisenssitiedostoa tiedossa [VAR: ei löytynyt] | Vain zip-julkaisut; Rust-uudelleentoteutus `unseamless-coop` toteaa, ettei ole johdettu alkuperäisestä | https://github.com/micthiesen/unseamless-coop |
| DS3 Seamless Co-op (Yui) | EI, release-repo (peili) | ei lisenssiä [VAR: ei löytynyt] | Repo vain julkaisujakelua | https://github.com/LukeYui/DarkSouls3SeamlessCoopRelease |
| DS2 Seamless Co-op (Yui) | EI, repossa vain BLOCKSYNC, README, VERSION | ei lisenssiä | BLOCKSYNC = versiotarkistus + ban-lista | https://github.com/yuiamoroll/DarkSouls2SeamlessCoopRelease |
| Nightreign Seamless Co-op (Yui) | EI löytynyt | tuntematon | Patreon-päivitys: "feature-complete" 02/2026 | https://www.patreon.com/yuiamoroll/posts/update-general-150100850 |
| Sekiro Online (Yui) | EI. Repossa `DATA/`, LICENSE, README, VERSION; ei lähdekoodia näkyvissä | MIT (repon LICENSE) [VAR], mutta koodia ei ole | Yui sanoo vanhan modin "aged poorly", uusi seamless-uudelleenkirjoitus kesken; ei julkaistu | https://github.com/LukeYui/Sekiro-Online , https://www.patreon.com/yuiamoroll/posts/update-current-129620429 |
| unseamless-coop (micthiesen) | KYLLÄ | MIT | Rust, fromsoftware-rs; README: "Not a working co-op replacement yet", pelisessiokerros (pelaajien sijoitus toistensa maailmaan) vielä RE:n alla. Ei vihollis-/animaatiosynkkaa. | https://github.com/micthiesen/unseamless-coop |
| sekiro-coop (mstampfli) | KYLLÄ | AGPL-3.0-or-later (!) | Rust + me3 + UDP, Sekiro v1.06. Ei pelattava; kaukopelaaja ei renderöidy. AGPL: älä kopioi koodia. | https://github.com/mstampfli/sekiro-coop |
| fromsoftware-rs (vswarte) | KYLLÄ | MIT OR Apache-2.0 (Cargo.toml) [VAR] | Rust-bindingit ER/DS3/Nightreign/Sekiro; paras struct-lähde | https://github.com/vswarte/fromsoftware-rs , https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/Cargo.toml |
| ds3os (Tim Leonard) | KYLLÄ | MIT | Palvelinemulaattori DS2/DS3; protobuf-määritykset. Ei pelin sisäistä P2P-synkkaa. | https://github.com/TLeonardUK/ds3os |
| ds3-open-re (garyttierney) | KYLLÄ | ei tarkistettu | proto-määritykset, CE-taulukot, Rust-palvelinemulaattori | https://github.com/garyttierney/ds3-open-re |
| scheissgeist/Seamless, Restezzz/Seamless-DarkSouls2 | KYLLÄ | MIT | DS2-co-op ds3os-forkin + dinput8-modin päälle; hookaa protobuf-kerrosta ja pudottaa istunnon katkaisevat viestit | https://github.com/scheissgeist/Seamless , https://github.com/Restezzz/Seamless-DarkSouls2 |
| ERSkeletonMan (Dasaav) | KYLLÄ | ei tarkistettu | C++, PlayerIns/EnemyIns-vtablejen hookkaus RTTI:llä (apuri, ei verkkoa) | https://github.com/Dasaav-dsv/ERSkeletonMan |
| elden-ring-open-re (garyttierney) | KYLLÄ mutta README pieni | - | Vain regulation-tiedosto; ei verkkoa | https://github.com/garyttierney/elden-ring-open-re |

Päätelmä [VAR/ARV]: LukeYuin repot ovat release-repoja ilman koodia (ER, DS3, DS2, Sekiro Online). "Open source" -väitteet sivustoilla (esim. thegamercodex) ovat harhaanjohtavia.

## 3. BBSC tarkemmin (kysytty erikseen)

**Avoin? KYLLÄ.** GPL-2.0-or-later, 182 tähteä, 2 committia, julkaisu v0.0.2 (21.10.). Docs: `docs/ARCHITECTURE.md`, `NETWORK.md`, `WORLD-SYNC.md`, `RESEARCH-NOTES.md`, `SESSION-LIFECYCLE.md`, `KNOWN-LIMITATIONS.md`, `CHANGELOG.md` (173 kB). Lähteet: https://github.com/CLSAMG22/BBSC/tree/main/docs

Mitä BBSC oikeasti tekee [VAR]:
- Emulaattori (shadp2p-fork) emuloi PSN:n NP/Matching2-kutsut; peli ajaa omaa `CSMultiPlayMan`/`CSMultiPlayerIns`-logiikkaansa. Pelin pelipaketit kulkevat UDP-sokettia (4 tavun vport-header + payload) suoraan vertaiselle, ilman uudelleenlähetystä/järjestystä. Pakettien sisältö on DOKUMENTOIMATON (BBSC itsekin: "gameplay packet format undocumented, blocks any approach that edits or replays game state"). https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/NETWORK.md , https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/KNOWN-LIMITATIONS.md
- Pelaajan animaatiot/hyökkäykset/väistöt kulkevat natiivisti `SprjChrBehaviorSyncModule`-moduulin kautta (merkitty STRONGLY INDICATED, ei täysin varmistettu). https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/WORLD-SYNC.md
- Vihollisten olemassaolo ja kuolema synkkaantuvat natiivisti (CONFIRMED); sijainti/AI/aggro "ei yksitellen varmistettu"; auktoriteetti "probably host-authoritative" (HYPOTHESIS). Sama lähde.
- Live-havainto: kun vieras (guest) kuoli ja persistence piti istunnon, vieraan viholliset JÄIVÄT JÄÄHDYTETTYINÄ ja osumat eivät vaikuttaneet, koska vieras jäi client-tilaan (6) eikä isäntähahmoa luotu uudelleen -> "vihollissimulaatio on host-authoritative: client odottaa isännän tilaa" (STRONGLY INDICATED). https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/RESEARCH-NOTES.md
- CHANGELOG 2026-09-16: isännän kuoleman/respawnin jälkeen vihollisten vieraan tekemä vahinko "parani pois" -> kahdella koneella on erilliset maailmakopiot; lataus ei kanna toisen vahinkoa uuteen kopioon. Sama lähde (CHANGELOG, 100 kB lukematta).
- CHANGELOG 2026-10-04: kun molemmat pelaajat kuolivat bossitaistelussa, toinen näki toisen yhä elossa, koska kuolleen pelaajan kopio toisella koneella piti HP:n > 0 -> korjaus: erillinen relay-paketti, jossa "dead"-lippu. (Suoraan sinun ongelmasi 2 kaltainen.) https://raw.githubusercontent.com/CLSAMG22/BBSC/main/docs/CHANGELOG.md
- CHANGELOG 2026-09-30: bossin kuolemaskripti nollaa boss-HP-palkin ENNEN kuolinvaihetta, joten "onko boss-taistelu"-tarkistus ei näe sitä -> muista bossipalkki 10 s. Sama lähde.

Osoitteet ovat Bloodborne CUSA03173 v1.09:lle, eivät hyödytä Sekiroa. Offset-rakenne (WorldChrMan +0x278 count, +0x280 entries, stride 0x18) on Bloodbornen oma [VAR], ei Sekiron.

## 4. Muut hyödylliset avoimet lähteet

### 4.1 fromsoftware-rs / Elden Ring -structit (MIT OR Apache-2.0) - tärkein löytö
Kaikki alla [VAR] (luettu raw-tiedostoista, offsetit osin laskettu `repr(C)`-järjestyksestä, ei tiedoston kommenteista, joten tarkista):

- `ManipulatorType` (`chr_manipulator.rs`): Default=0, Pad=1, **Network=2, Replay=3, NetAi=4**, Com=5, Ride=6, Follow=7. `ChrManipulator`-vtable: slot 0 runtime_class, 1 destructor, 2 `manipulator_type()`. Struct 0xC0 tavua, `owning_chr` +0xA8, `network_warp_distance` +0xA0 (TODO: fact-check). https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/chr_manipulator.rs
- `EnemyIns` (ER): `com_manipulator` +0x580, **`net_ai_manipulator` +0x588**, `ride_manipulator` +0x590, `npc_think_param` +0x5A0. `PlayerIns`: `chr_manipulator` +0x588. `ChrCtrl.manipulator` +0x18. https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/chr_ins.rs
- `ChrIns.net_position_synchronized` (+0x364, ER): doc-kommentti "set by NetAIManipulator after receiving a position update" [VAR, tiivistelmästä]. -> NetAI-manipulaattori itse asettaa lipun kun sijaintipäivitys on vastaanotettu.
- `ChrIns.network_authority` u32 +0x1E4 (arvoja ei dokumentoitu), `p2p_entity_handle` +0x70 (8 B; FieldInsHandle jossa block_id ja selector vaihdettu; paketeissa 4 B block_id + 3 B chr_selector; Sekiron debug-assertit kutsuvat pakattua muotoa "whoid"), `last_received_packet60` +0x388.
- `NetChrSyncFlags` (u8, +0x1C9): bitti 3 `replay_recorder_enabled` ("main player character"), **bitti 5 `distance_based_network_update_authority`** (merkki: auktoriteetti voi siirtyä etäisyyden mukaan; mahdollinen syy miksi isäntä "menettää" vihollisen).
- `death_flag` = ChrIns +0x1C5 bitti 7 ("Controls whether the character is dead or not"); `is_invincible` bitti 4; `enable_render` bitti 3. Hyödyllinen kuoleman pakottamiseen/lukemiseen ER:ssä [VAR]; Sekirossa offset on eri/tuntematon [ARV: sama suunnittelu].
- `NetChrSync` (0x630): `chr_slot_count`, 196 kpl `NetChrSetSync`-osoittimia (yksi per ChrSet). `NetChrSetSync` (0x50): puskurit `placement_readback_values`, `health_readback_values`, `update_flags`; P2P-päivitystaskit täyttävät ne ja asettavat vastaanottolipun. `ChrSyncUpdateFlags` bitti 0 = placement-päivitys odottaa, bitti 4 = health-päivitys odottaa. `ChrSyncHealthUpdate` {current_hp i32, damage_taken u32}; `ChrSyncPlacementUpdate` {position, rotation (rad), + dynaamisen geometrian kenttiä}. https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/net_chr_sync.rs
- `CSSessionManager`: `LobbyState` Host=3, Client=6; `ProtocolState` Ingame=6; `SessionManagerPlayerEntry.disable_emk_sync` = isäntä ei lähetä EMEVD-sync-paketteja (104) pelaajalle; paketti 90 = multiplay-alueen warp-data. https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/session_manager.rs
- `NetworkSessionVmt`: `broadcast_packet`, `send_hit` (0x120-rakenne), `receive_latest_sp_effect_initialize_packet` (paketti 60), `..._sp_effect_sync_data` (paketti 61). https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/network_session.rs
- `CSChrBehaviorModule` (ER, 0x19C0): `owner` +0x8, `root_motion` +0x30, `animation_speed` +0x17C8, `ground_touch_state` +0x1680 (IK). Ei animaatio-ID-kenttää dokumentoitu. `CSChrActionRequestModule` (0x140): `npc_action_id` +0xF4 (NPC EzState-action, -1 kun ei asetettu), `queue_tae_id_override` +0x134. https://raw.githubusercontent.com/vswarte/fromsoftware-rs/main/crates/eldenring/src/cs/chr_ins/module/action_request.rs
- Sekiro-crate (`crates/sekiro/src/sprj/`) sisältää vain event_flag, game_data_man, world_info, param, item, task ym. - EI chr_ins/world_chr_man. https://api.github.com/repos/vswarte/fromsoftware-rs/contents/crates/eldenring/src/cs

### 4.2 Muut
- DS2-modit (scheissgeist, Restezzz; MIT): toimintaperiaate = isännän oma ds3os-palvelin + hookataan pelin protobuf-serialisointi ja pudotetaan viestit jotka päättäisivät co-opin (bossikuolema, kuolema, aluevaihto). README-tiedostot listaavat tunnetut viat (alla kohta 5). Dokumentit `DS2_CUSTOM_SERVER_TECHNICAL_BREAKDOWN.md`, `PROJECT_NORTH_STAR.md` lukematta.
- Tim Leonardin blogisarja "Reverse Engineering Dark Souls 3 Networking" (osat 1-7): kattaa palvelinyhteyden/matchmakingin; osa 7 toteaa vain, että pelaajien välinen P2P kulkee Steam-verkon yli. EI pelisisäisen synkan (vihollinen/animaatio) kuvausta. https://timleonard.uk/2022/06/20/reverse-engineering-dark-souls-3-networking-part-7
- sekiro-coop (mstampfli, AGPL) HANDOFF: host-auktoriteetti vihollisille; vihollistila 5 Hz isännältä + 64-bittinen digest-suodatin (~97 % ohitus idle-ruuduilla); vihollisrekisteri `handle -> ChrIns*` rakennetaan ApplyEffect-hookin `entity`-argumentista (WorldChrMan-skannaus kaatoi peliä stale-osoittimiin); HP kirjoitetaan ketjulla ChrIns+0x1FF8 -> +0x18 -> +0x130, vain vähennys (decrement-only). https://raw.githubusercontent.com/mstampfli/sekiro-coop/HEAD/docs/HANDOFF.md
- sekiro-coop: raaka sijaintikirjoitus ChrIns-tilamoduuliketjuun "onnistuu" (arvo säilyy) mutta renderöinti ja fysiikka ignooraavat sen -> tarvitaan natiivi warp-funktio. Sekirossa ei ole phantom-slottia; jokainen "puppet" on NPC-kaappaus (AI pois, siirto, animointi). Animaation ajamiseen tarvittavilla funktioilla ei ollut tunnettuja AOB:eja. Sama lähde.

## 5. Tunnetut sudenkuopat muiden projekteista

1. DS2-co-op 0.3.0 (Restezzz) [VAR]: lepäämisen jälkeen vihollinen voi kuolla paikalleen molemmille samalla kun seuraava spawnaa elävänä; hyökkäys voi osua isännälle ja ohi vieraalla; vihollisen liike nykii; bossi ei herää jos vieras tulee ensin. -> samat oireet kuin sinulla (kuolema-/HP-ajelehtiminen, liikkeen nykiminen). https://github.com/Restezzz/Seamless-DarkSouls2
2. ER Seamless Co-op [VAR, foorumilähde, ei koodia]: vihollisten desynkka ratkaistaan resetoimalla maailma kun yksi lepää armolla (kaikille yhtä aikaa). Eli Yuikin pakottaa uuden tilan sen sijaan että korjaisi jatkuvasti. https://www.nexusmods.com/eldenring/mods/510 (haussa; tarkkaa kohtaa ei avattu)
3. BBSC: kuolevan pelaajan kopio toisella koneella voi pitää HP:n > 0 -> tarvitaan erillinen "dead"-lippu relay-paketissa (ks. kohta 3).
4. sekiro-coop: bincode-enum-tagit rikkovat wire-yhteensopivuuden jos PacketBody-varianttia lisätään; edge-triggered-liput (älä kirjoita joka ruudulla); aloita ChrIns-lukeminen vasta kun IGT on edennyt N ruutua peräkkäin, muuten menu-vaiheen kaatuminen.

## 6. Konkreettiset vinkit Sekiro-modille

Kaikki alla pohjautuvat yllä oleviin lähteisiin; merkitty luottamustaso.

1. **Hyödynnä ER-struktuurit Sekiro-RE:n karttana.** Etsi Sekiron EnemyIns:stä `net_ai_manipulator`-osoitin ER-vastineen perusteella (ER: EnemyIns+0x588, heti `com_manipulator`in jälkeen) ja `ManipulatorType`-arvot (NetAi=4, Network=2, Replay=3). Tarkista Sekirossa vtable slot 2 `manipulator_type()` -- jos palauttaa 4, osoitin on oikea. [VAR ER:lle; Sekiro-offset ARV, sama enginesuku]. Lähde: chr_manipulator.rs / chr_ins.rs (URLit kohdassa 4.1).
2. **Selitys "NetAI aktiivinen vain ~40 % ruuduista" voi löytyä `net_position_synchronized`-lipusta + `distance_based_network_update_authority`-bitistä.** ER-doc: NetAI asettaa position_synchronized-lipun saatuaan sijaintipäivityksen; NetChrSyncFlags-bitti 5 sallii etäisyyteen perustuvan auktoriteetin. Lokita Sekirossa molemmat (ChrIns+0x364 ER:ssä; NetChrSyncFlags ER:ssä ChrIns+0x1C9) ja katso korreloivatko ne 40 %:n ruutuihin. Jos pelin oma logiikka vaihtaa manipulaattorin takaisin (esim. kun vihollinen on "left behind": `ChrInsActivationFlags` bitti 0 `is_left_behind` = liian kaukana muista pelaajista), pidä ohjaus uudelleenasettamalla vtable/osoitin joka ruutu TAI estä ko. logiikka. [ARV, vahva johtolanka; ER-kentät VAR]
3. **Käytä NetChrSync-puskureita sen sijaan että ajat omaa HP-kanavaa.** ER:ssä peli täyttää per-ChrSet puskurit (`placement_readback_values`, `health_readback_values`) ja lipun (`update_flags` bitit 0 ja 4), ja lukee ne seuraavassa tikissä. Jos Sekirossa on sama rakenne (NetChrSync-singleton), voit syöttää vastaanotetun {current_hp, damage_taken}-parin ja liput -> peli soveltaa HP:n ja kuoleman omalla polullaan (death_flag, animaatio, drop) eikä tilat ajaudu eri suuntiin. [ARV että Sekirossa; rakenne VAR ER:ssä]
4. **Kuolema = erillinen luotettava (reliable) tapahtuma, ei pelkkä HP.** BBSC:n korjaus (dead-lippu relay-paketissa, koska toisen koneen kopion HP pysyi > 0) ja DS2-modin "vihollinen kuolee vain toisella" -oire viittaavat samaan: HP-snapshot ei riitä. Lähetä `died(enemy_id, tick)` reliable-kanavalla isännältä ja pakota clientillä natiivi kuolema (death_flag tai natiivi damage-polku), älä HP=0-kirjoitus. Lisäksi 5 Hz idempotentti "tilatarkistus" (host: alive/dead per entity) korjaa pudonneet viestit. [VAR vertailutapaukset; toteutusohje ARV]
5. **Host-auktoriteetti vahvistettu kaikissa muissa.** BBSC (client odottaa isännän tilaa, viholliset jäätyivät kun client ei saanut isännän dataa), sekiro-coop (host owns enemies, ignoroi clientin väitteet). Pidä sääntö: client EI koskaan kirjoita vihollisen sijaintia/tilaa; client lähettää vain osumat/vahinkotapahtumat isännälle. [VAR]
6. **Rekisteri hookeista, ei WorldChrMan-skannauksesta.** sekiro-coop kaatui stale-osoittimiin skannatessaan WorldChrMania; rekisteri ApplyEffect-hookin `entity`-argumentista oli luotettava (42 entiteettiä). Jos sinulla on jo vakaa vihollislistaus, säilytä; muuten käytä hookkipohjaista rekisteriä ja siivoa despawn-osoittimet (sekiro-coop jätti sen tekemättä, "P2"). [VAR]
7. **Delta-suodatin vihollistilalle.** 64-bittinen digest per entiteetti, lähetä vain muuttuneet; 5 Hz riitti heidän testeissään (~97 % ohitus idle-ruuduilla). Sinun 40 % NetAI-ongelmasi on eri asia (ohjauksen jatkuvuus), mutta digest vähentää kaistaa ja jättää tilaa sijaintitiheydelle liikkuvilla vihollisilla. Huom: 5 Hz ilman interpolointia näyttää nykivältä (vrt. DS2-modin "jerky enemy movement"); interpoloi/ekstrapoloi clientillä. [VAR luvut; interpolointi ARV]
8. **Haamuhahmon animaatio: kaksi toimivaa mallia, kumpikaan ei ole avoimena Sekirolle.** (a) Natiivi: ER/BB ajavat pelaajan animaatiot `Replay`/`Network`-manipulaattorin ja behavior-synkkamoduulin kautta (BBSC: `SprjChrBehaviorSyncModule` [STRONGLY INDICATED]). (b) NPC-kaappaus (sekiro-coop): AI pois, siirto natiivilla warpilla, animaatio natiivilla funktiolla; raaka kenttäkirjoitus ei renderöidy. Sinun oire (juoksun alku toimii, muuten liukuu) viittaa siihen, että lähetät vain animaatio-ID:n kerran; lähetä ID + TAE-aika/frame + nopeus + niitä seuraava "uusi animaatio" -tapahtuma uudelleen kun ID vaihtuu, ja aja `npc_action_id`/EzState (ER: CSChrActionRequestModule+0xF4) tai `Replay`-manipulaattorin kautta. [ARV; kentät ER:stä]
9. **Älä kopioi AGPL/GPL-koodia.** sekiro-coop on AGPL-3.0 ja BBSC GPL-2.0+; lue ideat, kirjoita itse. fromsoftware-rs (MIT/Apache) ja ds3os (MIT) ovat vapaasti käytettävissä (huomioi attribuutiovaatimus). [VAR lisenssit]
10. **Wire-yhteensopivuus ja alku.** Versioi paketit ja rakenna molemmat päät aina yhdessä (bincode-tag-sudenkuoppa); lue ChrIns/vihollislistaa vasta kun IGT on edennyt useita ruutuja (menu-vaiheen kaatumiset). [VAR sekiro-coop]

## 7. Mitä EI löytynyt (negatiiviset tulokset)

- Ei yhtään avointa dokumenttia, joka selittäisi miksi NetAI-manipulaattori "putoaa pois" tai vaihtuu takaisin ("+0x58"-polkua ei mainittu missään). Lähin johtolanka on kohta 6.2 [ARV].
- Ei avointa koodia tai kuvausta Yuin vihollis-/animaatiosynkasta missään pelissä; Souls Modding Wiki ei sisällä manipulaattoriluokkia (haettu nimillä ChrNetworkManipulator / CSChrNetAiManipulator).
- Ei avointa Sekiro-ChrIns/WorldChrMan-structia fromsoftware-rs:ssä; mstampfli-projektissa ChrInsLayout oli "unresolved" ja sijainti/animaatio-offsetit puuttuvat.
- Cheat Engine -taulukoita, Discord-arkistoja (?ServerName?) ei tavoitettu hakukoneella; ei liitytty Discordeihin.
- Lukematta jäi: BBSC CHANGELOG (loput ~72 kB), BBSC `emulator/src/core/bloodborne_re.cpp` (849 kB) ja `emulator/mod/bbsc_*.cpp`, DS2-modien tekniset dokumentit, unseamless-coop `docs/ARCHITECTURE.md`, sekiro-coop Rust-lähteet (`worldchrman.rs`, `chrins.rs`, `offsets.rs`).

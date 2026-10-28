# RE: Elden Ring Seamless Co-op (ERSC v2.0.1, LukeYui), opittavaa Sekiro-modiin (2026-10-23)

Staattinen analyysi, oppimistarkoitus. Modia, launcheria tai peliä ei ajettu, suojausta ei purettu.
Kohde: `F:\Temp\re_ersc\extract\` (ersc_launcher.exe, SeamlessCoop\ersc.dll, ersc_settings.ini, locale\english.json, crashpad).
IDA 9.3 -kanta `F:\Temp\re_ersc\ersc.i64`. Ajot: `F:\Temp\re_ersc\o1` (merkkijonojen xrefit + dekompilaatit) ja `o2\all.c` (kaikki 2742 funktiota alueelta .text < RVA 0x100000).
Osoitteet ovat ersc.dll:n RVA:ita (IDA-osoite − 0x180000000). Kuvaukset ovat omin sanoin, koodia ei ole kopioitu.

Merkinnät: **[VAR]** = luettu binääristä tai tiedostoista, **[WEB]** = julkinen lähde (URL), **[ARV]** = päätelmä.

## 0. Lyhyesti

- **Suojaus:** Themida/WinLicense (Oreans). Pakkaus koskee vain entry pointia, importteja ja osaa funktioista. `.text` on selväkielistä ja dekompiloituu, joten modin logiikka on suurimmaksi osaksi luettavissa. [VAR]
- **Istunto:** ERSC käyttää **pelin omaa sessiota** (`CSSessionManager`, pelin oma Steam-P2P `DLNW3D::SteamConnectionManager`) pelaajien ja vihollisten synkkaan. Matchmaking korvataan salasanalla avainnetulla **Steam-lobbylla**, ja modin omat viestit kulkevat erillisellä **ISteamNetworkingMessages**-kanavalla. [VAR]
- **Seamless-kikka:** EMEVD:n ehtotestien (`CS::EcTest*`) vtablet hookataan. Tärkein on `EcTestMultiplayState`: valituille eventeille se vastaa "yksinpeli". Näin sumuseinät, "lähetä kotiin" ja moninpelirajoitukset jäävät pois päältä. [VAR mekanismi, ARV vaikutus]
- **Kuolema ja lepo:** kuollut herää omalla gracella ja istunto jatkuu. Bossissa kuollut siirtyy katsojaksi. Lepo resetoi maailman kaikille ja potkaisee boss-areenalla olevat ulos. Kuolemasta saa pinoutuvan "Rot"-debuffin. [VAR + WEB]
- **Skaalaus:** paramitauluihin luodaan latauksessa **ajonaikaisia SpEffect-rivejä** pelaajamäärää kohden, rot-debuffit mukaan lukien. [VAR]

## 1. Suojaus ja rakenne

### 1.1 ersc.dll [VAR]
- Osiot: normaalit MSVC-osiot (`.text` 1,6 MB, entropia 6,7, `.rdata`, `.data`, `.pdata`, `.gxfg`, `.retplne`). Niiden perässä ovat suojaimen lisäämät `.edata`, `.idata`, kaksi `.tls`-osiota ja **`ERSC`**: 11 MB, RWX, entropia 6,55. Lopussa on vielä 16-tavuinen `.reloc`.
- **Entry point on `ERSC`-osiossa** (0x29FD7C).
- Import-taulussa on yksi funktio per DLL: GetModuleHandleA, GetAsyncKeyState, SteamAPI_GetHSteamPipe, WSACleanup, BuildExplicitAccessWithNameW, CertAddCertificateContextToStore, WLDAP32 #301 ja IdnToAscii. Tämä on tyypillistä Themidan import-suojaukselle: oikea IAT rakennetaan ajossa.
- Exception-hakemisto on siirretty `ERSC`-osioon. DllCharacteristics on vain 0x20 (ei DYNAMIC_BASE), ja relokaatiot on riisuttu.
- Tunnisteet `ERSC`-osiossa: `WinLicenseInstance`, `WLProtectionDateTime`, `Software\WinLicense`, `/checkprotection`, Oreansin virheteksti ("Please, contact the software developers…") ja `IL86.pdb`. Suojain on siis **Themida/WinLicense 3.x** (custom-nimetty osio). Build-aika on "Wed Sep 2 23:55:02 2026".
- Ainoa export on `modengine_ext_init` (ModEngine 2 -extension-tuki).
- Virtualisointia on vähän. `.text`-osiosta on vain 9 suoraa kutsua `ERSC`-osioon. Muutama alustusfunktio on kuitenkin virtualisoitu tai mutatoitu: signature-listat, RTTI-hakijat, ini-avainten käsittely ja viestien vastaanottodispatch. Niitä ei löydy luettavasta koodista. [VAR/ARV]
- Merkkijonosuojaus on kevyt. Osa merkkijonoista on xorstr-tyylisiä vakioita, jotka XOR:taan ajossa: ini-avaimet ja URL:t (asetuslataaja 0x9E300 tekee näin). Lokalisaatioavaimet, lähdetiedostopolut ja singletonien nimet ovat selväkielisiä. [VAR]
- Staattisesti linkatut kirjastot: libcurl (TLS schannel, versiotarkistus/lataukset [ARV]), zlib 1.3.1, crashpad-client, nlohmann::json 3.12 ja std::format. Mukana on myös AES-tyylinen lohkosalaus ("Plaintext length must be divisible by") ja yksi base64-avainta muistuttava vakio. [VAR]

### 1.2 Lähdekoodin moduulit (assert-polut, `.rdata` ~0x1DF188) [VAR]
`ersc\buddy`, `mod\voice_chat`, `hooks`, `signatures`, `mod\preferences`, `game_memory_unlimiter`, `seamless_session_manager`, `cs\session_manager`, `mod\cheat_detection_spider`, `cs\menu_man`, `cs\lua_event_man`, `cs\lock_tgt_man`, `cs\world_chr_man`, `cs\map_item_man`, `cs\event_flag_man`, `cs\game_man`, `cs\fe_man`, `cs\game_data_man`, `cs\emk_system`, `param`, `networking`, `spectate`, `battle_royale`, `yui_nexus_3`.
Arkkitehtuuri: jokaiselle pelin managerille on oma C++-kääre (`cs\*`), ja niiden päällä ovat modin palvelut (session manager, spectate, buddy, battle royale). Verkkokerros on nimeltään **YuiKeyNexus3** (YKNX3), boss rush ja PvP-areenat **YuiKeyBattleRoyale2** (YKBR2).

### 1.3 Pelin rajapinnan löytäminen [VAR]
- **Singletonit nimellä:** `CSSessionManager`, `CSMenuMan`, `CSLockTgtMan`, `CSMapItemMan`, `CSEventFlagMan`, `CSGameMan`, `CSFeMan`, `GameDataMan`, `CSRegulationManager`, `SoloParamRepository`, `SteamSocketManager`. Jokainen kääre pitää osoittimen-osoittimen singletoniin, ja jokainen käyttö tarkistaa null-arvon ja kaatuu assertiin nimen kanssa. [ARV: ER:n FD4-singleton-rekisteri nimihaulla]
- **Vtablet RTTI-nimellä:** `.rdata` 0x1E0C51… listaa pelin luokkia mangled-nimillä. Näillä ei ole omia TypeDescriptoreita, joten ne ovat hakuavaimia pelin RTTI:hin: `CS::ChrSet`, `DelayGaugeViewParts`, `PlayerIns`, `WorldBlockRes`, `EcTestIsMyMapOwner`, `CSVoiceChatManager`, `TpfFileCap`, `TpfbndFileCap`, `EcTestMultiplayState`, `EcTestChrType`, `EcTestDistance`, `EcTestIsMyWorld`, `EcTestChrDead`, `DLNW3D::MTInternalThreadSteamSocketManager`, `DLNW3D::SteamConnectionManager`, `DLNR3D::VoiceChatMemberRefInfo` ja `DLNR3D::VoiceChatSteam`. Viittaavaa koodia ei löydy luettavasta osasta, joten hakija on virtualisoitu. [VAR/ARV]
- **AOB-skanneri** (0xD6E80): tavu- ja maskikuvio, harvinaisin tavu ankkurina (frekvenssitaulu) ja SIMD-haku. Kuviolle voi antaa **varakuvion**. Jos kumpaakaan ei löydy, tulee selvä virhe: "No such pattern … The mod may not be compatible with the installed game version". Kuviot itse eivät ole luettavassa muodossa. [VAR]
- `game_memory_unlimiter` ("Unable to write to game engine memory allocation table", `DLDynamicAllocator`) kasvattaa moottorin allokaattorien rajoja suurempaa pelaajamäärää varten. [VAR nimi, ARV tarkoitus]
- `CSCheatDetectionSpider::WhatIsPatchBytes` tunnistaa muistipatcheja (oma kevyt huijauksentunnistus). [VAR]

### 1.4 Launcher [VAR]
Clang/LLD-build (`C:\Users\Luke\source\repos\ersc\...ersc_launcher.pdb`). Se asettaa `SteamAppId=1245620` ja käynnistää `eldenring.exe`:n suoraan EAC-startterin ohi. Sitten se injektoi `SeamlessCoop//ersc.dll`:n klassisesti: VirtualAllocEx, WriteProcessMemory ja CreateRemoteThread(LoadLibraryA), sitten ResumeThread. Crashpad-handler (2023) ajetaan erillisenä prosessina, ja raportit kulkevat crashpadin omalla tietokannalla. Upload-URL on piilotettu. [VAR/ARV]

## 2. Istunto (kohta 1)

- **Löytäminen:** Steam-lobby (`SteamMatchMaking009`, "Steam lobby creation/search timed out", "No sessions found", "This world is marked as private and cannot be joined"). Salasana on lobbyn hakuavain. [VAR] Kaikkien on oltava samassa pelin ja modin versiossa ([WEB] https://ersc-docs.github.io/faq/). Kuulumattomilla salasanoilla voi tulla törmäyksiä ([WEB] sama).
- **Pelin oma sessio:** `OnBeJoinType (%u)`, `YKNX3: OnBeJoinStart_Wanderer` / `_EvilWanderer` (0x28150) ja `CSSessionManager+0x60 → +0x708` (SteamSocketManager). Mod kutsuu pelin omaa "be joined" -polkua (sama kuin hyväksytty kutsumerkki) ja antaa liittymistyypin itse. Pelaajat ovat siis pelin natiiveja phantom-PlayerInsejä pelin omalla P2P-siirrolla. [VAR kutsu, ARV tulkinta] Sama malli näkyy julkisessa uudelleenkirjoituksessa: host pitää lobbyn hostina, ja liittyjä saavuttaa Client-tilan pelin omalla P2P:llä ([WEB] https://github.com/micthiesen/unseamless-coop/releases).
- **Sessiojäsenet:** `CSSessionManager+0x78/+0x80` on 256-tavuisten jäsenrakenteiden vektori. Siinä `+0x10` on Steam-id, `+0xD0` remote-pelaajan GameData-slot (< 5, 0xAF0 B per slot GameDataManissa), `+0xE9` "paikallinen" ja `+0xEA/+0xEF` tilatavuja. 0x8F610 asettaa ne kaikille remoteille tietyssä tilassa. [ARV: pitää jäsenet "elossa" latausten yli]
- **Pysyvyys:** pelin omat "lähetä kotiin" -triggerit tulevat EMEVD:stä (bossin kaato, alueen raja, kuolema). ERSC kumoaa ne ehtotestihookeilla (§4.3). Host on maailman omistaja: `CSPartyMemberInfo` (`CSGameMan+0xD90`) `+0x174 == 2` = liittyjä. [VAR] Hostin Separation Mist lopettaa koko istunnon ([WEB] https://www.gamepressure.com/download/elden-ring-seamless-co-op-v174-mod/zf13deb).
- **Liittyjän spawn:** `SetPseudoMultiplayerSpawnPosition` (0x91580). Kun rooli on liittyjä, mod laskee pelin funktiolla spawn-pisteen hostin luo ja kirjoittaa sen `CSGameMan`-spawnikenttiin (`+0xAA0…`). Mukana on erikoiskorjaus yhdelle kartalle (`CheckLegacyRadahnSpotPosBug`, nollakoordinaatit). [VAR]
- **Modin oma kanava** (0xA7690): viesti lähetetään `SteamNetworkingIdentity{type SteamID, steamid}` -osoitteella ISteamNetworkingMessages-rajapinnan ensimmäisellä metodilla. Kanava valitaan viestiluokan mukaan, ja luotettava/epäluotettava on lippu. Ennen lähetystä viesti pakataan, myös salaus on mahdollinen [ARV]. Kokoraja tarkistetaan, ja viesti lähetetään vain jäsenille, joiden yhteystila on kunnossa (`+0x198 & 2/4`). Viestityyppejä: 17 = lipun asetus {u32 flag, u8 arvo}, 18 = lippupyyntö {u32}, 21 = buddy, 31 = oma kanava 5. [VAR]

## 3. Kuolema ja katsojatila (kohta 2)

- **Kuolema avoimessa maailmassa:** herätys omalla viimeisellä gracella, ja istunto jatkuu. [WEB] https://www.pcgamer.com/is-elden-ring-co-op-multiplayer/ ja gamepressure.
- **Bossissa tai invaasiossa kuollut** siirtyy katsojaksi, kunnes koko party kuolee tai joku lepää gracella. `always_spectate_on_death = 1` tekee katsojatilasta aina päällä olevan "until party wipeout or bonfire rest". [VAR ini + WEB]
- **Spectate-moduuli** (0xD7BB0, `spectate.cpp`): tila-avaimella (condition id) on taulu callbackeja. Oletuspolku käy jäsenlistan läpi ja valitsee ensimmäisen elossa olevan liitetyn pelaajan, jonka chr-osoitin vastaa sessiojäsentä. Viesti on "Spectating {}". Nuolinäppäimet vaihtavat kohdetta [WEB]. [VAR]
- **Bossitila tunnistetaan CSFeMan-bossipalkeista** (0x28960): FeMan `+0x5BF0…+0x5C30` sisältää kolme bossipalkin kahvaslottia, ja jokaiselle tarkistetaan "onko tämä kahva elävä chr". Tämä on sama signaali kuin meillä (RE-FINDINGS-2 §4). [VAR]
- **Kuolemadebuffit:** `death_debuffs` antaa kuolemasta satunnaisen Rot Essencen viidestä tyypistä. Ne pinoutuvat ja poistuvat vain gracella levätessä. Tarkoitus on estää, ettei yksi pelaaja voi kuolla jatkuvasti ilman haittaa, kun toinen pitää viholliset resetoimattomina. [WEB] pcgamer. Toteutus on param-tasolla (§6). [VAR]
- **Vihollisten reset:** kuolema ei resetoi muiden maailmaa. Reset tapahtuu vain levossa, ja silloin kaikille (§4.1). [WEB] Koko partyn wipe vie kaikki gracelle [ARV/WEB].
- `FE_PLAYERDEATH` ("DEATH"), `YKNX3_PLAYERDIED` ("{} has died") ja kuolemalaskuri nimikilvessä (`overhead_player_display = 4`, "Deaths: ") ovat UI:ta. [VAR]

## 4. Viholliset, bossit, sumuseinät ja maailma (kohta 3)

### 4.1 Lepo = maailman reset kaikille [VAR + WEB]
- Kun joku lepää, muut saavat ilmoituksen (`YKNX3_PLAYERBONFIREWARP`, `…FIRSTLVLUP`). Bossiareenalla oleva (FeMan-palkki aktiivinen) **siirretään areenan ulkopuolelle**. Siirto laskee sijaintideltan karttablokkien välillä `WorldMapLegacyConvParam`-muunnoksella ja lisää sen fysiikkasijaintiin. Ilmoitus on "The power of grace expelled you from the arena". (0x28710)
- Samassa funktiossa asetetaan **WorldChrMan `+0x1E524` = 1**, kun se on 0. [ARV] Tämä on ER:n vastine Sekiron `AllChrRespawn`-lipulle (WCM+0x67C): in-place-reset ilman latausta. WCM:n pääpelaaja on `+0x1E508`, mikä vastaa fromsoftware-rs:ää.
- "Resting at a Site of Grace resets the world state for all players … to prevent enemy desync" [WEB] gamepressure/pcgamer.

### 4.2 Viholliset ja skaalaus
- Viholliset käyttävät pelin **natiivia co-op-omistusta**, koska pelaajat ovat oikeita phantomeja pelin sessiossa. Erillistä vihollissynkkaa modissa ei näy. [ARV, vahva: ei omaa vihollisprotokollaa luettavassa koodissa]
- Skaalaus (ini, `[SCALING]`): enemy/boss × health/damage/posture (35/0/15 ja 100/0/20 % per pelaaja). Asetuslataaja jakaa arvot sadalla ja leikkaa ne ≥ 0:aan. [VAR]
- Param-hook (0x82950, `CSRegulationManager`-latauksen jälkeen): SpEffect-rivi 7800 kopioidaan pelaajamäärän verran uusiksi riveiksi neljään sarjaan (2147200000+n, 2147201000+n, 2147202000+n ja 2147203000+n). Sarjat ovat [ARV] vihollis- ja bossiskaalauksen SpEffectit per pelaajamäärä, ja ne tukevat yli 4 pelaajaa. Julkinen uudelleenkirjoitus kirjoittaa skaalauksen pelin omiin moninpelikorjausriveihin ([WEB] https://github.com/micthiesen/unseamless-coop/releases). [VAR rivien luonti, ARV kentät]
- Pelaajamäärä: alkuperäinen 2.0-beta väitti 127 pelaajaa (testattu 15:een asti) ([WEB] https://steamcommunity.com/app/1245620/discussions/0/3395163747104259416/). Vanhempi kuvaus mainitsee 3 phantomia ([WEB] gamepressure). Koodin taulukot on mitoitettu pelaajamäärämuuttujan (`repo+0x20`) mukaan. [VAR]
- Bossin palkinto kaikille: `GetBlockClearBonus` (0x871E0) hookkaa bossin kaatopalkkion. Pelaaja, jolle peli ei sitä antaisi, saa runepalkkion laskettuna paramista 29 (GameArea [ARV]) ja palkin "ENEMY FELLED" (FE 8380002). Tapahtuma lähetetään verkkoon nimellä. [VAR]
- `ERSCRuntimeEnemyGenerator` / `EnemyGeneratorCtrl` (0x298E0, 0x1E680) varaavat vapaita vihollisslotteja ajonaikaisille spawneille (boss rush / "enemy rush" -tilat). [VAR]

### 4.3 Sumuseinät, "lähetä kotiin" ja ehtotestit [VAR mekanismi]
- `emk_system` hookkaa `CS::EcTestMultiplayState`:n (0x8F300). Jos eventin id on modin listalla, testi vastaa kuin tila olisi **4** (= Singleplayer ER:n EMEDF-enumissa [WEB] https://soulsmods.github.io/emedf/er-emedf.html, arvo 4 on johdettu BB:n vastaavasta) ja muut tilat epätosi. Tilat 5–6 ovat aina epätosi. Listan ulkopuoliset eventit ajavat alkuperäisen testin.
- Hookattaviksi haetaan myös `EcTestIsMyWorld`, `EcTestIsMyMapOwner`, `EcTestChrType`, `EcTestChrDead` ja `EcTestDistance` (§1.3). [ARV] Näillä liittyjä hyväksytään maailman omistajaksi tavara- ja eventtiehdoissa, phantom-tyyppi pelaajaksi, ja kuolema- ja etäisyysehdot ulotetaan koko partyyn.
- Lopputulos [WEB]: kaikki moninpelin sumuseinät ja niiden teleportit ovat poissa. Bossin kaato ei lähetä kotiin. Torrent toimii kaikilla, ja karttamerkit synkkaavat.
- **Lippusynkka** (0x214E0, `YKEventFlagMan`): SetEventFlag-hook ohittaa, jos arvo ei muutu. Muuten se asettaa lipun paikallisesti (ja tarvittaessa toiseen bankkiin) ja lähettää tyypin 17 kaikille, ellei lippu ole poissulkulistalla (0x21B40). Erityislistalla ovat "kaikkien oltava paikalla" -liput: jos joku jäsen ei ole valmis, asetus estetään ja näytetään viesti (vrt. "All Wanderers must enter together" evergaolissa). Muuten lippu asetetaan ja lähetetään tyyppi 18. [VAR]
- Pelin edistyminen: vain ne edistykset, jotka tehdään hostin maailmassa, siirtyvät omaan maailmaan. Maailman tavarat ja bossipudotukset tulevat kaikille, elleivät ne ole jo omassa maailmassa. Vihollisten dropit ovat per pelaaja. [WEB] pcgamer.

## 5. Pelaajasynkka (kohta 4)

- Pelaajat ovat pelin natiiveja `PlayerIns`-phantomeja pelin omalla `NetChrSync`/P2P-synkalla. Mod ei synkkaa sijaintia tai animaatioita itse. [ARV, vahva]
- Phantom-väri: 2.0.1 korjasi "phantom colours", eli värejä hallitaan, todennäköisesti param- tai vtable-tasolla [WEB] (Nexus-changelogin tiivistelmä hakutuloksessa; sivu antoi 403). 2.0.0 lisäsi hevostyyppien näkymisen verkossa. [WEB]
- Aikarajat: kutsuaikaraja poistuu, koska EMEVD-ehdot näkevät yksinpelin [ARV].
- Esinekäyttö phantomina (0x82950):
  - EquipParamGoodsin (taulu 3) riveihin 130, 190 ja 2160 sytytetään bitit `+0x48 |= 0x1C` [ARV: käyttöoikeudet harmaa/valkoinen/musta-tilassa];
  - 253 muun esineen listassa asetetaan `+0x6F` bitti 0 ja nollataan `+0x4A` bitti 3 [ARV: "käytettävissä moninpelissä"].
  - Modin omat esineet (Tiny Great Pot, Effigy of Malenia, Separation Mist ja muut) ovat goodseja. [VAR]
- Buddy (spirit ash) moninpelissä: `allow_summons`, `buddy.cpp` 0x8A7D0 jakaa buddy-slotit pelaajille (65 per pelaaja). [VAR]
- Äänichat: pelin oma `CSVoiceChatManager`/`VoiceChatSteam` + modin audio-objekti per pelaaja (`YuiNexus3VoiceChatAudioInputObj_%d`, Wwise `Play_VoiceChat`/`Stop_VoiceChat`). [VAR]
- Estolista: "You have been blocked by the host…" ja Steam-id nimikilvessä. [VAR]

## 6. Asetukset ja säännöt (kohta 5) [VAR, ersc_settings.ini + english.json]

| Asetus | Arvo | Mekanismi |
|---|---|---|
| allow_invaders | 1 | oma invaasiohaku (Challenger's Lynchpin, "Seek opponent", Dried Fingers houkuttelee lisää) |
| death_debuffs | 1 | Rot Essence ×5 tyyppiä, pinoutuu, paranee levossa |
| allow_summons | 1 | spirit ashit moninpelissä |
| overhead_player_display | 0–5 | nimikilpi: ping / taso / kuolemat |
| append_steam_id_to_players | 0 | huijarin tunnistus |
| always_spectate_on_death | 0 | 0 = katsoja vain bossissa ja invaasiossa, 1 = aina wipeen tai lepoon asti |
| enemy_/boss_ health/damage/posture_scaling | 35/0/15, 100/0/20 | % per lisäpelaaja, SpEffect-rivit |
| cooppassword | – | Steam-lobbyn avain |
| save_file_extension | co2 | erillinen tallennus (ei FromSoftin palvelimia) |
| mod_language_override | – | locale-JSON |

- **Rot-debuffien toteutus** (0x82950): SpEffect-pohja 4270 kopioidaan 10 pinotasolle per tyyppi (rivit 2147100000+k, …+10+k, …+20+k jne.):
  - kestävyyden palautus −18 % per taso;
  - HP/FP/kestävyys −20 % per taso;
  - muut tyypit vastaavasti (puolustus, hyökkäys, runet ja löytöonni).
  Uudet rivit liitetään pelin paramitauluun latauksen jälkeen. Muut paramimuutokset: SpEffect 1420 muokataan, taulu 60 rivi 0 `+0x48 = 3` ja tauluun 144 lisätään rivi 8380001. [VAR rivinumerot/kertoimet, ARV merkitykset]
- **Pelisääntöesine** (Judicator's Rulebook): PvP päälle/pois, PvP-tiimit ja friendly fire on/off, ja muutokset ilmoitetaan kaikille. Invaasion ehdot ovat "kaikkien pelaajien oltava paikalla" ja "N pahaa vaeltajaa estää usvan". [VAR]
- **Lukitus:** maailman voi lukita ja avata vaeltajille, ja salasanan voi katsoa pelissä. Evil session (PvP-areena), boss rush (BOSS / DLC / ALL) ja enemy rush (Easy–Impossible). Rune Arc maksetaan runeilla. [VAR]
- "This mod uses a separate save and does not connect to the matchmaking servers" / "It does not contain an anticheat" ovat käynnistysbannerin tekstejä. [VAR]

## 7. Opittavaa Sekiro-modiin (kohta 6), prioriteetin mukaan

Meidän malli on oma UDP + oma protokolla, natiivi remote-PlayerIns oman recv-sessionin kautta ja NetAI-viholliset (IMPLEMENTATION-NOTES §1, §3, §7). ERSC:n malli on pelin oma sessio ja oma ohjauskanava. Alla on se, mitä siitä kannattaa ottaa.

1. **Lepo = reset kaikille + potku boss-areenalta** (prioriteetti 1). Meillä P2 peilaa levon jo (`0x3AFEB98`/`0x3AFEBA0`, `0xA4C0F0`). Lisäksi: jos paikallinen pelaaja on bossitaistelussa (meidän bossipalkki-signaali, RE-FINDINGS-2 §4), kun kaveri lepää, siirretään hänet areenan ulkopuolelle (viimeisin idoli tai areenan sisäänkäynti) ja näytetään ilmoitus. Ilman tätä toinen pelaaja on resetoidussa maailmassa ja toinen bossissa. *Sekiro-RE: areenan ulkopuolinen piste (bossin sumuportin/sisäänmenon sijainti tai idolin warp) ja warp-funktio.*
2. **"Kaikkien oltava paikalla" -lippuportti** (prioriteetti 1). Meidän `should_sync` tuntee vain jaettavat ja jakamattomat liput. Lisätään kolmas luokka: bossin aloitus- ja sumuportin liput. Niitä asetettaessa tarkistetaan, että kaveri on samassa blokissa ja valmis. Muuten asetus estetään ja näytetään viesti. Tämä estää puolikkaat bossitaistelut. *Sekiro-RE: listataan bossien aloitusliput (common EMEVD).*
3. **Kuolemasäännöt ERSC:n tapaan** (prioriteetti 1). Kuollut herää yksin idolilla, eikä vihollisia resetoida kenellekään. Bossissa kuollut on katsoja, kunnes boss kuolee tai kaikki kuolevat (wipe → yhteinen reset). Meidän P3-lite (EV_DEATH/EV_BACK, `BULK_DEAD_LIST`) on lähellä. Lisätään eksplisiittinen wipe-tila ja kuolemalaskuri nimikilpeen. *Sekiro-RE: kuolleen kameran kiinnitys toiseen chr:ään (SprjCam-kohde), ja kuoleman uudelleenlatauksen estäminen tai viivästys bossin ajaksi (CSDeadStep, RE-DEATH-RESET §1.2).*
4. **Kuolemarangaistus väärinkäytön estoon** (prioriteetti 2). ERSC:n Rot: pinoutuva debuff, joka poistuu vain levossa. Sekirossa luonteva vastine on oma SpEffect tai lohikäärmetauti-tyylinen pino, joka synkataan omana tilana (liput 9500–9599 pysyvät synkkaamatta, P1).
5. **Skaalaus SpEffect-riveillä** (prioriteetti 2). Paramitaulukkoon lisätään latauksen jälkeen omat rivit (HP- ja posture-kerroin per pelaaja), ja ne annetaan host-vihollisille ja bosseille spawnissa. Tämä on vakaampaa kuin HP:n kirjoittaminen suoraan, ja se toimii NetAI-tilan kanssa. Oletukset ERSC:stä: HP +35 %/pelaaja, bossi +100 %, posture +15/+20 %, vahinko 0. *Sekiro-RE: SoloParamRepository/SpEffectParam-taulun rakenne ja rivin lisäys, SpEffectin anto chr:lle.*
6. **Viestikanavan rakenne** (prioriteetti 2). Tyypitetyt viestit, kanava ja luotettavuus viestiluokittain, vain "valmiille" jäsenille, kokoraja ja pakkaus. Meillä on bulk + unreliable. Erotetaan selvästi "ohjausviestit" (lippu, lepo, kuolema, ilmoitus: luotettava, järjestetty) ja "virta" (snapshotit, PlayerRaw: epäluotettava).
7. **Robusti versioriippuvuus** (prioriteetti 2). AOB-kuvio + varakuvio + selkeä virhe "peliversio ei sovi" ennen yhtäkään patchia. Singletonit nimellä tai RTTI:llä kovakoodattujen RVA:iden sijaan. Meidän `rtti`-apu on jo olemassa. Yksi alustus, joka kaatuu siististi.
8. **Bossipalkkio kaikille** (prioriteetti 3). Kun boss kaatuu hostilla, liittyjä saa saman palkkion (muisto/helmi/XP) omaan maailmaansa paikallisesti, jos sitä ei ole jo. ERSC hookkaa block clear -palkkion. *Sekiro-RE: bossin kaadon item lot ja kokemusfunktio.*
9. **Pelin oma sessio ja EMEVD-ehtohookit** (prioriteetti 3, iso). Jos siirrymme joskus FrpgSessionManageriin, ERSC:n resepti on tämä:
   - matchmaking Steam-lobbylla;
   - pelin "be joined" -polku;
   - MultiplayState-ehto pakotetaan valituille eventeille yksinpeliksi;
   - IsMyWorld/ChrType-tyyppiset ehdot hyväksyvät liittyjän.
   Nyt emme ole pelin sessiossa, joten EMEVD näkee jo valmiiksi yksinpelin. Tämä on syy, miksi meillä ei ole sumuseinäongelmaa. *Sekiro-RE: Sekiron MultiplayerState-enum on eri (Host 0, Client 1, Trying… 2–5, [WEB] soulsmods), ja EcTest-vastineiden luokat on löydettävä.*
10. **Pienet:** erillinen tallennuspääte (meillä `S0000.coop.sl2`, ok), nimikilven tilat (ping/kuolemat), estolista ja muistirajojen nosto, jos pelaajia tulee yli kaksi.

**Vaativat oman Sekiro-RE:n:** 1 (areenapiste, warp), 2 (bossiliput), 3 (katsojakamera, kuolemalatauksen viive), 5 (param-rivin lisäys, SpEffectin anto), 8 (palkkiot), 9 (session ja EcTest-luokat). 4, 6, 7 ja 10 onnistuvat nykyisillä tiedoilla.

## Lähteet [WEB]
- https://www.pcgamer.com/is-elden-ring-co-op-multiplayer/
- https://www.gamepressure.com/download/elden-ring-seamless-co-op-v174-mod/zf13deb
- https://ersc-docs.github.io/faq/
- https://steamcommunity.com/app/1245620/discussions/0/3395163747104259416/
- https://www.resetera.com/threads/elden-rings-seamless-co-op-mod-rewritten-with-support-for-shadow-of-the-erdtree-dlc.915549/
- https://github.com/micthiesen/unseamless-coop (README ja releases)
- https://soulsmods.github.io/emedf/er-emedf.html, https://soulsmods.github.io/emedf/sekiro-emedf.html
- Nexus-sivu https://www.nexusmods.com/eldenring/mods/510 palautti 403. Changelogin tiedot (2.0.0/2.0.1) ovat hakutuloksen tiivistelmästä.

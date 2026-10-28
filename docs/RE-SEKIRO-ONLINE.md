# RE: Sekiro Online v2.05 (`dinput8.dll`, Yui) – toimintaperiaatteet (2026-10-23)

Kohde: `C:\Users\<user>\Downloads\SSDT_Fix_Repair_Steam\dinput8.dll` (SHA-256 `880bea34…f912c`, 424 960 B, x64, PDB `…\Sekiro-Online-Release\x64\Release\dinput8.pdb`).
Analysoitiin kopio `F:\Temp\re_so\so.dll` (IDA 9.3 headless, kanta `F:\Temp\re_so\so.i64`). Pelin puoli: kopio `F:\Temp\re_so\game\g.i64` (quick_re7, 1.06, base `0x140000000`).
Peliä ei ajettu, mitään asennusta ei muutettu. Raportissa on vain ideat, rakenteet ja osoitteet, ei heidän koodiaan.

Merkinnät: **[VAR]** = varmistettu disassemblystä/dekompiloinnista, **[ARV]** = päätelty kontekstista, tarkistettava ajossa.
Kaikki pelin osoitteet ovat RVA:ita (1.06). DLL:n sisäiset osoitteet (`0x18…`) ovat vain viitteeksi.

Raakadata: `F:\Temp\re_so\sigtab.txt` (DLL:n AOB-kuviot), `F:\Temp\re_so\gsig2.txt` (kuviot ratkaistuna pelin RVA:iksi), `F:\Temp\re_so\ctx.txt` (pakettityypit hook-kohdissa), `F:\Temp\re_so\o1\all.c` (DLL:n dekompilaatti, vain omaan käyttöön).

---

## 0. Lyhyesti

- DLL **ei ole pakattu eikä virtualisoitu** [VAR]. Merkkijonoja on piilotettu pienillä XOR-silmukoilla, ja paketit on suojattu omalla tiivisteellä ja salauksella (YuiKey-luokat). RTTI-nimet näkyvät.
- Arkkitehtuuri on **"pelin oma netcode, vaihdettu kuljetus"** [VAR]. Peli tuottaa ja kuluttaa omat pakettinsa (pelaaja type 1, viholliset 4/22/23/70/71, liput 26 jne.). DLL ohjaa jokaisen `FrpgSessionManager`-lähetyksen ja -vastaanoton kutsukohdan omiin funktioihinsa. Kuljetuksena on Steam-lobby ja `ISteamNetworkingSockets009` (SDR-relay).
- Pelin oma sessio (`SprjSessionManagerImp`, `*(base+0x3D948E8)`, 56 B:n jäsenvektori) **jää tyhjäksi** [VAR]. Peli luulee olevansa online, koska noin 40 kutsukohtaa tarkistuksiin "IsHost / IsClient / InSession / jäsenmäärä" on vaihdettu DLL:n omaan tilaan.
- Etäpelaaja on **natiivi PlayerIns**, ja se luodaan pelin oman "jäsen liittyi" -spawnin kautta (`0xA7A840`). Sijainti ja animaatio tulevat pelin omista type 1 -paketeista ja NetworkManipulatorista. Omaa sijaintikorjausta ei löytynyt [ARV].
- Viholliset käyttävät **koko natiivia NetAI-omistusprotokollaa** (isäntä jakaa omistuksen pyyntölistojen perusteella). DLL vain välittää paketit ja väärentää jäsenmäärän ja isäntäroolin.
- Kuolemaan ei ole omaa käsittelyä: kuolema-, respawn- tai rangaistusfunktioita ei ole AOB-listoissa [VAR, puuttuu]. Sessio on Steam-lobby, joten se ei katkea kuolemaan. Peli tekee tavallisen uudelleenlatauksen, ja DLL luo etäpelaajat uudelleen latauksen jälkeen [ARV].

---

## 1. Tekniikka (miten DLL koukuttaa)

**AOB-taulut** [VAR]. Datassa on kaksi taulukkoa, joissa jokainen merkintä on `{pattern*, mask*("x?"), u32 maskin pituus}`. Init-funktiot skannaavat `sekiro.exe`:n ja tallentavat osoitteen + vakio-offsetin olion kenttiin.
- **A-taulu (85 funktiota, joita DLL kutsuu):** 84/85 löytyy 1.06:sta yksiselitteisesti. `A[1]` ei löydy, ja `A[51]` löytyy kahdesti.
- **B-taulu (121 hook-/patch-kohtaa):** 119 löytyy, joista 1 kahdesti. Kuviot ratkaistiin pelin kantaan skriptillä, ja tulokset ovat tiedostossa `gsig2.txt`.

**Hook-tavat** [VAR], `CHooks`-vtable:
- `[0] Hook(target, detour, &paluu, paluuOffset)`: kirjoittaa 5 tavun hypyn detouriin. Jos `paluu` annetaan, kyseessä on keskelle funktiota tuleva hook: asm-tynkä tekee oman työnsä ja hyppää kohtaan `target+offset`.
- `[1] Patch(addr, bytes, n)`: VirtualProtect + memcpy.
- `[2]`: vtable-slotin vaihto.
- **Kutsukohdan uudelleenohjaus:** `Hook(callsite)` ja sen jälkeen `Patch(callsite, E8, 1)`. Pelin `call X` muuttuu muotoon `call detour`, eli vain se yksi kutsu vaihtuu eikä koko funktio. Tätä käytetään noin 70 kohdassa.
- **Yleiset patchit:** `30 C0` (xor al,al = "false"), `30 C0 90 90 90` (korvaa 5 tavun callin), `90…` (NOP).

**Muut** [VAR]:
- Oma säie (`InternalThread.cpp`) pyörii 10 ms välein. Se päivittää session tilan, pumppaa verkkoa ja asentaa `SetUnhandledExceptionFilter`in säännöllisesti uudelleen.
- ImGui-overlay on DX11:llä.
- `DL_PANIC`-käsittelijä `0x19DC9D0` on koukutettu pelkäksi lokitukseksi.
- Versiotarkistus hakee tiedostot `raw.githubusercontent.com/LukeYui/Sekiro-Online/master/VERSION` ja `/DATA` (WinHTTP).
- Ulkoinen TCP-palvelin `74.91.125.31:54000` (winsock, sama 13 B:n kehysformaatti) [VAR]. Tarkoitus on [ARV] matchmaking/telemetria.

---

## 2. Istunto (kysymys 1)

**Kuljetus** [VAR]:
- Steam-callbackit: `LobbyCreated`, `LobbyEnter`, `LobbyMatchList`, `LobbyChatUpdate`, `GameLobbyJoinRequested`, `ValidateAuthTicketResponse`, `ConnectionStatusChanged`, `InitRelayAccess`.
- Rajapinnat haetaan suoraan `steamclient64.dll`:stä: `SteamMatchMaking009`, `SteamNetworkingSockets009`, `SteamNetworkingUtils003`, `SteamUser020`, `SteamFriends017` (oikea `steam_api64` on nimetty `.org`:ksi).
- Lähetys on `SendMessageToConnection` (vtbl +88) ja vastaanotto `ReceiveMessagesOnConnection(conn, msgs, 255)` (vtbl +112), jäsenkohtaisesti. Jäsenen kahva on jäsenrakenteessa offsetissa `+253`.

**Oma tila** [VAR]. Kenttä `P2P+0x50` (luetaan joka hookissa):
- `-1` = ei sessiota.
- `0xFFFEFFFF` = HOST (lobbyn omistaja = oma SteamID).
- `0xFFFDFFFF` = CLIENT.

Säie päivittää kentän 10 ms välein lobby-id:n perusteella. Oma jäsenlista (pelaajat 0…n, 0 = itse) on 58B0-luokassa, ja jäsenmäärä on `sub_1800479D0` (päivitetään globaaliin).

**Mitä pelistä väärennetään.** Pelin funktiot ovat [VAR]. DLL ei koske itse funktioihin, vaan vaihtaa yksittäisiä kutsukohtia:

| Pelin tarkistus | Mitä se on | DLL:n korvaaja | Kutsukohdat (esim.) |
|---|---|---|---|
| `0x1072FE0(S)` | InSession (`S+0x18` aktiivinen) | tila ≠ −1 | `0xA1AF9C, 0xA1B36D, 0xA1BE41, 0xA1C18B, 0xA1C8A3, 0xA1CA03, 0xA1DB08, 0xA1D956-62` |
| `0x1072F80(S)` | `S+0x184 == 3` (= host) | tila == HOST | `0xA1CF85, 0xA1DDC6, 0xA1D4B0, 0xA4B8A4` |
| `0x1072F00(S)` | `S+0x184 == 6` (= client) | tila == CLIENT | `0xA1D506, 0xA4BA81` |
| `0x8E4B10(FSM)` | `FSM+0xDC == 4` (client) | tila == CLIENT | `0xA6F351` (maailman lataus) |
| `0x832050()` | `GameMan(0x3D5C0D0)+0xBA6` (offline-lippu) | tila == −1 | `0x76F10B, 0xF0E6B7, 0x89A689, 0xA4CF79` |
| `0x8E3E60(FSM)` jäsenmäärä | `(S+0x168−S+0x160)/56` | oma määrä | `0xA4DC38` (chr-silmukka), `0xA734D8`; inline-laskut `0xA1CF13, 0xA1DD5E, 0xA4E5BE` (keskelle tuleva hook: `edx = oma määrä`) |
| `0x6BE270()` | jäsenmäärä > 0 ("online"-ehto) | oma määrä > 1 | `0x6BC504` |

`S = *(base+0x3D948E8)` ja `FSM = *(base+0x3D67C90)` (FrpgSessionManager).

**Väärennöksen kaksi tärkeää pointtia** [VAR]:
- Jäsenvektoriin ei lisätä mitään. Sitä ei siis tarvitse rakentaa, eikä DLNRD-jäsenosoittimia dereferoida.
- Se on juuri se kirurginen tapa, jota RE-ENEMY-TARGETING.md arvioi, mutta laajennettuna kaikkiin NetAI-kohtiin.

Seuraukset [ARV]:
- Jos jäsenvektori olisi tyhjä, `0x8E7210` (lähetä kaikille) ei tekisi mitään. Siksi DLL ohjaa myös jokaisen send/recv-kutsun (§5).
- Oikeaa `SessionManager`ia ei tarvitse käynnistää.

---

## 3. Pelaajien synkka (kysymys 2)

**Lähetys** [VAR]:
- Pelin `0x8DA080` sarjallistaa pelaajapaketin (`0x8D7000`, ≤ 1024 B) ja kutsuu `session.vtbl[1]`:tä (`call [r10+8]` @ `0x8DA0DD`, hook B[2]).
- DLL lähettää saman tavujonon omana tyyppinä 1 kaikille, 4 B:n otsikolla `{u16 oma indeksi, u16 x}`.
- Tahti on **pelin oma**, eli aina kun peli itse lähettäisi (ks. IMPLEMENTATION-NOTES: noin 12/s) [ARV tahdista].

**Vastaanotto** [VAR]:
- Koko pelin vastaanottosilmukka `0x8D8050` korvataan (B[3]). DLL etsii oman NetPlayerinsa, jonka session-osoitin (NetPlayer `+0x2038`) on sama kuin kyseisen chr:n sessio (`*(*(chr+0x1E0)+0x18)`).
- Se ottaa tyypin 1 viimeisimmän paketin (≤ 2056 B) ja kirjoittaa session `+0x148/+0x14C` = otsikon u16:t.
- Sitten se ajaa pelin omat purkufunktiot: `A[24] 0x8DD210`, `A[25] 0x8E8510` ×2 ja `A[26] 0x8DEA40` (sama ketju kuin RE-FINDINGS-2.md:ssä).
- Sijainti, HP, animaatio ja tilat menevät siis **pelin NetworkManipulatorille**. Omaa sijainnin pakotusta ei löytynyt [ARV: ei löytynyt chr-fysiikkakirjoituksia]. Me teemme tässä enemmän (pakotus 60/s).

**PlayerDebugSession-vtable** [VAR]. DLL kirjoittaa pelin **.rdata-vtableen** `0x2B86048`, joten muutos on globaali:
- slot 4 (type 20, 192 B): lähetetään DLL:n tyyppinä 6, 168 B, sille jäsenelle.
- slotit 7/8 (vastaanotto, tyypit 60/61): luetaan DLL:n tyypeistä 35/36 siltä jäseneltä, joka on liittymässä eikä ole vielä spawnannut.
- Samat tyypit 60/61 ja 8 (416 B) lähtevät `0x8DAE90`:sta (hookit B[115–117]).

Tyypit 60/61/8/20 ovat [ARV] "pelaajan perusdata / varusteet / liittymiskättely".

**Etäpelaajan luonti** [VAR]:
- Hook B[1] korvaa kutsun `0xA7A9F0` funktiossa `0xA7A080` (TestNetStepin päivitys) ja NOPpaa sitä edeltävän 2 tavun ehdon.
- DLL:n funktio käy joka ruutu läpi NetPlayerit, joilla on "valmis spawnaamaan" -liput, ja kutsuu luontia.
- Luonti on **`A[19] 0xA7A840(step, &init, &chrType=1, warp)`**, eli pelin oma "jäsen liittyi" -spawn, ei debug-`0xA49460`. Init on `{i16 playerNo (paketista), manipType 2 (0x200000000), PGD = GDM+0x10 + 0xA10*jäsenIndeksi, MenuMan+0x3238}`.
- Luonnin jälkeen:
  - `chr+0x7D8` = jäsenen 8 B:n tunniste (SteamID) [ARV].
  - 80 B ulkonäkö/kasvo → `*(*(chr+0x868))+0xA0` [ARV].
  - varusteet `A[36/37] 0x7C2C50 / 0x7C4DD0` [ARV].
  - lopuksi asetus id 4378 [ARV, SpEffect?].
- Poisto: `A[21] 0xA4BF90`.
- Hyödylliset apufunktiot: `A[20] 0x7F86F0` (init), `A[50] 0x10B8240` (CharaInitParam), `A[23] 0xA49F90` (GetChrInsWithHandle).

**Muut pelaajapaketit** (pelin tyyppi → DLL:n tyyppi) [VAR kohdat, ARV merkitys]:
- 21 (208 B, `0x8DA215` kaikille / `0x8DBA35` isännälle) → 27
- 28 (128 B, `0x8D9484`) → 10
- 18 (44 B, `0x8D9E74`) → 14
- 42 (`0x8DA142`) → 19
- 45 (6 B, `0x8DB7F4`) → 20
- 52 (104 B, `0x8D9464`) → 21
- `0x8D9583/0x8D9684` → 3
- `0xB1D6AE` → 8
- `0xA2E22D` → 2 (160 B, oma tila)

SpEffect- tai osumatieto:
- Hook `0x9E9F53` (kutsu `A[44] 0x9F8320`, 11 argumenttia) lähettää omalle pelaajalle tyypin 9 (12 B) ja viholliselle tyypin 29 (10 B, netId + id). Sen jälkeen alkuperäinen ajetaan [VAR kulku, ARV = "lisää SpEffect / osuma"].

---

## 4. Vihollisten synkka (kysymys 3)

**DLL käyttää pelin natiivia NetAI-järjestelmää sellaisenaan** [VAR]. Kaikki alla olevat funktiot ovat sync-taulun `T = *(WCM+0x630)` metodeja, ja ne toimivat, kun session tarkistukset on väärennetty (§2):

| Pelin tyyppi | Funktio | Suunta | Sisältö [VAR] |
|---|---|---|---|
| 4 (256 B) | `0xA1C100` lähetys kaikille, `0xA1BDE0` vastaanotto | omistaja → muut | Liikerekordit (28 B, lippu 0x02), enintään 8 vihollista/paketti, kursori kiertää (`T+0x3A8/+0x3AC`). Vain omat (`0xA4A650`). |
| 70 (≤ 0x5700) | `0xA1B320` lähetys, `0xA1AF40` vastaanotto | omistaja → muut | Käytösrekordit (hkb-tilat) [ARV] |
| 71 (52 B × n) | `0xA1DAC0` lähetys | omistaja → muut | `{u32 netId, 48 B action-rekordi}` jonosta `T+0x6998` |
| 5 (80 B) | `0xA1C860` vastaanotto isännältä | isäntä → client | [ARV] tila/aika |
| 22 | `0xA1CED0` → isännälle | client → isäntä | Ehdokaslista `u32[n] = netId \| prio<<16` (`T+0x380..0x388`), ajastin `T+0x3A4/+0x3A0` |
| 23 | `0xA1DD00` | isäntä → kukin | Omistuslista: vastaanottaja nollaa taulun (`0xA19A50` → −1) ja merkitsee listatut omiksi (`0xA19B40`) |

**Omistuksen jako (natiivi)** [VAR]:
- Isäntä kerää jokaiselta jäseneltä tyypin 22 listan (`0xA1C6C0`) ja oman listansa (`0xA1D7E0`).
- Ristiriidat ratkaistaan (`0xA1D550` → `0xA1C590`): **suurempi prio voittaa, tasapelissä se jäsen, jolla on vähemmän vihollisia**.
- Jako toistuu ajastimella (`T+0x39C/+0x398`). Omistus siis **siirtyy dynaamisesti** sen mukaan, mitä kukin ehdottaa.
- [ARV] prio = läheisyys/aggro. Se on pelin sisäistä, eikä DLL laske sitä.

**DLL:n osuus vihollisissa** [VAR]:
- Vaihtaa näiden funktioiden session tarkistukset (yllä) ja ohjaa niiden send/recv-kutsut omille tyypeilleen: 4→22, 70→25, 71→30, 22→24 (isännälle), 23 vastaanotto (oma 23), 5 (oma tyyppi).
- Chr-silmukan `0xA4DC38` jäsenmäärä tulee DLL:ltä. Kun pelaajia on 2 tai enemmän, `forceLocal = 0` ja omistustaulu ratkaisee slot 37:n valinnan (sama mekanismi kuin RE-FINDINGS-2.md:ssä).
- DLL kutsuu myös suoraan `A[54] 0xA1D7E0` ja `A[55] 0xA1C6C0` [VAR].

**HP, kuolema ja vahinko vihollisille** [ARV]:
- Omaa HP-kanavaa ei löytynyt. Vahinko lasketaan paikallisesti (osumat kulkevat hyökkääjän pelissä).
- Viholliskohtaiset SpEffectit menevät tyypillä 29.
- Kuolemat ja HP tulevat pelin omissa rekordeissa (käytös/action). Mekanismia ei varmistettu.

**Moninpeliskaalaus** [VAR kulku, ARV merkitys]:
- Keskelle funktiota tulevat hookit kohdissa `0xC04EA4` (`movss xmm1,[rcx]`) ja `0xC04FFA` (`[rcx+8]`): jos ollaan sessiossa ja pelaajia on n > 1, arvoon lisätään `n × 0.3`.
- Clientille `0x10B8D30` antaa korvaavan param-rivin.
- Tämä on [ARV] vihollisten HP-/vahinkokerroin pelaajamäärän mukaan.

---

## 5. Kuolema, Unseen Aid, lepo, bossit, sumuseinät (kysymys 4)

- **Kuolema:** ei hookia kuolemapolkuun. AOB-listoista puuttuvat [VAR]: `0xA8F7E0` (respawn-lataus), `0x11D3430` (rangaistus), `0xA2CD50` (respawn), `0xA2BE40` (lepo) ja CSDeadStep.
  - Sessio on Steam-lobby, joten kuolema ei pura sitä. Pelin `0xA73430` (purku, kun jäseniä ≤ 1) saa jäsenmäärän DLL:ltä (`0xA734D8`), joten väärää purkua ei tule [VAR].
  - Latauksen jälkeen B[1]-hook luo etäpelaajat uudelleen [ARV].
- **Liput** [VAR]:
  - Pelin `0x8D9ED0(flagId, state)` lähettää tyypin 26 vain jos `IsFlagNetSynced 0x6C4240` on tosi. Kutsukohta `0x8D9F46` → DLL:n tyyppi 17 (5 B: `u32 id, u8 state`), kaikille.
  - Vastaanottaja asettaa lipun `CNetWorldChr`-käsittelijässä. **Pelin oma suodatin säilyy.**
  - Lisäksi `0x6C44A0` (lippublokin nollaus, `a1+0x228`) korvataan: nollaus tehdään vain offline-tilassa (sessiossa lippuja ei pyyhitä).
- **Maailman lataus clientinä** (`0xA6F060`, [VAR] kohdat, [ARV] merkitys):
  - `0xA6F351` (IsClient) ja `0xA6F404` (`0xA91290`: client → aina tosi) valitsevat clientille "vieraan" lippumoodin: `EventFlagMan+0x220 = 1`, edellinen tallessa `+0x224`.
  - `0xA6F45A` kirjoittaa `+0x220 = (tila == −1)`.
  - [ARV] Kyse on DS3:n natiivista phantom-moodista: clientin oma tallennus ei etene isännän maailmassa.
- **EMEVD/ESD näkee yksinpelin** [VAR patch, ARV vaikutus]:
  - `0x69D310`:n (EMEVD-moninpeliehtojen arviointi) kuusi kutsua `0x69D44C…0x69D491` on korvattu muotoon `xor al,al`. Funktiot ovat `0x68ABF0/AA60/B090/A950/AB80` (`MultiplayerState == 0/8/1/2/12`) ja `0x68AA80` (`0x8E4790`).
  - Lisäksi `0x6A2F8E` (`test al,al` → `xor al,al`).
  - ESD:n `env`-kysely `0x8522B0` (kohdat `0x84EE44`, `0x84EECD`) korvataan omalla, joka vastaa id:ille 60–67 ja 3980 session mukaan.
  - Seuraus [ARV]: bossit, sumuseinät ja tapahtumat ajetaan kuten yksinpelissä, ja moninpelirajoitukset (esim. "phantom ei pääse") eivät laukea.
- **Unseen Aid / rangaistus:** ei omaa käsittelyä [VAR, puuttuu].
- **Aikaskaala** [VAR, ARV merkitys]: hook `0x545020` (FD4Time-ctor) asettaa paikallisen pelaajan kertoimeksi 1.0 sessiossa. Tämä estää hidastukset.
- **Kaatumissuojat** [VAR]:
  - `0xB573E0`: PlayerInAirAction-puskurin raja.
  - `0x19DC9D0`: DL_PANIC.
  - `0x7C0DE0`: client ohittaa (`state & 0x20000 == 0` → paluu heti).
- **Omat online-esineet** [VAR]: Cursed Emblem, Coward's Bell, Demon Eyes, Ethereal Confetti jne. Ne tehdään param-hookeilla (`0x10BF630` id:t 60–67, `0x10B4BF0` id:t 17000/17001, `0x10CEC30`) ja FMG-tekstihookeilla (`0x10AA24E`, `0x10AA1CE`, `0x10A9E9E`).

---

## 6. Pakettiformaatti (kysymys 5)

**Kehys** [VAR]: `{u32 tiiviste, u32 kanava, u32 hyötykuorman pituus, u8 tyyppi} + data`. Otsikko on 13 B, pituus enintään `0x3E80D`.
- Tiiviste lasketaan datasta ja lähettäjän SteamID:stä (YuiKey 58C0 vtbl[0]). Data salataan vtbl[1]:llä ja puretaan vtbl[2]:lla.
- Vastaanottaja tarkistaa pituuden ja tiivisteen.
- Kanavat:
  - 1 = pelin viestit (tyypit ≤ 50)
  - 2 = protokolla/kättely (`CProtoServer`/`CProtoGame`, versio-/kirjastotarkistus)
  - 3 = kolmas käsittelijä

**Vastaanottopuskuri** [VAR]: **yksi paikka per jäsen per tyyppi**, jäsen `+258549 + 25605*tyyppi`: `{u8 on, u32 len, u8 data[25600]}`.
- Uusi viesti **ylikirjoittaa** edellisen. Lukija (pelin hook tai CNetPlayer/CNetWorldChr-käsittelijä) ottaa sen ja nollaa lipun.

**Luotettavuus** [VAR arvot, ARV merkitys]:
- Kaikki pelin viestit lähtevät `a6 = 1` → Steam-liput `2`. Ohjauslähetykset `a6 = 2` → `2` ja `a6 = 3` → `1`.
- Steamworksin `k_nSteamNetworkingSend_*`: Unreliable = 0, NoNagle = 1, NoDelay = 4, **Reliable = 8**. **Arvoa 2 ei ole määritelty, joten kaikki menee käytännössä epäluotettavana.** [ARV: vaikuttaa EP2PSend-sekaannukselta, jossa 2 = Reliable.]

**Kulkeva tieto:**
- pelaajan oma pakettivirta (type 1)
- liittymisdata (20/60/61/8)
- pelaajan pienet tilat (18/21/28/42/45/52)
- SpEffectit (9/29)
- liput (17)
- vihollisten liike, käytös ja action (22/25/30)
- omistus (23/24)
- oma tilapaketti (2, 160 B)
- sisäiset (5/7/15/16/26/31–34)

**DLL:n omat tyypit 1–36** (lähetysfunktiot) [VAR]:
- 1 pelaaja
- 2 tila
- 3 `0x8D9583/684`
- 8 `0xB1D6AE`
- 9/29 SpEffect
- 10 type28
- 13 type(r9−6)@`0x8D9EB8`
- 14 type18
- 16 objektitapahtumat 32 B jonossa (`0x65F657`, poislukien id:t 12005462 / 20005450 / 11115770)
- 17 liput
- 19 type42
- 20 type45
- 21 type52
- 22 vihollisliike
- 24 ehdokkaat
- 25 käytös
- 27 type21
- 30 action
- 35/36 type60/61

Vastaanotto: CNetPlayer-vtable lukee tyypit 2, 3, 5–9, 11, 12, 15 ja 18. CNetWorldChr lukee tyypit 13, 16, 17, 24, 26, 29 ja 31–34.

---

## 7. Mitä meidän modiin kannattaa ottaa (prioriteettijärjestyksessä)

1. **[Korkea] Väärennä istunto kutsukohdittain, älä vektorilla.** Meillä on jo slot 37 -hook. Sekiro Online osoittaa, että koko natiivi NetAI-kone käynnistyy, kun nämä kohdat vastaavat oikein:
   - jäsenmäärä: `0xA4DC38`, inline `0xA1CF13`, `0xA1DD5E`, `0xA4E5BE`
   - IsHost: `0xA1CF85`, `0xA1DDC6`, `0xA1D4B0`, `0xA4B8A4`
   - IsClient: `0xA1D506`, `0xA4BA81`
   - InSession: §2:n lista

   Vektoria ei kosketa, joten DLNRD-kaatumisriskiä ei ole.
2. **[Korkea] Anna pelin itse tehdä vihollispaketit ja välitä vain tavut.** Koukuta send/recv-kutsukohdat (`0xA1C4F9`/`0xA1BF0B` type 4, `0xA1B4BA`/`0xA1B07B` 70, `0xA1DC74` 71, `0xA1D1FC` 22, `0xA1E005`/`0xA1E075` 23) meidän UDP:hen. Silloin saamme natiivin omistuksen jaon (prio + kuormantasaus) ja liikkeen ilman omia rekordikirjoituksia. Nykyinen "isäntä omistaa kaiken" on erikoistapaus tästä.
3. **[Korkea] Lippusynkka natiivisuodattimella.** Koukuta `0x8D9F46` (type 26 lähetys `0x8D9ED0`:ssa), jolloin vain `IsFlagNetSynced`-liput lähtevät. Lähetä ne **luotettavasti ja jonossa** (ei viimeisin per tyyppi). Ohita lisäksi `0x6C44A0`:n nollaus session aikana.
4. **[Keski-korkea] Bossit ja sumuseinät yksinpelinä:** `0x69D310`:n moninpeliehdot (6 kutsua) → false ja `0x6A2F8E`. ESD:n `env 0x8522B0` -id:t 60–67 ja 3980. Sopii meidän "kumpikin pelaa omaa maailmaansa" -malliin. Tarkista ensin RE-DEATH-RESET.md:n P-patchien kanssa.
5. **[Keski] Etäpelaajan luonti `0xA7A840`:llä** (pelin oma jäsenspawn, sama ketju kuin debug-moodi 1, mutta `chrType` ja PGD-indeksi jäsenen mukaan). Meidän N1-polku käyttää jo `0xA49460`-osia. Arvioi, korjaako `0xA7A840` (warp + tyyppi) jotain tämänhetkisestä käsityöstä.
6. **[Keski] Liittymisdata:** PlayerDebugSessionin slot 4 (type 20) ja slotit 7/8 (60/61) sekä `0x8DAE90`:n type 8/60/61. Natiivi etäpelaaja saattaa tarvita nämä, jotta varusteet/nimi/taso tulevat pelin omalla tavalla (meillä nyt `dress_like`) [ARV].
7. **[Keski] SpEffect-synkka:** kutsukohta `0x9E9F53` (`0x9F8320`) pelaajalle ja vihollisille (netId-muodossa). Ratkaisee buffit ja tilat (palaminen, myrkky jne.) [ARV merkitys].
8. **[Matala-keski] Moninpeliskaalaus** `0xC04EA4`/`0xC04FFA` (+0.3/pelaaja): halutaanko sama (tai pois)?
9. **[Matala] Kaatumissuojat:** `0xB573E0` (InAirAction-puskuri) ja `0x19DC9D0` (DL_PANIC → loki). Halpoja ja hyödyllisiä, koska natiivi etäpelaaja on päällä.
10. **[Matala] Clientin maailmamoodi** `0xA6F060`: `EventFlagMan+0x220/+0x224` (vieras-lippupankki) liittymissnapshotin vaihtoehdoksi tai täydennykseksi [ARV, vaatii ajotestin].

## 8. Mitä EI kannata kopioida (heidän tunnetut tai ilmeiset heikkoudet)

- **Viimeisin per tyyppi -postilaatikko:** tapahtumaviestit (liput, SpEffectit, omistuslistat) katoavat, jos samaa tyyppiä tulee kaksi ennen lukua [VAR mekanismi]. Käytä jonoja; ylikirjoitus vain tilavirroille (type 1, liike).
- **Kaikki epäluotettavana** (Steam-liput 2): luultava syy liput-/boss-desynceihin [ARV]. Meille luotettava kanava tapahtumille.
- **Globaali .rdata-vtable-patch** (PlayerDebugSession `0x2B86048`, myös paikallinen pelaaja käyttää sitä): meidän per-objekti-vtable-kopiot ovat turvallisempia. Pidä ne.
- **Ei kuolemalogiikkaa:** heillä kuolema = pelin oma lataus ja sessio vain jatkuu. Meidän RE-DEATH-RESET-säännöt (lepo/reset/Unseen Aid) ovat oma lisäarvomme.
- **Riippuvuudet:** Steam-lobby ja CreamAPI-appid 480 (SSDT-paketti), ulkoinen palvelin `74.91.125.31:54000`, GitHub-versiohaku, XOR-piilotus ja oma salaus. Ei meille (legal/ToS ja yksityisyys; HANDOFF §9).
- **Pelaajan sijainti vain manipulaattorin varassa** (ei korjausta): meidän 60/s-pakotus on parempi. Älä poista sitä.

---

## Liite A: DLL:n kutsumat pelin funktiot (A-taulu, 1.06 RVA) [VAR osoite, ARV rooli]

| A | RVA | Rooli |
|---|---|---|
| 4 | `0x6795F0` | matka idolille (warp) |
| 14 | `0x76EE60` | idolin aktivointi |
| 17 | `0x91C970` | GiveItem |
| 19 | `0xA7A840` | etäpelaajan spawn |
| 20 | `0x7F86F0` | player init |
| 21 | `0xA4BF90` | poisto |
| 23 | `0xA49F90` | chr kahvasta |
| 24–26 | `0x8DD210`, `0x8E8510`, `0x8DEA40` | type 1 -purku |
| 28/29 | `0xFF1D10`, `0xFF1DA0` | hkb-tapahtuma |
| 44 | `0x9F8320` | SpEffect/osuma |
| 47 | `0x10BF630` | param-haku |
| 50 | `0x10B8240` | CharaInitParam |
| 54/55 | `0xA1D7E0`, `0xA1C6C0` | omistusehdokkaat |
| 71–74 | `0x6C3610`, `0x6C4A10`, `0x6C4520` (SetEventFlag), `0x6C3FF0` | lippuluku |
| 83 | `0x8DAE90` | pelaajadatan lähetys (8/60/61) |

Loput (`0xE2E260`, `0x68E7F0`, `0x508730`, `0x2F5520`, `0x1110A60`, `0x8C42C0`, `0x917B20`, `0x91E740`, `0x91F2D0`, `0xBF1830`, `0xB06B70…0xB1E930`, `0xBD64E0…0xBE0DC0`, `0x838810…0x838AB0`, `0x1125870/9B0`, `0xEA6700/7AE0`, `0x8E0530`, `0x8C4B40`) ovat listattuna `gsig2.txt`:ssä, eikä niiden roolia selvitetty.

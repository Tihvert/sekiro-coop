# SPEC: pelin oma netcode meidän UDP:n päällä ("natiivi sessio", testiasetus) (2026-10-23)

Tavoite: peli tuottaa ja kuluttaa omat NetAI- ja pelaajapakettinsa kuten Sekiro Onlinessa (SO), mutta tavut kulkevat `coop-net`-kuljetuksessa. Oikeaa `SprjSessionManager`ia ei käynnistetä eikä sen jäsenvektoria täytetä. Kaikki on testiasetuksen takana. Kun asetus on pois tai linkki katkeaa, alkuperäiset tavut palautetaan ja nykyinen järjestelmä (enemies.rs NetAI-pakotus, oma HP-auktoriteetti) toimii kuten ennenkin.

Lähteet: oma kopio `F:\Temp\re_spec\g.i64` (sekiro.exe 1.06, base `0x140000000`), ajot `F:\Temp\re_spec\o1` … `o10` (`summary.txt`, `f_<ea>.c`), ajuri `F:\Temp\re_spec\job.py` (lisätty `disb` = osoite + tavut + käsky ja `callsto`). SO:n kohdat: `F:\Temp\re_so\gsig2.txt`, `so.asm`. Peliä ei ajettu. SO:n koodia ei ole kopioitu. Alla on vain pelin omat tavut ja omat ratkaisut.
Merkinnät: **[VAR]** = luettu disassemblystä/dekompilaatista, **[ARV]** = päätelmä, varmistettava ajossa. Kaikki osoitteet ovat RVA:ita. Tavut on luettu kannasta, joten ne ovat 1.06:n purettua koodia (levyn exe on salattu).

---

## 0. Arkkitehtuuri lyhyesti

1. **Session tarkistukset väärennetään kutsukohdittain** (§1). Funktioita `0x1072FE0/F80/F00` ei patchata, koska niitä kutsuu myös Steam-, summon- ja tallennuskoodi. Patchit kirjoitetaan vasta, kun rooli (host/client) tiedetään, ja ne palautetaan, kun tila päättyy. Kaikki patchit tehdään `hook::patch_code(rva, expected, new)`:llä pelisäikeessä (FrameBegin), jolloin NetAI-funktiot eivät ole kesken.
2. **Kuljetus vaihdetaan viiden pelin funktion sisääntulossa** (§2.1): broadcast, lähetys isännälle, lähetys jäsenelle, vastaanotto jäseneltä ja vastaanotto isännältä. Hookit tehdään prologin relokoinnilla kuten `observe.rs`:ssä. Valkolistan tyypit menevät meidän jonoihin, ja muut tyypit ajetaan alkuperäisellä koodilla. Tyhjällä jäsenvektorilla alkuperäinen ei lähetä mitään.
3. **Jäsenvektoria iteroivat vastaanottosilmukat** saavat meidän valevektorin (§2.2). Niitä on kolme: tyypit 70, 4 ja 71. Valevektori annetaan lähdeosoitinta vaihtamalla, ei rekisterejä vaihtamalla, eikä oikeaan `S`:ään kosketa.
4. **"Mikä tahansa jäsen" -vastaanottofunktiot** (pelaajan pienet paketit) korvataan kokonaan (§2.3).
5. **Isännän tyyppi 22** (clientin ehdokaslista) syötetään suoraan pelin omilla funktioilla `0xA1D7E0`/`0xA1C6C0` (§4). Jäsenen kautta kulkevaa `0xA4A340`-hakua ei tarvita.

---

## 1. Väärennettävät tarkistukset kutsukohdittain [VAR tavut]

Alkuperäinen on aina `E8 rel32` (5 B), ellei toisin mainita. Patchit:
- `TRUE` = `B0 01 90 90 90` (mov al,1)
- `FALSE` = `30 C0 90 90 90` (xor al,al)
- `CNT2` = `B8 02 00 00 00` (mov eax,2)

Jäsenmäärä on aina 2 (host + yksi kaveri). Yli kahdelle pelaajalle ks. §3.

### 1a. InSession `0x1072FE0(S)` = `add rcx,18h; jmp 0x1BE10C0` (`48 83 C1 18 E9 …`, siis RIP-relatiivinen hyppy: funktiota ei hookata)

| Kohta | Alkup. tavut | Funktio (rooli) | Host | Client |
|---|---|---|---|---|
| `0xA1AF9C` | `E8 3F 80 65 00` | `0xA1AF40` recv 70 | TRUE | TRUE |
| `0xA1B36D` | `E8 6E 7C 65 00` | `0xA1B320` send 70 | TRUE | TRUE |
| `0xA1BE41` | `E8 9A 71 65 00` | `0xA1BDE0` recv 4 | TRUE | TRUE |
| `0xA1C18B` | `E8 50 6E 65 00` | `0xA1C100` send 4 | TRUE | TRUE |
| `0xA1C8A3` | `E8 38 67 65 00` | `0xA1C860` recv 5 (client) | TRUE | TRUE |
| `0xA1CA03` | `E8 D8 65 65 00` | `0xA1C9C0` send 5, muuttuneet | TRUE | TRUE |
| `0xA1CCCA` | `E8 11 63 65 00` | `0xA1CC70` send 5, kiertävä täysi (SO ei patchannut) | TRUE | TRUE |
| `0xA1D918` | `E8 C3 56 65 00` | `0xA1D8D0` recv 71 | TRUE | TRUE |
| `0xA1DB08` | `E8 D3 54 65 00` | `0xA1DAC0` send 71 | TRUE | TRUE |

Muut InSession-kutsut jäävät ennalleen: `0x8E2E83…0x8E7A5F` (sessiomanageri) ja `0x1061C93` (PlayerNetworkSession).

### 1b. IsHost `0x1072F80` (`S+0x184 == 3`) ja IsClient `0x1072F00` (`== 6`)

| Kohta | Alkup. | Mitä valitsee | Host | Client |
|---|---|---|---|---|
| `0xA1CF85` IsHost | `E8 F6 5F 65 00` | `0xA1CED0`: host kerää 22, client lähettää 22 | TRUE | FALSE |
| `0xA1DDC6` IsHost | `E8 B5 51 65 00` | `0xA1DD00`: host jakaa ja lähettää 23, client lukee 23 | TRUE | FALSE |
| `0xA1D42A` IsHost | `E8 51 5B 65 00` | `0xA1D2D0` → `0xA1CC70` (type 5 täysi) | TRUE | FALSE |
| `0xA1D4B0` IsHost | `E8 CB 5A 65 00` | `0xA1D2D0` → `0xA1C9C0` (type 5 muutos) | TRUE | FALSE |
| `0xA1D506` IsClient | `E8 F5 59 65 00` | `0xA1D2D0` → `while(0xA1C860)` (lue 5) | FALSE | TRUE |
| `0xA4B8A4` IsHost | `E8 D7 76 62 00` | `0xA4B830`: host kirjoittaa vihollisten HP:n tauluun | TRUE | FALSE |
| `0xA4BA81` IsClient | `E8 7A 74 62 00` | `0xA4B830`: client asettaa HP:n taulusta | FALSE | TRUE |
| `0xA4BA93` | `84 C0` (test al,al) → `30 C0` | client: ohittaa ehdon `0x832050() == 0` | – | patch, jos `GameMan+0xBA6 ≠ 0` (§7) |

Ennalleen jäävät:
- `0xA1E227`, `0xA1EA50`, `0xA1E5C2`, `0xA1EDCF`: `0xA1E140`/`0xA1E9F0`, uuden jäsenen liittymissnapshot (tyypit 41/55). Niiden listat (`T+0x3F8/+0x410`) täyttyvät vain oikeasta jäsenliittymisestä.
- `0xA1ACCB` ja `0xA1ADDB`: jäsenen liittymis- ja poistumiskäsittelijät.

`0x8E4B10` (`FSM+0xDC == 4`) @ `0xA6F351` jää myös ennalleen. Se valitsisi clientille vieraan lippupankin (`EventFlagMan+0x220`), ja meillä on oma liittymissnapshot (world.rs).

### 1c. Jäsenmäärä `0x8E3E60` = `(S+0x168 − S+0x160)/56`

| Kohta | Alkup. | Merkitys | Patch (molemmat) |
|---|---|---|---|
| `0xA4DC38` | `E8 23 62 E9 FF` | chr-silmukka `0xA4DB20`: `forceLocal = count<=1 \|\| 0x9F33F0(chr)` → slot 37 | CNT2 |
| `0xA4A6CB` | `E8 90 97 E9 FF` | `0xA4A680` IsOwnedHere(chr): `count<=0` → aina tosi (kutsuja `0xA02A10`) | CNT2 |

**Inline-laskut** (`rcx = end − begin`, sitten jako 56:lla). Patchi kirjoittaa `rcx := 56·N`, ja pelin oma jakolasku jää ennalleen. Väliin limitetyt pinotallennukset säilyvät.

| Kohta | Alkup. → uusi | Funktio |
|---|---|---|
| `0xA1CF13` | `49 8B 88 68 01 00 00` → `B9 70 00 00 00 90 90` (mov ecx,0x70) | `0xA1CED0` (22) |
| `0xA1CF24` | `49 2B 88 60 01 00 00` → 7× `90` | |
| `0xA1DD5E` | `49 8B 88 68 01 00 00` → `B9 70 00 00 00 90 90` | `0xA1DD00` (23) |
| `0xA1DD65` | `49 2B 88 60 01 00 00` → 7× `90` | |
| `0xA4E5BE` | `48 8B 88 68 01 00 00` → `B9 70 00 00 00 90 90` | `0xA4E520` (ehdokkaiden keruu, prio `0x9F0230`) |
| `0xA4E5C5` | `48 2B 88 60 01 00 00` → 7× `90` | |

Ennalleen jäävät (tyhjä vektori = offline-käytös säilyy):
- `0xA734D8` ja `0xA73B1B`: `0xA73430`, purku jos ≤1 ja sessiossa. Meillä InSession on natiivisti epätosi.
- `0x7C4580`: `chr+0x9A9 = clamp(n−2,1,3)`, kutsuja `0x68CFA0`.
- `0x9B2D00`, `0x89B8D0`, `0x781340`, `0xA2E1B0`, `0x6DDC70`, `0x90D5C0`, `0xA77BB0`, `0x67A4B0`.

### 1d. Muut SO:n väärennökset ja meidän valinta

- **`0x832050()` = `u8 GameMan(0x3D5C0D0)+0xBA6`.** SO:n kutsukohdat `0x76F10B`, `0xF0E6B7`, `0x89A689`, `0xA4CF79` valitsevat sillä tallennetun karttatilan pankin (`'OBJ '`/`'CHA '`-blobit) [VAR kulku, ARV merkitys]. **Ei patchata:** kumpikin pelaa omaa maailmaansa.
- **`0x6BE270()` (jäsenmäärä > 0) @ `0x6BC504`** (`E8 67 1D 00 00`). Se vartioi `0x6BC1B0`:ssa tyypin 45 (6 B) objektitapahtuman lähetystä `0x8DB7E0`:lla, ja vastaanottaja on `0x6BEBB0` → `0x8D8750`. **Vain vaihe 4:** TRUE + tyyppi 45 valkolistalle.
- **Valemuuttujat koodissa:** `NATIVE_ROLE: AtomicU8` (0 pois, 1 host, 2 client) ja `NATIVE_N: AtomicU32` (= 2). Patchien vakiot lasketaan näistä aktivoinnissa.

---

## 2. Pakettien reititys

### 2.1 Viisi sisääntulohookia (prologi relokoidaan kuten `observe.rs`:ssä) [VAR prologit]

| Funktio | Signatuuri | Prologi (relokoidaan) | Natiivitilassa |
|---|---|---|---|
| `0x8E7210` broadcast | `u8 (mgr, u32 type, u8* data, u32 size)` | `48 89 5C 24 10` | valkolista: jonoon kaikille peereille, paluu 1 |
| `0x8E7310` isännälle | `u8 (mgr, type, data, size)` | `48 89 6C 24 10` | client: isäntäpeerille, paluu 1 |
| `0x8E6FA0` jäsenelle | `u8 (mgr, Member* m, type, data, size)` | `48 8B C4 4C 89 48 20` (7 B, sisältää `mov rax,rsp`) | peer jäsenestä (alla), paluu 1 |
| `0x8E6B50` recv jäseneltä | `i32 (mgr, Member* m, type, u8* buf, u32 cap)` | `48 89 5C 24 08` | `m` valevektorissa → peerin jonosta |
| `0x8E6D20` recv isännältä | `i32 (mgr, type, buf, cap)` | `48 89 6C 24 10` | client: isäntäpeerin jonosta |

Säännöt:
- **Recv:** yksi paketti per kutsu. Paluuarvo on kopioitu pituus, tai 0 kun jono on tyhjä. Paketti, joka on isompi kuin `cap`, hylätään ja lasketaan (`too_big`), ja hook kokeilee seuraavaa. NetAI-kutsujat ajavat `while(recv())`-silmukoita, joten tyhjän jonon on palautettava 0.
- Hookit ajetaan pelisäikeessä. Jono on lukittu (`parking_lot`/`std` Mutex, `try_lock`, ja epäonnistunut lukitus = 0). Hookit eivät saa paniikoida.
- **Jäsenestä peeriksi:**
  - osoitin välillä `FAKE_MEMBERS[0..N−1]` → indeksi `(m − base)/56`;
  - tyhjä jäsen (`*(u64*)m == 0`, esim. `0xA29DD0`:n tulos tyypille 23) → ainoa peer.
  - Yli kahdelle pelaajalle ohjaa kutsu `0xA1DFC1` (`E8 0A BE 00 00` → `0xA29DD0(chr, &member)`) kääreeseen, joka tallentaa `rcx` = vastaanottajan chr globaaliin `LAST23_CHR`. `0x8E6FA0`-hook mappaa chr:n → peer (`native.rs`:n remote-chr).
- **Oikea jäsen** (`*(u64*)m != 0`) ja tyypit valkolistan ulkopuolella → alkuperäinen koodi.
- **observe.rs:** se hookkaa jo `0x8E7210` ja `0x8E6B50`. Natiivitilan hookit sisältävät samat laskurit, ja `observe` jättää nämä kaksi asentamatta, kun natiivitila on käännetty mukaan. Muut observe-hookit (IsHost-funktiot, NetAI-funktioiden prologit) eivät mene päällekkäin tämän spekin kohtien kanssa.

### 2.2 Jäsenvektoria iteroivat vastaanotot: valevektori [VAR]

Varaa `alloc_near_pub(base+0xA1B000)`-sivulta:
- `FAKE_S` (0x170 B nollaa); kirjoita `FAKE_S+0x160 = &FAKE_MEMBERS[0]` ja `FAKE_S+0x168 = &FAKE_MEMBERS[N−1]`, eli yksi alkio per peer (N−1 = 1);
- `FAKE_MEMBERS` (4 × 56 B nollaa).

Pelille ei anneta muuta kuin nämä kaksi kenttää. Silmukoissa jäsenosoitin menee vain `0x8E6B50`:lle.

| Kohta | Alkup. | Uusi | Funktio / tyyppi |
|---|---|---|---|
| `0xA1AFB1` | `48 8B 35 30 99 37 03` (mov rsi,[S]) | `48 8D 35 <rel32→FAKE_S>` (lea rsi) | `0xA1AF40`, 70 (cap `0x5700`) |
| `0xA1AFF4` | `48 8B 35 ED 98 37 03` (sama, assert-polku) | `48 8D 35 <rel32>` | |
| `0xA1BE56` | `48 8B 1D 8B 8A 37 03` (mov rbx,[S]) | `48 8D 1D <rel32>` | `0xA1BDE0`, 4 (cap `0x100`) |
| `0xA1BE91` | `48 8B 1D 50 8A 37 03` (assert-polku) | `48 8D 1D <rel32>` | |
| `0xA1D936` | `E8 D5 66 EC FF` (call `0x8E4010` = `S+0x158`) | `E8 <rel32→stub>`, stub = `48 8D 05 <rel32→FAKE_S+0x158> C3` | `0xA1D8D0`, 71 (cap `0xD00`) |

`rel32 = kohde − (kohta + 7)` (lea) tai `kohde − (kohta + 5)` (call). Rekisteri ladataan näissä vain `+0x160/+0x168`-lukuja varten, ja se ylikirjoitetaan heti: `0xA1B007/0xA1B014` ja `0xA1BE98/0xA1BE9F`.

**Ei tarvitse patchata:**
- `0xA1CED0`:n isäntäsilmukka `0xA1D040…0xA1D134`: se lukee loppuehdon joka kierroksella `[rbp+168h]`:sta ja kutsuu `0xA4A340`:aa, joka vertaa jäsenen `vtbl+88`:aa. Korvataan §4:n suoralla syötöllä.
- Tyyppi 23 isännältä tulee `0x8E6D20`:n kautta (`0xA1E075`), ja tyyppi 5 samoin (`0xA1C8D0`). Silmukkaa ei ole.

### 2.3 "Mikä tahansa jäsen" -funktiot (vektori `0x8E4010()+8/+0x10`): korvataan kokonaan

`hook::redirect_function(rva, expected, our_fn)`. Meidän funktio ottaa peerin jonosta yhden tyypin T paketin, jonka pituus on enintään cap, ja kopioi sen `buf`:iin. Paluu 1, jos paketti saatiin, muuten 0. Signatuuri on `u8 (void*, u8* buf[, u32 cap])`.

| RVA | Tyyppi | cap | Prologi (expected) | Kutsuja | Merkitys |
|---|---|---|---|---|---|
| `0x8D77B0` | 21 | `0xD0` | `48 89 5C 24 08 48 89 74 24 10 57` | `0xA4F940` (WCM) | vahinko pelaajaan: `+0` kohde-netId (pelaaja), `+0xC` hyökkääjä-netId → `0xB680C0`/`0xB68FF0` [VAR kulku] |
| `0x8D7320` | 28 | `0x80` | sama | `0xA4F940` | toinen osumarekordi [ARV] |
| `0x8D7290` | 52 | `0x68` | sama | `0xA1B770` | chr-tila entity-id:llä → `0x9F3D70` (lähettäjä ChrIns slot 25 `0xA05060`) [ARV] |
| `0x8D7D70` | 18 | `0x2C` | sama | `0x7FAB00` | [ARV] luoti/emitteri |
| `0x8D8260` | 42 | arg a3 | `48 89 5C 24 08 48 89 6C 24 10` | `0x8975E0` | [ARV] karttaobjekti |
| `0x8D8750` | 45 | 6 | `48 89 5C 24 08 48 89 74 24 10 57` | `0x6BEBB0` | objektitapahtuma (vaihe 4) |

Lähettäjät (kaikki menevät `0x8E7210`/`0x8E7310`:n kautta, joten §2.1 kattaa ne ilman patchia):
- 21: `0x8DA200` (kaikille) ja `0x8DBA20` (isännälle), molemmat `0xB6A040`:stä
- 28: `0x8D9470` ← `0xB6C460`
- 18: `0x8D9E60` ← `0x7FDF00`
- 42: `0x8DA130` ← `0x88E390`
- 45: `0x8DB7E0` ← `0x6BC1B0`
- 52: `0x8D9450` ← `0xA05060`
- 26 (liput): `0x8D9ED0` @ `0x8D9F46`, `{u32 flag, u8 state}` 8 B

### 2.4 Tyypit, kanavat ja jonot (protokolla VERSION 8)

Uusi `Packet::GameNet { seq: u32, ptype: u16, data }`:
- epäluotettava: oma kind, ≤ 1100 B;
- luotettava: `Session::send_bulk` tagilla `GNET` + u16 tyyppi.

**Älä sekoita PlayerRaw-kanavaan:** PlayerSession-saumalla on oma "type 23" (PlayerDebugSession slot 5), joka on eri asia kuin NetAI:n 23.

| Tyyppi | Suunta | Koko | Kanava | Vastaanottojono |
|---|---|---|---|---|
| 4 liike (8 vih./paketti, kiertävä kursori) | omistaja → muut | ≤ 256 | epäluot. | FIFO 32 (EI viimeisin, koska joka paketissa on eri viholliset) |
| 5 HP `{u32 netId, u32 (u16 HP, u16 x)}`×10 | host → client | 80 | epäluot. | FIFO 16 |
| 70 käytös | omistaja → muut | ≤ `0x5700` | luot. | FIFO 16 |
| 71 action `{u32 netId, 48 B}`×n | omistaja → muut | ≤ `0xD00` | luot. | FIFO 16 |
| 22 ehdokkaat `{i32 n; u32 (netId \| prio<<16)[n]}` | client → host | ≤ `0x1000` | luot. | FIFO 4 (§4) |
| 23 omistus `{i32 n; u32 netId[n]}` | host → client | ≤ `0x1000` | luot. | FIFO 4 |
| 21, 28, 52, 18, 42 | kumpi tahansa | taulukko 2.3 | luot. | FIFO 32 |

Ei valkolistalla:
- 26 (liput): world.rs hoitaa, eikä kaksoissovellusta haluta.
- 41/55 (liittymissnapshot).
- 76 ja kaikki sessiomanagerin sisäiset tyypit.

Lokissa joka 10 s per tyyppi: tx/rx/drop/too_big.

---

## 3. "Jäsen", playerNo ja etäpelaaja

**Jäsenarvo pelille** on `FAKE_MEMBERS[i]`: 56 B nollia, eli tyhjä `SessionMember` (`+0` osoitin = 0) [VAR rakenne]:
- `0x1BD9510` (valid) = epätosi;
- `0x1BD9480` (self) = epätosi;
- `0x1BD9010` (yhtäsuuruus) = epätosi;
- `0x1BD8F80` (dtor) on null-turvallinen [ARV].

Valejäsen ei siis koskaan osu pelin omiin jäsenpolkuihin. Ainoat käyttäjät ovat §2.2:n silmukat, jotka antavat sen `0x8E6B50`:lle (meidän hook). playerNo:ta ja PGD-slottia **ei** tarvita jäsenenä.

**Omistuksen identiteetti on pelaaja-chr:n kahva `u32 chr+8`** [VAR]:
- isännän tietueet `T+0x368` (lista), avaimena `*(*(WCM+0x88)+8)` itselle ja `remoteChr+8` kaverille;
- `0xA1D550` poistaa tietueet, joiden kahvaa `0xA4A050(WCM, h)` ei enää löydä;
- `0xA1DD00` lähettää 23:n vain chr:lle, jolla `vtbl[53]() != 0` (`call [rdx+1A8h]` @ `0xA1DFAF`).

**Pidetään meidän natiivi etäpelaaja** (native.rs N1: `0xA49460`-ketju, PGD-slotti, playerNo 1..5, recv-sessio `chr+0x2038`, lähetystee):
- `0xA7A840` on sama luontiketju + `chrType` + `0x9F6320`-warp. Se ei tuo mitään uutta, joten se on valinnainen.
- Pelaajapaketit 1/20/37/38/60/61 kulkevat edelleen PlayerSession-saumassa.
- **Vaatimus:** isännällä on oltava kaverin natiivi etäpelaaja samassa blokissa. Muuten §4:n syöttö ei löydä kahvaa, eikä clientin vihollisille tule omistusta. Ks. pudotussääntö §4.

**Pelaajien netId** on `(*(*(WCM+0x10)+0x18) << 11) | playerNo` (`0xA4CE60`, `0xA2A110` = `*(PGD+0x10)`) [VAR]:
- Meillä kummankin oma pelaaja ja etäpelaaja ovat eri playerNo:illa, joten tyyppien 21/28 kohde- ja hyökkääjäkentät pitää kääntää.
- Lähettäjän hook kirjoittaa pelaaja-netId:t muotoon `0xFFF0 | 0` = "lähettäjä itse" tai `0xFFF0 | 1` = "vastaanottaja".
- Vastaanottaja mappaa ne omaan remoteen ja omaan pelaajaansa. Kentät: 21: `+0x00` ja `+0x0C` (u16). 28: `+0x00` [ARV].
- Vihollis-netId = `(blokkislotti << 11) | indeksi`, jossa slotti tulee `0x82B130`:sta (karttatunnuksesta) [VAR]. Oletus on, että se on sama molemmilla koneilla [ARV]. **Pakollinen tarkistus aktivoinnissa (§7):** vaihdetaan `{entityId, netId}`-lista samassa blokissa. Jos yksikin poikkeaa, natiivitila ei aktivoidu (käytös 70 on muuttuvamittainen, eikä netId:tä voi siinä luotettavasti kääntää).

**Yli 2 pelaajaa:** `N` = 1 + peerit. Silloin `B9 <56·N>`-vakio, FAKE-vektorin pituus ja `LAST23_CHR`-mappaus (§2.1) muuttuvat. Ensimmäinen versio tukee vain N = 2.

---

## 4. Vihollisten omistus natiivisti [VAR kulku]

**Taulu** `T = *(WCM+0x630)`, lohko `B = T+0x10+8·(netId>>11)`, omistus `B+0x18 i16[]` (−1 = ei täällä).

**Ehdokkaat (molemmat, joka ruutu):** `0xA4E520` (n > 1) lisää jokaisen aktiivisen vihollisen `0xA1ABC0(T, netId, prio = 0x9F0230(chr))`:llä. Prio lasketaan etäisyydestä omaan pelaajaan. Lista on `T+0x380..0x388`.

**Client:**
1. `0xA1CED0` IsHost = FALSE → ajastin `T+0x3A4`/`+0x3A0` → `0x8E7310(22)` → hook → host.
2. Vastaanotto: `0xA1DD00` IsHost = FALSE → `while (0x8E6D20(23))`. Jokaiselle lohkolle nollaus (`0xA19A50` → −1), sitten listatut omiksi (`0xA19B40` → 1).

**Host:**
1. `0xA1CED0` IsHost = TRUE: oma lista → `rec = 0xA1D7E0(T, *(*(WCM+0x88)+8))`, `0xA1C6C0(rec, n, list)`. Jäsensilmukka on tyhjä.
2. **Meidän syöttö** (FrameBegin, pelisäie) jokaiselle jonossa olevalle 22:lle:
   - tarkista, että `len == 4·(n+1)` ja `0 ≤ n ≤ 1023`;
   - `h = u32 remoteChr+8`;
   - `rec = 0xA1D7E0(T, h)` (rcx = T, edx = h, palauttaa tietueen; luo, jos puuttuu);
   - `0xA1C6C0(rec, n, &payload[4])` (rcx = rec, edx = n, r8 = lista; korvaa tietueen listan).
   
   Kutsuistus on sama kuin pelin omassa `0xA1D0D7…0xA1D0F8`:ssa [VAR].
3. `0xA1DD00`, ajastin `T+0x39C`/`+0x398`:
   - `0xA1D550` ratkaisee ristiriidat (`0xA1C590`): suurempi prio voittaa, tasapelissä se, jolla on vähemmän;
   - oma tietue → `0xA19B40` paikallisesti;
   - muut → `0xA29DD0` (tyhjä jäsen) → `0xA1D240` → `0x8E6FA0(23)` → hook → client.

**Kummallakin** chr-silmukka (`0xA4DC38` = 2) jättää `forceLocal`in pois. Slot 37 `0xA14B50` valitsee Com-AI:n, jos `0xA4A650` (omistettu täällä) on tosi, muuten NetAI:n. Omistaja lähettää 4/70/71, ja muut syöttävät ne tauluun pelin omilla recv-funktioilla (`0xA1BDE0` liput `0x01`, `0xA1AF40` `0x04`, `0xA1D8D0` `0x40`).

**HP natiivisti:**
- host: `0xA4B830` → `0xA1C810`/`0xA1CC20` (u16 HP `0x5A9C80`, u16 `0x9EEBE0`, likaisuus `0x20`) → tyyppi 5;
- client: `0xA1C860` → taulu (`0x10`) → `0xA4B830`: `0xBD64E0(SetHP)`, `0x9F6950`, ja jos `0xBD5D40` (kuollut) → `0x9EE7E0(chr,0)`. Sentinel `0xF987` = ei arvoa.

**Pudotussääntö (pakollinen):** natiivitila keskeytetään kummallakin (patchit pois → `forceLocal` takaisin, kaikki viholliset paikallisiksi), jos jokin seuraavista toteutuu:
- kaveri on eri blokissa;
- isännällä ei ole kaverin remotea;
- client ei ole saanut 23:a 5 s:iin aktivoinnin jälkeen;
- netId-tarkistus epäonnistuu.

Muuten clientin viholliset jäätyisivät NetAI:hin ilman dataa. Huom [ARV]: isännän viholliset, joita kukaan ei pyydä, ovat NetAI:ssa ilman dataa (jäässä). Se on DS3:n natiivi käytös kaukana kaikista pelaajista. Ensimmäiseen jakoon asti (ajastin `T+0x398`) kaikki ovat −1. Loki näyttää, kuinka pitkä tämä aika on.

**Kytketään pois natiivitilassa:**
- `enemies.rs`, koko tila 4:
  - slot 37 -vtable-kopio (`netai::engage`) ja `switch_to_net`-pakotus;
  - `netai_step`-tietuekirjoitus ja isännän tietuelähetys;
  - HP/kuolema-auktoriteetti ja kumulatiivinen vahinko;
  - lethal hold, `throw_window`;
  - UpdateCoords-pakotus vihollisille, tilat 2/3, CompletelyNoMove.
- `activation.rs`: pois aluksi. Clientin lähellä olevat viholliset ovat clientin omistamia ja päivittyvät clientin koneella. Isännän NetAI-kopioiden päivitystiheys on vain kosmeettinen, joten kokeile päälle vasta vaiheen 2 jälkeen.
- `death.rs` P3 `BULK_DEAD_LIST`: pois, koska HP tulee tyypistä 5.

**Pidetään:**
- native.rs (remote, `follow_native`-sijainti);
- world.rs (liput, liittymissnapshot);
- lepo- ja Unseen Aid -säännöt;
- UI.

---

## 5. EMEVD/ESD-ehdot

**`0x69D310`** on rekisteröity `0x6A3A20`:ssa (`lea rcx, 0x69D310` @ `0x6A3A98`), tiedostossa `SprjLuaEventScriptImitation.cpp`. Se on tapahtumaskriptin C++-versio, joka käyttää id:itä 4000, 4090 ja 4067 [VAR].

Sen kuusi kutsua [VAR tavut]:

| Kohta | Alkup. | Funktio |
|---|---|---|
| `0x69D44C` | `E8 9F D7 FE FF` | `0x68ABF0` = tyyppi == 0 |
| `0x69D458` | `E8 03 D6 FE FF` | `0x68AA60` == 8 |
| `0x69D464` | `E8 27 DC FE FF` | `0x68B090` == 1 |
| `0x69D46F` | `E8 DC D4 FE FF` | `0x68A950` == 2 |
| `0x69D47A` | `E8 01 D7 FE FF` | `0x68AB80` == 12 |
| `0x69D491` | `E8 EA D5 FE FF` | `0x68AA80` = `0x8E4790` = IsHost |

Arvo luetaan `0x6D5C10`:lla: pelaajan entiteetti `dword_143AFE450` → `0x9EEEC0` = **pelaajan chr-tyyppi** (0 elävä, 1 valkoinen haamu, 2 musta, 8/12 [ARV]).
- Tyypit 0 tai 8 ja isäntä → kuolema/paluu-haara 4000 ja ilmoitus jäsenille (`0x8E1FC0`, `0x8DA150` → type 29).
- Tyyppi 1 → oma haara.

SO pakottaa kaikki kuusi muotoon `xor al,al`, ja lisäksi `0x6A2F8E` `84 C0` → `30 C0` (callbackin `0x6A2EA0` ehto `0x68AAC0`).

**Meille: EI patchata.** Paikallisen pelaajan chr-tyyppi pysyy 0:na, eikä IsHostia väärennetä globaalisti, joten skripti näkee täsmälleen offline-arvot. Patch katkaisisi tyypin 0 normaalin haaran [ARV]. Jos myöhemmin halutaan SO:n käytös, patchit ovat taulukossa (FALSE).

**`0x8522B0` ei ole ESD-env** [VAR]. Se tarkistaa tavaran käytön ehdot:
- `a2` = tavara-id, haku `0x1410BF630` = goods-param;
- kutsujat `0x84EE44`, `0x84EECD`, `0x84EF26`.

SO vastaa siinä omille online-tavaroilleen 60–67 (jotka se lisää param-hookilla) ja tavaralle 3980 (sallittu vain, kun SO-sessio ei ole päällä). **Meille ei tarvita** (ei omia online-tavaroita).

---

## 6. Moninpeliskaalaus `0xC04EA4` / `0xC04FFA`

**Mitä funktiot palauttavat** [VAR kulku, ARV nimet]:
- `0xC01940(mod)` (max HP:n kerroin) = Π aktiivisista SpEffecteistä `0xC04E30(e)` × `0xBFB0E0(mod,300)`.
- `0xBFD500` vastaavasti `0xC04FB0`:lla ja 302:lla (max stamina = posture).
- `0xC04E30(e)` = `row+0x10` (maxHpRate). Jos `row+0x168 & 0x40` (NG+-korjattava efekti), arvo kerrotaan `ClearCountCorrect`-rivin `+0x00`:lla.
  - Rivi tulee `0x10B8D10`:stä: id = clamp(`GameDataMan+0x70` NG-kierros, 0..7), +100 jos `0xA8E120()`.
- `0xC04FB0` käyttää `row+0x18`:aa ja korjausrivin `+0x08`:aa.
- Koskee kaikkia chr:iä, joilla on NG-skaalausefekti (viholliset ja bossit). Pelaajilla ei ole [ARV].

**SO** lisää ladattuun korjauskertoimeen `+0.3·n`, kun n > 1. Kahdella pelaajalla NG0:ssa kerroin on siis 1.0 → 1.6 eli +60 % (so.asm `loc_18004D7D3`).

**Meidän patch** (molemmilla koneilla samat arvot, koska HP-synkka 5 on absoluuttinen u16):
- `0xC04EA4`: `F3 0F 10 09 0F 57 C0` (movss xmm1,[rcx]; xorps xmm0,xmm0; 7 B) → `jmp stub` + 2× `90`.
  Stub: `movss xmm1,[rcx]; mulss xmm1,[rip+HP_MULT]; xorps xmm0,xmm0; jmp 0xC04EAB`.
- `0xC04FFA`: `F3 0F 10 49 08` (5 B) → `jmp stub2`.
  Stub2: `movss xmm1,[rcx+8]; mulss xmm1,[rip+POSTURE_MULT]; jmp 0xC04FFF`.
- `HP_MULT` / `POSTURE_MULT` ovat f32 stub-sivulla, oletus 1.0. Tavoite "+35 %": 1.35.
- **Bossit +100 %** (kerroin 2.0) vaativat chr:n, mitä tämä kohta ei näe. Hookkaa `0xC01940`:n sisääntulo (`mod` = rcx) ja vertaa `mod`:ia boss-chr:n SpEffect-moduuliin. Boss-chr saadaan HP-mittarista (RE-FINDINGS-2 §4), ja moduuli on [ARV] `*(boss+0x11C8)`. Ensimmäisellä ajolla lokita `boss+0x11C8` ja `mod`, jotta osuma varmistuu. Osumassa kerroin asetetaan säikeen globaaliin, jota stub lukee.
- **Pelin oma vaihtoehto:** `HP_MULT = 1 + 0.3·n/f` (SO-tyyli, additiivinen).
- Max HP lasketaan uudelleen vasta, kun efektit tai statit päivittyvät. Kirjoita kertoimet ennen alueen latausta, tai pakota uudelleenlaskenta [ARV].

---

## 7. Riskit, testijärjestys ja loki

**Vaihe 0: havaintotila ensin (observe.rs, nykyinen build, offline ja nykyinen co-op).** Katso lokista:
1. `observe: session …`: `S+0x184` (ei 3/6), `S+0x18`, jäseniä 0, `FSM+0xDC`, **`GameMan+0xBA6`**. Jos arvo ≠ 0, `0xA4BA93`-patch tarvitaan clientillä (§1b).
2. `observe: calls …`: `netai send 4/70/71/22`, `owner list 23` ja `recv 4/70/5` kasvavat noin 600 / 10 s. Tämä varmistaa, että `0xA1D2D0` ajetaan offline joka ruutu (kutsuja `0xA509F0`). Jos ne ovat 0, koko NetAI-polku ei käy, ja spekki pysähtyy tähän.
3. `observe: send/recv types …`: mitkä tyypit peli jo nyt yrittää lähettää offline broadcastilla. Odotus [ARV]: 26 (liput), 21/28 osumissa, 52 ja 18. Nämä kulkevat §2.1:llä ilman lisäpatcheja.
4. `observe: callers …`: kutsujien RVA:t, joita verrataan §1:n ja §2.3:n listoihin. Tuntematon kutsuja = uusi kohta.

**Vaihe 1: kuljetus ilman patcheja.** Hookit asennetaan tyhjällä valkolistalla, ja vain laskurit ovat päällä. Peli ei saa muuttua mitenkään. Loki: `nativenet: hooks 5/5, fake S at 0x…, replaced 0/6`.

**Vaihe 2: viholliset** (valkolista 4/5/22/23/70/71, §1a–c, §2.2, §4). Edellytykset:
- molemmat samassa blokissa;
- remote olemassa kummallakin;
- `netId check: K/K match` (entityId → netId, `0xA4CE60`).

Loki:
- `nativenet: patches role=host|client applied X/X` (jokainen `patch_code` true);
- 10 s välein `tx/rx` per tyyppi;
- `host 22 from peer: n cand` (host) ja `23 rx: n owned` (client);
- `owners: Com A / NetAI B / owned-here C` (laske `chr+0x58 == chr+0x2000` vs `+0x2008`, ja `0xA4A650`);
- ajastimet `T+0x398`, `T+0x3A0` kerran.

Onnistuminen:
- viholliset liikkuvat ja hyökkäävät kummallakin;
- kaverin lähellä olevat ovat clientin omistamia (`owned-here` clientillä > 0);
- omistus siirtyy, kun pelaajat vaihtavat paikkaa;
- isännän HP:t näkyvät clientillä (type 5);
- `too_big = 0`, ja recv-jonot eivät kasva.

**Vaihe 3: vahinko ja pelaajan pienet tilat** (21/28/52/18/42; netId-käännös §3). Onnistuminen: isännän vihollisen osuma kaveriin laskee kaverin HP:ta hänen koneellaan kerran, ei kahdesti eikä nollaa kertaa.

**Vaihe 4 (valinnainen):** objektitapahtumat 45 + `0x6BC504`.
**Vaihe 5 (valinnainen):** SpEffect-synkka `0x9E9F53` (`E8 C8 E3 00 00` → `0x9F8320`, 11 argumenttia). Vaatii vielä argumenttien 5–11 RE:n. `0x9E9D10` jättää ei-omistettujen vihollisten efektit paikallisesti väliin [VAR].
**Vaihe 6:** skaalaus §6.

**Palautus:** asetus pois, linkki alas, `invalidate_world` tai eri blokki → kaikki `patch_code`-kohdat takaisin alkuperäisiin tavuihin, jonot tyhjiksi ja enemies.rs:n vanha tila päälle. Lataus ei vaadi patchien poistoa, mutta jonot tyhjennetään ja netId-tarkistus tehdään uudelleen.

**Riskit:**
1. **netId-ero koneiden välillä** → tietueet väärille vihollisille. Torjuntana tarkistus ennen aktivointia.
2. **Jäätyneet viholliset** (ei omistusta) → pudotussääntö §4. Lisäksi isännän kaukana olevat jäävät NetAI:hin.
3. **Kaksoisjärjestelmät** (enemies.rs ja natiivi yhtä aikaa) → kova lukko: natiivi aktivoituu vain, kun vihollistila on vaihdettu "natiivi"-arvoon, joka poistaa tilan 4 kokonaan.
4. **Säikeet:** jos FD4 ajaa `0xA509F0`:n työsäikeessä, hookien lukitus ei saa jumittaa (`try_lock`).
5. **Muistin elinikä:** `FAKE_S` ja stubit vuotavat tarkoituksella, eikä niitä koskaan vapauteta.
6. **Pelaaja-netId:t** paketeissa 21/28 (§3). Kentät 28:ssa ovat [ARV].
7. **Kaistanleveys:** 70 voi olla 22 KB; jaa bulkkikanavalla. 4:n epäluotettava häviö näkyy hetkellisenä nykimisenä, ja NetAI:n snap-säännöt korjaavat sen.
8. **Luotettavuus:** SO lähetti kaiken käytännössä epäluotettavana ja "viimeisin per tyyppi" -postilaatikossa. Meillä 22/23/70/71/21 ovat luotettavia ja jonossa (RE-SEKIRO-ONLINE §8).

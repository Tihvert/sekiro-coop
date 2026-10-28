# SPEC: kuolemarangaistus ja vihollisskaalaus ERSC:n tapaan (Sekiro 1.06, coop-dll) – 2026-10-23

Toteutusspeksi kahdelle ominaisuudelle nykyisellä mallilla (oma UDP, isäntä on vihollisten
HP-auktoriteetti, pelin nettisessio pois). Pohjana RE-ERSC.md §3/§4.2/§6, RE-SEKIRO-ONLINE.md §4,
RE-BACKSTAB.md, RE-DEATH-RESET.md §1–§3 ja enemies.rs.
- IDA-kanta `F:\Temp\re_debuff\db.i64` (kopio `quick_re7.i64`), ajuri `F:\Temp\re_debuff\job.py`,
  tulokset `F:\Temp\re_debuff\o1 … o13` (`summary.txt` + `f_<ea>.c`), param-indeksikartta `pmap2.txt`.
- **[VAR]** = luettu dekompiloinnista/tavuista, **[ARV]** = päätelmä, **[LIVE]** = varmistettava ajossa.
- RVA = IDA-osoite − 0x140000000. Peliä ei ajettu, repon koodiin ei koskettu.

## 0. Lyhyesti ja valinnat

- **Max HP ja posture lasketaan joka ruutu uudelleen** perusarvosta ja SpEffecteistä
  (`0xBD6F40`, kutsuja chr-päivitys `0xA02A10`). Perusarvo on ChrDataModulessa:
  `+0x138` = perus-max-HP ja `+0x150` = perus-max-posture. [VAR]
- **Valinta 1 (rangaistus), vaihe 1:** pinot kirjoitetaan **paikallisen pelaajan perusarvoon**
  (`+0x138`/`+0x150`) joka ruutu, ei SpEffectiä. Ei paramimuutoksia, ei hookkia, ei tallennusvaikutusta.
  Vaihe 2 (valinnainen): oma SpEffect-rivi (ikoni + hyökkäys−), §2.4.
- **Valinta 2 (skaalaus):** vaihtoehto **(a′)**, eli `+0x138`/`+0x150` kirjoitetaan **jokaiselle
  viholliselle molemmilla koneilla** NpcParam-arvosta × kerroin. NpcParam-taulua ei muokata.
  Isäntä lähettää kertoimet, jolloin max HP on molemmissa sama. Lisäksi tarvitaan clamp-suoja
  enemies.rs:ään (§3.5). Vaihtoehdot (a) NpcParam get_mut, (b) 0xC04EA4/0xC04FFA ja (c) SpEffect
  hylätään, perustelut §3.1.
- **Varoitus vendor-crateen:** `SoloParam`-indeksit ovat siirtyneet kahdella jossain välillä 22…75.
  Peli käyttää MultiPlayCorrectionParamille indeksiä 78 (vendor sanoo 76) ja ClearCountCorrectParamille
  88 (vendor 86). NpcParam 6 ja SpEffectParam 15 täsmäävät (`NpcParam.cpp`-merkkijono `0x10C6750`:ssa,
  `0x10CEC30` → 15). [VAR] **Älä käytä `get::<P>`:tä muille kuin 6/15** ilman ajonaikaista
  `struct_name()`-tarkistusta (release-buildissa tarkistusta ei ole).

## 1. Yhteiset rakenteet [VAR ellei toisin merkitty]

ChrIns: `+0x68` NpcParam-id (`game.rs` CHR_CHARACTER_ID), `+0x2010` → ParamRef `{i32 id, u64 row @+8}`
(EnemyIns-init `0xA11F60`), `+0x11D0` SpEffect-lista (ensimmäinen solmu `+0x08`), `+0x1FF8` moduulit,
data-moduuli = `*(moduulit+0x18)` (SprjChrDataModule, vtable `0x2A8BE18`).

| data+ | kenttä | kirjoittaja |
|---|---|---|
| 0x130 / 0x134 | HP / max HP | `0xBD5EA0` (max), HP leikataan ≤ max |
| 0x138 | **perus-max-HP** (vt[2] `0xBD55D0`) | ctor `0xBD4830`, NPC-init `0xBD6AE0` = NpcParam `+0x20`, pelaaja `0xBD6410` (`0xA368F0`, `0x7C5FF0`, `0xD59CE0`), `0xBD6DC0` |
| 0x144 | perus-max-MP (vt[3]) | sama; NpcParam `+0x24` |
| 0x148 / 0x14C | posture / max posture | `0xBD60F0` |
| 0x150 | **perus-max-posture** (vt[4]) | NPC-init = NpcParam `stamina` u16 `+0xF0`; pelaaja PGD `+0x3C` |
| 0x160 | "parannusreferenssi": kasvu → HP += ero (`0xBD4D40`) | `0xBD5EA0` |

**Max HP -kaava `0xBD5EA0(data, heal)`:** `max = max(1, Σ maxHpIncrease(+0x3AC, i16) + base × Π maxHpRate(+0x10))`.
Tulo `0xC01940` kertoo myös tekijän `0xBFB0E0(lista, 300)` (1.0, ellei SpEffect 9600 ja sessio).
Lasketaan vain, jos chr ei ole etäpelaaja (`!0x9F36C0`). Kun heal = 1 ja `+0x160` kasvaa, HP kasvaa
saman verran. Posture `0xBD60F0`: `max = Σ maxSpIncrease(+0x3AE) + base × Π maxStaminaRate(+0x18)`.
`0xBD6F40` ohittaa kaiken, jos chr on kuollut (`0xBD5D40`). Kutsujat:
- per ruutu `0xA02A10(chr, dt)` heal = 1;
- pelaajan perusarvon päivitys `0xA368F0` heal = 0;
- moninpelikorjaus `0x9E9A90`.

**SpEffect-API:**
- **Anto** `0x9E9C00(chr, id, source, u8 0, u8 0, const f32[4]* {1,1,1,1}, f32 1.0, u8 force, i32 0, u8 0) -> u8`.
  Kymmenen argumenttia, kutsukohta `0xBE321E`:
  - pinossa: `+0x20` a5, `+0x28` vec, `+0x30` f32, `+0x38` force, `+0x40` i32, `+0x48` u8;
  - vakio `0x328AE90` = 4 × 1.0f;
  - prologi `48 8B C4 48 89 58 08 48 89 68 10 48 89 70 18 57 48 81 EC 90 00 00 00`.
  - Pelin oma malli (pelaajan spawn `0xA4BD60`): `(chr, id, chr, 0, 0, &ONE4, 1.0, 0, 0, 0)`.
  - Portti `0x9E9D10`. `force = 1` ohittaa sen. Paikallinen pelaaja (`0x9F3430`: `WCM+0x88 == chr`)
    saa efektin, ellei lähde ole etäpelaaja. Vihollinen saa sen vain, jos se on WCM:n vektorissa
    `+0x3130..+0x3138` (`0xA4A600`) tai jos FSM `0x8E44C0` = 0 (offline). Meillä offline → menee läpi [ARV].
- **Poisto** (KORJATTU 2026-10-23: oikea poisto on `0xBFB6D0(lista, id)`; alla kuvattu `0xC00340` vain pitää solmun yhden päivityksen, IMPLEMENTATION-NOTES §15) `0xC00340(*(chr+0x11D0), id)`: merkitsee jokaisen solmun, jonka `+0x58 == id`, asettamalla
  `+0x26 = 0x0101`. Poistuu seuraavassa päivityksessä [ARV].
- **Kysely** `0xBFEFB0(lista, id) -> bool`.
- **Solmu:**
  - `+0x00` f32 jäljellä = rivin `effectEndurance` × 1.0 (`0xC03AF0`);
  - `+0x25` kertaluonteinen, jos 0 ≥ t > −0.967;
  - `+0x1C` liput (maski `0x800C0003` = ei aktiivinen);
  - `+0x58` id, `+0x60` **rivin osoite (välimuisti, luetaan joka laskussa)**, `+0x70` seuraava.
  - `effectEndurance = -1` = pysyvä, kunnes poistetaan [VAR kynnys, ARV ei ajastinta].
  - Riviä muokattaessa muistissa muutos näkyy heti kaikille solmuille, joilla on rivi.
- **SpEffectParam-kentät** (SP_EFFECT_PARAM_ST, koko 0x440):
  - `+0x00` iconId; `+0x08` effectEndurance;
  - `+0x10` maxHpRate; `+0x18` maxStaminaRate (= posture);
  - `+0x48` physicsAttackPowerRate (+`0x4C..0x54` muut elementit); `+0xC8` staminaAttackRate (annettu posture-vahinko);
  - `+0x149` saveCategory (**−1**, muuten tallentuu [ARV DS3]); `+0x15E` stateInfo;
  - `+0x168` bit 6 bGameClearBonus, `+0x16A` bit 1 bCurrHpIndependeMaxHp;
  - `+0x3AC/+0x3AE` max HP/posture +lisäys.
- **SpEffect ei säily latauksen yli** [ARV]: chr luodaan uudelleen. Siksi kaikki tila pidetään modissa
  ja sovelletaan uudelleen.

## 2. Kuolemarangaistus (death_debuffs)

### 2.1 Tila ja säännöt
- `World::death_stacks: u8` (0…5). Asetukset `tuning`: `death_debuff` (päälle/pois), `debuff_hp_pct = 10`,
  `debuff_posture_pct = 10`, `debuff_max = 5`.
- **+1 pino:** `DEATH_COMMITTED` (`base+0x3EA0D74`) 0 → 1 **ja** yhteys päällä. Sama reuna kuin
  world.rs:n `EV_DEATH` (rivi ~387). Resurrect ei laske (ei sitoudu).
- **Nollaus:** vain **oma** lepo: `REST_NOW` (`0x3AFEB9C`) ≠ −1 eikä kaiku (`rest_from_friend_at`, sama ehto
  kuin `EV_REST`-lähetyksessä). Tunnista myös valikkolepo (`0x11C9370` ei käytä postilaatikkoa,
  RE-DEATH-RESET §2.1): `WCM+0x67C` 0 → 1, kun kaverin lepoa ei ole tullut viimeisen 3 s aikana.
  Kaverin peilattu lepo ei nollaa.
- **Pysyvyys:** kirjoita `coop_state.json` (tallennuspaikan id + pinot) muutoksen yhteydessä ja lue se
  käynnistyksessä. Ilman tätä pelin uudelleenkäynnistys poistaa pinot [ARV: ERSC:n Rot säilyy].
- Kaverille `EV_DEBUFF(stacks)` vain overlayn tai nimikilven takia. Etäpelaajan max HP:hen sillä ei ole
  vaikutusta, koska `0xBD5EA0` ohittaa etäpelaajat.

### 2.2 Vaihe 1: perusarvon kirjoitus (suositus)
Joka ruutu pelisäikeessä (`Puppet::on_frame`), kun pääpelaaja `p = *(WCM+0x88)` on olemassa ja elossa:
```
pgd   = *(*(base+0x3D5AAC0) + 0x08)          // PlayerGameData
hp0   = *(i32*)(pgd+0x20)  ; po0 = *(i32*)(pgd+0x3C)   // pelin oma perusarvo (0xA368F0) [VAR]
k     = death_stacks (0 kun ominaisuus pois)
want_hp = max(1, hp0 * (100 - debuff_hp_pct*k) / 100)
want_po = max(1, po0 * (100 - debuff_posture_pct*k) / 100)
d = data(p); if *(d+0x138) != want_hp { *(d+0x138) = want_hp }  ; sama +0x150
```
- Pelaajan PGD:tä ei kirjoiteta koskaan. Se on tallennusrakenne, ja vain data-moduulin kenttä muuttuu.
  Max ja HP:n leikkaus tapahtuvat seuraavassa `0xBD6F40`:ssa automaattisesti.
- Kun pino poistuu (lepo), perusarvo nousee. `+0x160` kasvaa, ja lepo täyttää HP:n muutenkin.
- `0xA368F0` palauttaa perusarvon, kun PGD on likainen (tasonnousu, rukoushelmi). Seuraava ruutu
  korjaa sen. Yhden ruudun pidempi palkki on näkyvä haitta, ei hyödyllinen (heal = 0).
- Tarkista kerran: `vt[440](p) == pgd` (`0xA368F0` käyttää virtuaalikutsua). **[LIVE]** loki.
- Lokit: `debuff: stacks {k} (death)`, `debuff: cleared (own rest)`,
  `debuff: base hp {hp0}->{want_hp} posture {po0}->{want_po}` (vain muutoksessa).

### 2.3 Miksi ei SpEffectiä vaiheessa 1
Oma rivi vaatii taulun laajennuksen (§2.4). Olemassa olevan rivin uudelleenkäyttö (`get_mut`) on
turvallista vain, jos rivin tiedetään olevan käyttämätön. Sitä ei voi tietää staattisesti, koska
regulation, EMEVD ja TAE ovat salatuissa arkistoissa. HP ja posture onnistuvat ilman riviä.

### 2.4 Vaihe 2 (valinnainen): oma SpEffect-rivi, ikoni ja hyökkäys−
- Lisää SpEffectParamiin (holder 15) viisi riviä id:illä `2140000001…5`. Tarkista ensin, ettei id:itä ole
  (`find_index`). Sama tapa kuin ERSC:n 0x82950 (RE-ERSC §6) [ARV menetelmä].
- **Uusi tiedostopuskuri:**
  - otsikko kopioidaan;
  - row_count `+0x0A` += 5;
  - `RowDescriptor`ien (24 B, alkaen `+0x40`) perään 5 uutta;
  - kaikki descriptorien jälkeinen (nimi, data) siirtyy 120 B. Kaikki file-relatiiviset offsetit +120:
    descriptorien data_offset/name_offset, otsikon struct_name_offset ja data_offset;
  - 5 × 0x440 riviä loppuun;
  - metadata (−0x10) `after_name_offset` uuteen loppuun;
  - id-järjestetty lookup-taulu `{u32 id, u32 index}` 16-tasattuna perään.
  - Pelin oma haku `0x10CEC30` lukee: `count = *(u16*)(f+0x0A)`, taulu = `f + align16(*(u32*)(f-0x10))`,
    64-bittinen data = `*(u64*)(f + 0x48 + 24*idx)`. [VAR]
- Kirjoita `FD4ParamResCap.size` ja sitten `.data` (`*(ParamResCap+0x70)+0x70`). **Vanhaa puskuria ei
  vapauteta**, koska solmujen `+0x60`-osoittimet ja NPC:n ParamRefit osoittavat siihen.
- **Rivien sisältö:**
  - kopio pohjarivistä, jonka id:n antaa ajonaikainen dumppi;
  - `effectEndurance = -1`, `saveCategory = -1`;
  - `maxHpRate = 1 - 0.1k`, `maxStaminaRate = 1 - 0.1k`, `physicsAttackPowerRate` (+ elementit)
    `= 1 - 0.05k`;
  - `iconId` jostain olemassa olevasta debuffista [LIVE: valitse dumpista];
  - stateInfo 0, `bGameClearBonus = 0`.
  - Tällöin §2.2:n perusarvokirjoitus jätetään pois.
- **Anto ja ylläpito:**
  - joka 0.5 s: jos `k > 0` eikä `0xBFEFB0(lista, 2140000000+k)` → poista muut pinorivit `0xC00340`:lla
    ja anna `0x9E9C00(p, id, p, 0, 0, &ONE4, 1.0, 0, 0, 0)`;
  - toistuu automaattisesti latauksen ja kuoleman jälkeen.
- **Riskit:** pointterin vaihto ajon aikana (peli lukee `+0x70`:n joka haulla, vanha puskuri pysyy
  validina) ja muistivuoto (~kopio taulusta, kerran). Debug-buildin `debug_assert_eq!(id, actual_id)`
  vendorissa toimii, kunhan taulu on järjestetty.
- Lokit: `speffect: appended 5 rows (count {old}->{new}, {bytes} B)`, `debuff: applied sp {id}`.

## 3. Vihollisten ja bossien HP-/posture-skaalaus

### 3.1 Vaihtoehdot
| | mekanismi | arvio |
|---|---|---|
| (a) NpcParam `hp`/`stamina` get_mut | luetaan vain init-hetkellä (`0xBD6AE0` ← `0xA11F60`) [VAR] | jo spawnatut eivät muutu; rivi on jaettu kaikkien instanssien kesken; palautus vaatii uudelleenlatauksen. **Hylätty.** |
| (b) `0xC04EA4`/`0xC04FFA` | ClearCount (NG+) -kerroin `0xC04E30/0xC04FB0`: vain SpEffecteille, joilla bGameClearBonus. Rivi `ClearCountCorrectParam[min(GDM+0x70,7) + (lippu 6911 ? 100 : 0)]` (`0x10B8D10`) [VAR] | globaali, ei erota bosseja, keskelle funktiota tuleva hook, ja riippuu siitä, onko vihollisella NG+-SpEffect [ARV]. **Hylätty.** |
| (b2) natiivi MultiPlayCorrection | `0x9E9A90`: NpcParam `+0x208` → param 78 `{client1/2/3 SpEffect, bOverride}`, antaa `0xA01720`:lla, kun `0x8E4730() == 0`; jäsenmäärä `*(GameMan+0xD30)+8` [VAR] | Sekiron arvot ovat tuntemattomia (todennäköisesti −1) [ARV], eikä niitä voi säätää. Vain dumppi. |
| (c) oma SpEffect vihollisille | vaatii §2.4:n rivit ja jokaisen spawnin seurannan | toimii, mutta (a′) tekee saman ilman taulumuutosta |
| **(a′) perusarvo per chr** | `data+0x138/+0x150` = NpcParam × kerroin; peli laskee max:n joka ruutu ja parantaa kasvun verran | välitön, idempotentti, palautus = NpcParam-arvo. **Valittu.** |

### 3.2 Kerroin ja luokittelu
- `n` = pelaajamäärä. 1 + kaveri, kun yhteys on "valmis" (sama ehto kuin NetAI-tilassa); muuten 1.
- `hp_pct(n) = 100 + p_hp × (n−1)`; oletukset ERSC: vihollinen HP 35 / posture 15, boss HP 100 / posture 20,
  vahinko 0.
- **Boss** = NpcParam `ninsatu_num` (`+0x126`, i8, deathblow-määrä) ≥ 2, **tai** entity == oma
  bossipalkin entity (`game::boss_state()`, `GameDataMan+0xC0`), **tai** kaverin `EV_BOSS(1)`-entity.
  - Parametripohjainen osa on deterministinen molemmilla koneilla.
  - Minibossit (2 deathblowta) lasketaan bossiksi. Asetus `miniboss_as_boss`, oletus 1. [ARV kentän merkitys]
- **Kertoimet tulevat isännältä:** uusi luotettava `BULK_SCALE_CFG {u8 ver, u16 e_hp, e_po, b_hp, b_po}`
  yhteyden alussa ja muutoksessa. Client käyttää vain isännän arvoja. Ilman tätä eri asetukset antavat
  eri max HP:n ja käynnistävät clampin (§3.5). Protokollaversio +1.

### 3.3 Sovellus (molemmat koneet, pelisäikeessä)
```
for chr in kaikki ladatut EnemyIns (WCM chr-setit; ei pelaajia, ei kuolleita):
  ref = *(chr+0x2010); row = *(ref+8); if row==0 -> skip
  npc_hp = *(u32*)(row+0x20); npc_po = *(u16*)(row+0xF0)
  boss   = *(i8*)(row+0x126) >= 2 || boss_entities.contains(entity)
  want_hp = npc_hp * hp_pct(n,boss) / 100 ; want_po = npc_po * po_pct(n,boss) / 100
  d = data(chr); cur = *(d+0x138)
  if cur == want_hp -> ok
  elif cur == npc_hp || cur == last_written[chr] -> *(d+0x138) = want_hp ; last_written[chr] = want_hp
  else -> log "scale: skip {entity} base {cur} != npc {npc_hp}" (joku muu muutti; ei kosketa)
  (sama +0x150:lle)
```
- **Ajo:**
  - koko silmukka 1 s välein;
  - lisäksi enemies.rs:n client-polussa jokaiselle viholliselle **ennen HP:n kopiointia** (kevyt
    tarkistus `cur == want`). Isännällä ennen HP:n lukemista lähetykseen.
- **Yhteys katkeaa → n = 1** → `want = npc`. Perusarvo laskee, max laskee, HP leikataan. Täydellä
  HP:lla oleva on edelleen täynnä.
- **Kasvu yhteyden tullessa:** `+0x160` kasvaa → HP += ero (`0xBD4D40`), eli täysi vihollinen pysyy
  täynnä. Isännällä vaurioitunut 700/1000 → 1050/1350 [VAR mekanismi].
- `last_written`: avain `(chr-osoite, entity id)`. Tyhjennetään latauksessa (chr:t luodaan uudelleen).

### 3.4 Miksi max HP on sama molemmilla
- Molemmat lukevat saman NpcParam-rivin (sama regulation) ja käyttävät isännän kertoimia ja
  kokonaislukulaskua → sama perusarvo.
- Max = perus × Π SpEffect-kertoimet. NG+-kerroin riippuu kummankin omasta `GDM+0x70`:stä ja lipusta
  6911, eli **eri NG-kierros antaa eri max:n jo nyt** [VAR kaava, ARV käytännössä]. Liittymisessä pitää
  varoittaa, jos `GDM+0x70` tai lippu 6911 eroaa.
- Client peilaa HP:n isännältä (enemies.rs). Max ei kulje verkossa, eikä sen tarvitse.

### 3.5 Pakollinen clamp-suoja enemies.rs:ään
- Jos clientin max on hetken pienempi kuin isännän HP (skaalaus ei vielä ajettu, luokittelu eroaa,
  NG-ero), peli leikkaa kopioidun HP:n maxiin. Seuraava kierros näkee "paikallisen menetyksen" ja
  lähettää sen vahinkona isännälle. **Tämä on desync- ja tappobugi.**
- Korjaus client-polussa (rivi ~678): lue `max = *(data+0x134)`. Jos `written > max && local == max`,
  menetystä ei lasketa ja `last_written_hp = local`.
- Loki rajoitettuna: `enemies: hp clamp {id} host {s.hp} max {max}` (1/s/vihollinen).
- Lisäksi: `HP mismatch` -diagnostiikkaan max-arvo, muodossa `id:hp/max`.

### 3.6 Riskit
- **NetAI-vihollinen clientillä:** jos `0xA02A10` ei aja NetAI-chr:lle, max ei päivity → HP-palkki
  > 100 % (ei leikkausta). **[LIVE]** loki `scale: {id} max {*(d+0x134)}` 3 s skaalauksen jälkeen.
- Multi-life-bossit: deathblow täyttää HP:n maxiin, joka on jo skaalattu. Kuolema ja HP pysyvät
  isännän auktoriteetilla.
- AllChrRespawn (`vt[18]`) ei kutsu `0xBD6AE0`:aa (ainoa kutsuja `0xA11F60`) [VAR xref]. Perusarvo
  säilyy, ja idempotentti silmukka korjaa joka tapauksessa.
- Perusarvon muut kirjoittajat data-moduulin ulkopuolella haettiin vain alueelta `0xBD0000–0xBE0000`
  [VAR rajattu]. Siksi `skip`-haara.
- Bossipalkki näyttää uuden maxin. Sen HP-% on molemmilla sama.

## 4. Testit (kaksi konetta, sama build)
1. **Yksin:** ei muutosta. Loki `scale: n=1`, ei yhtään `base ... ->` -riviä.
2. **Yhteys päälle Ashina-reunalla:**
   - `scale: applied {N} enemies n=2 (e 135/115, b 200/120)`;
   - tavallisen vihollisen `+0x134` = 1.35 × NpcParam molemmilla;
   - `HP mismatch` tyhjä 10 s;
   - ei `hp clamp` -rivejä 5 s jälkeen.
3. **Lyö clientillä:** isännän HP laskee lyönnin verran (ei 35 % ylimääräistä). Tappo samassa
   kohdassa molemmilla.
4. **Boss (Gyoubu tai Lady Butterfly):** palkki 2 × HP molemmilla; boss-loki `scale: boss {entity}
   ninsatu {k}`; deathblow-pisteet toimivat.
5. **Katkaise yhteys:** max palaa NpcParam-arvoon, ja täysi HP pysyy täytenä.
6. **Kuolema yhteydessä:**
   - `debuff: stacks 1 (death)`;
   - latauksen jälkeen HP-palkki 90 %;
   - kolme kuolemaa → 70 %; viisi → 50 %; kuudes pysyy 50 %:ssa;
   - Resurrect ei lisää pinoa.
7. **Oma lepo:** `debuff: cleared (own rest)` ja palkki täysi. Kaverin lepo ei nollaa omaa pinoa.
8. **Tasonnousu (rukoushelmi) pinojen aikana:** uusi `hp0` näkyy lokissa, ja pino pysyy.
9. **Pelin uudelleenkäynnistys:** pinot palautuvat `coop_state.json`:sta.

## 5. Avoimet [LIVE]
- Ajaako `0xA02A10` NetAI-chr:lle clientillä (§3.6)?
- `vt[440](player) == *(GDM+8)`?
- Dumppi kerran käynnistyksessä:
  - MultiPlayCorrectionParam (holder 78) rivit;
  - NpcParam `+0x208`- ja `ninsatu_num`-jakauma;
  - SpEffect-rivien `iconId`-ehdokkaat vaiheelle 2.
- Lipun 6911 merkitys (ClearCount +100 -rivit: kellodemoni tai Kuron viehe?).

# RE: kuolema, idolilla lepo ja vihollisten reset (Sekiro 1.06)

Staattinen analyysi, IDA 9.3 headless, tietokantakopio `F:\Temp\re_death\db.i64` (kopio `re_throw`-kannasta).
Ajot: `F:\Temp\re_death\job.txt` -> `F:\Temp\re_death\o1 ... o18` (`summary.txt` + `f_<ea>.c`).
Merkinnät: **[VAR]** = luettu dekompiloidusta koodista tai tavuista, **[ARV]** = päätelmä.
RVA:t ovat moduulin kannasta (IDA-osoite − 0x140000000).

## 0. Lyhyesti

- Vihollisten reset tapahtuu kahta eri reittiä. **Lepo** käyttää kevyttä in-place-resetiä
  `AllChrRespawn` (WorldChrMan `+0x67C`, ei latausruutua). **Kuolema** tekee täyden kartan
  uudelleenlatauksen (`0xA8F7E0` → GameMan `+0x10 = 1`). Kuolemassa ei siis ole erillistä
  "reset-askelta", jonka voisi ohittaa: viholliset luodaan uudelleen MSB:stä latauksen aikana. [VAR]
- `REST_REQUEST` (`0x3AFEB98`) yksin **ei resetoi vihollisia**, se vain täydentää pelaajan. Reset
  tarvitsee myös `0x3AFEBA0`:n tai suoran kutsun `0xA4C0F0`. [VAR]
- Kuolemarangaistus (sen/XP, Unseen Aid, lohikäärmetauti) on `CSDeadStep`issä. Se käynnistyy **vain
  pääpelaajasta** (`WorldChrMan+0x88`). Etäpelaajan kuolema ei aja sitä. [VAR]
- Kaverin "Unseen Aid" -ilmoitus tulee lähes varmasti **modin omasta lippusynkasta**: `CSDeadStep`
  kirjoittaa lipun 9505 (Unseen Aid) ja 9510 (tauti leviää), ja `should_sync()` jakaa ne (top 0,
  k = 9). [VAR kirjoitukselle ja säännölle, ARV näytölle]

## 1. Kuolema → respawn -polku (a)

### 1.1 Kuoleman merkintä [VAR]
`0x9EE7E0(chr, bool remote)` (kill):
```c
SetHp(*(*(chr+0x1FF8)+0x18), 0);            // 0xBD64E0, HP = data+0x130
if (!(*(*(chr+0x18)+0x30)+0x8C & 1)) {      // 0x9E5C00
    *(u8*)(chr+0x1A11) |= 0x80;             // DEAD-bitti
    *(u8*)(chr+0x1BF8)  = remote;           // NetworkManipulator antaa 1
    0x9F7070(chr);                          // kuolematila; remote+1 ohittaa 0x9EDA50:n
}
```
`0x9F7070` tyhjentää bitin `0x80` (ja `+0x1A15 &= 0xAF`, `+0x1A16 &= ~2`, `+0x1BF8 = 0`), kun HP > 0 taas.

### 1.2 CSDeadStep käynnistyy vain pääpelaajasta [VAR]
InGame-stepin päivitys `0xA71190` kutsuu `0xA740D0`; jos se on tosi, `0xA72A70` luo `CSDeadStep`in
(vtable `0x2BD5BE8`, ctor `0x11D2B20`, tila-taulu `0x3EA0D80`) kohtaan `inGameStep+0x320`.
```c
bool 0xA740D0(step) {
  if (g_deadLatch /*RVA 0x3EA0D74*/ || step+0x320) return false;
  p = *(WorldChrMan+0x88);                       // VAIN pääpelaaja
  return p && (*(float*)(p+0x212C) > 0 || *(float*)(p+0x2138) >= 0 || 0x9E5C00(*(p+0x18)));
}
```
Kaikki `CSDeadStep`in tilat (`0x11D4110`, `0x11D2EF0`, `0x11D2FA0`, `0x11D3430`) lukevat
`*(WCM+0x88)`. Etäpelaaja (`WCM+0x40`-setin indeksi ≥ 1) ei koskaan ole siellä.

### 1.3 Rangaistus: `0x11D3430` (kutsutaan lambdasta `0x11D5460`) [VAR]
```c
if (EventFlagMan+0x220 == 1) { 0xA8F7E0(); goto done; }   // erikoistila (bank 1)
if (GetFlag(9500)) goto done;                     // 0x84E430: rangaistus ohitetaan
pgd = *(GameDataMan+8); sen = pgd+0x7C; xp = pgd+0x160;
aid = !GetFlag(9501) && (GetFlag(9502) ||
      ((sen||xp) && chance(param630 via pgd+0x278) <= rand));
if (!aid) {                                       // menetys
  sen' = round((1 - Param126[+200]/100) * sen);   // GameSystemCommonParam? tavu +200
  xp'  = round((1 - Param126[+201]/100) * xp);    // tavu +201
  pgd+0x7C = sen'; pgd+0x160 = xp';               // 0x66DA30 / 0x11D4970
  *(u64*)(MenuMan+0x1FA0) = {sen-sen', xp-xp'};   // "menetetty"-näyttö
}
SetEventFlag(9505, aid);                          // 0x6C4520 -> MODIN HOOK NÄKEE TÄMÄN
if (!GetFlag(9511)) {
  if (GetFlag(9512)) { GameMan warp 0x0B000000 / 1102621 ...; spread = 1; }
  else spread = !(chance(param620, pgd+0x148) <= rand);
  SetEventFlag(9510, spread);                     // lohikäärmetaudin leviäminen
  if (pgd+0x148 < 0x7FFFFFFF) pgd+0x148++;        // kuolemalaskuri (taudin todennäköisyys)
}
done: g_deadLatch(0x3EA0D74) = 1; ...
```
Kooditasolla lippua 9505 käyttää vain tämä funktio (imm-haku `0x2521`: yksi osuma). Ilmoituksen
näyttää siis data (EMEVD/ESD), joka reagoi lippuun. [VAR haulle, ARV näytölle]

### 1.4 Respawn idolille = täysi uudelleenlataus: `0xA8F7E0` [VAR]
Kutsujat: EMEVD-pankki 2003 käsky 95 (`0x904EF0`, case 95), ESD-komento 131 (`0x11C63A0`),
`CSDeadStep` (vain erikoistilassa) ja matkavalikko `0x6795F0` (vain erikoistilassa). Tavallisessa
kuolemassa reload tulee siis EMEVD:stä tai ESD:stä [ARV], mutta aina tämän yhden funktion kautta.
```c
if (EventFlagMan+0x220 == 1) for 7 lippua taulusta 0x2A45C80: Set(id, Get(id)); // remnant
byte_3D7C534 = 0;
0x7C3C60(pgd);                    // *(pgd+0x190)+0x0E = 1
GameDataMan+0x78 = 1;             // latauksen jälkeen: 0xA2EE70(player) + HUD (0xA73430)
GameDataMan+0x7B = 1;             // latauksen jälkeen: 0x7A95B0 = tavaroiden täydennys
idol = 0x831940();                // GameMan+0xB50 (>=0) muuten +0xB4C = viimeisin idoli
0x676CE0(EventMan*0x3D55600, &pt, idol);
GameMan+0x1C = pt; +0x20 = -1; +0xBF0 = pt; +0xB4A = 1; +0xC41 = 1;
0x6D7070();                       // GameMan+0x10 = 1 -> KARTAN UUDELLEENLATAUS
```
Latauksessa kaikki chr:t tuhotaan ja luodaan uudelleen, ja kartan EMEVD alustetaan uudelleen
[ARV, sama mekanismi kuin idoli-warpissa `0x68E870`]. Tavalliset viholliset palaavat, koska niitä ei
tallenneta mihinkään (RE-FINDINGS-2 § flag storage). Bossit ja minibossit pysyvät kuolleina
kuolinlippujensa ja EMEVD:n kautta.

Mitä kuoleman reload tekee muuta [VAR funktiot, ARV vaikutukset]:
- Pelaaja palaa elämään. HP/tila nollataan 0xA2EE70:lla (`GDM+0x78`), ja tavarat täydennetään
  0x7A95B0:lla (`GDM+0x7B`). `pgd+0x148`-laskuri kasvaa (taudin leviäminen).
- Kaikki viholliset, NPC:t ja objektit luodaan uudelleen. Bossiareena nollautuu (sumuseinä, boss
  täysillä HP:illä, ellei kuolinlippu ole päällä). EMEVD-tapahtumat käynnistyvät uudelleen.
- Ylösnousemuspallot (Resurrection): ladataan/rangaistaan kuolematilan ja palautuksen kautta, ei
  tämän funktion sisällä [ARV]. "Kuolin/Ylösnouse"-valinta on aiemmin samassa `CSDeadStep`issä
  (kentät `+0x212C`/`+0x2138`, nollaus `0xA26120`).

### 1.5 Ohitetaanko reset kuolemassa?
Koska resetti on itse lataus, pienin "ei resettiä" -muutos on joko (A) olla estämättä latausta ja
palauttaa vihollisten tila elossa olevalta pelaajalta (vain modin logiikkaa), tai (B) korvata
`0xA8F7E0` in-place-elvytyksellä. Vaihtoehdon B ongelmat [ARV]:
- EMEVD/ESD odottaa latausta, ja häivytys on mustana.
- `g_deadLatch` jää arvoon 1, jolloin uutta kuolemaa ei enää käsitellä.
- Toinen blokki vaatii latauksen joka tapauksessa.
- Idolin sijainti pitää selvittää erikseen.

Ks. EHDOTETTU PATCH.

## 2. Lepo (b)

### 2.1 Idolin postilaatikko ja idoliobjekti `0x665B50` [VAR]
`0x76EFF0` siirtää kerran ruudussa B90→B94, B98→B9C ja BA0→BA4, ja palauttaa sitten arvon -1.
Idoliobjekti (oma entity id `this+0x20`):

| luettu | toiminto |
|---|---|
| B94 (`0x76E7B0`) | `0x76EE60(id,0)`, `0x8C5C70(id)` (idolin aktivointi/löytö) |
| **B9C** (`0x76E7C0`) | **vain** `Rest(player, idolValue)` = `0xA2BE40` (täydennys), ei vihollisia |
| **BA4** (`0x76E7D0`) | SFX ×2, ajastin `this+0x48 = 1.333 s` → nollassa: jos `!(EFM+0x220==1)`: **`0xA4C0F0(WCM)`** (AllChrRespawn), **`0x899500(*(base+0x3D5D600))`** (objektien/esineiden respawn), `0x107AF00(*(base+0x3D94970))` jos `+0x10`; sitten `player.vtbl[234]()` (`+0x750`) |

Kirjoittajat:
- `0x68E5B0` (B90 + B98) ja `0x68E5D0` (B98).
- `0x68E5E0` = Lua `Util_RequestRespawn` (BA0).
- **`0x69B030` = `OnEvent_BonfireRespawn`: B98 + BA0 + `0x7A95B0()`**. Tämä on "täysi lepo".

Valikon oma lepo (ESD talk `0x11C9370`) tekee suoraan `0xA2BE40` + `0xA4C0F0` + `0x899500` ilman
postilaatikkoa. Se ajetaan vain, jos pelaajan chr-tyyppi on 0, 1 tai 8.

**Johtopäätös:** world.rs `apply_rest` kirjoittaa vain `REST_REQUEST`in (B98), joten kaveri saa vain
HP/kurpitsat takaisin, ja **sen viholliset eivät resetoidu** [VAR]. Kirjoita sama idoli-id myös
`0x3AFEBA0`:een, tai kutsu suoraan `0xA4C0F0(WCM)` ja `0x899500(*(base+0x3D5D600))` pelisäikeessä.

### 2.2 AllChrRespawn-tilakone, WCM `+0x67C` [VAR]
- `0xA4C0F0`: `if (wcm+0x67C == 0) wcm+0x67C = 1`. Kutsujat: idoliobjekti, EMEVD 2004[51]
  (`0x9017D0` case 51), ESD-lepo `0x11C9370`, `0xC247D0` ja debug `0xA526B0`.
- WCM-päivitys `0xA4E320`:
  1. Tila 1: odotetaan, että jokainen chr-setti on valmis (`0xA40E60`, setit `wcm+0x30`, 320 B,
     määrä `wcm+0x28`).
  2. Sitten `0x64A7E0` ja jokaiselle setille `0xA40FC0`, sen jälkeen `0xA1B6B0(wcm+0x630)`.
  3. Tila 3 (`EventRespawnBeginWait`) → SprjEventMan `0x661520` ajaa `0x6620B0` (karttatapahtumien
     uudelleenkäynnistys) → 4 → 5.
  4. Tila 5: `0xA412D0` jokaiselle setille → 0.
- `0xA40FC0` (setti): elvytettäville chr:ille (`0x9F2FF0`: `*(vt[47]()+8)+0x150 & 2 == 0`) se
  ilmoittaa `0x919790(*(0x3D6CDC0), entityId)`, kutsuu `vt[18]` (`+0x90`, uudelleenalustus, HP
  täyteen, paikalleen) ja `vt[221]` (`+0x6E8`), ja jokaiselle chr:ille `vt[234]` (`+0x750`). Lopuksi
  generaattorit (`Generator_%02d`).
- Reset koskee **kaikkia ladattuja** chr-settejä, ei vain idolin blokkia [VAR silmukka]. Postilaatikko
  toimii vain, jos idoliobjekti, jolla on sama entity id, on ladattuna (sama kartta) [VAR vertailu,
  ARV lataus]. Jos kaveri on eri blokissa, postilaatikko ei tee mitään. Suora `0xA4C0F0` toimii.
- Tunnistus omalle levolle: lue `wcm+0x67C` ruuduittain (0 → ≠ 0 = reset alkoi), tai hookkaa
  `0xA4C0F0`. Prologi `83 B9 7C 06 00 00 00` (7 tavua, ei RIP-relatiivinen) [VAR].

## 3. "Kaikki kuolleet" -tunnistus (c) [VAR]

| kenttä | merkitys |
|---|---|
| `p = *(*(base+0x3D7A1E0)+0x88)` | pääpelaaja |
| `*(u32*)(*(*(p+0x1FF8)+0x18)+0x130)` | HP (`0x5A9C80`) |
| `u8 p+0x1A11 & 0x80` | kuollut-bitti (kill), nollautuu kun HP > 0 |
| `*(*(p+0x18)+0x30)+0x8C & 1` | kuolematila (`0x9E5C00`) |
| `f32 p+0x212C > 0`, `f32 p+0x2138 >= 0` | elvytysvalinnan ajastimet (Resurrect-ikkuna) |
| **`u32 base+0x3EA0D74`** | **0 = normaali, 1 = kuolema sitoutunut** (rangaistus tehty, idoli-reload tulossa), 2 = InGame-step poistuu. Nollautuu InGame-stepin init/latauksessa (`0xA6D0F0`, `0xA6E370`) |
| etäpelaaja: `chr+0x1A11 & 0x80` + paketin HP | NetworkManipulator `0x7EC250` → `0x9EE7E0(chr,1)` |

Suositus: "oikea kuolema" tarkoittaa, että `0x3EA0D74` muuttuu 0 → 1. Pelkkä HP 0 ei riitä, koska
Resurrect voi vielä elvyttää. Lähetä tämä tapahtumana kaverille. Wipe tarkoittaa, että molemmat
sitoutuvat ennen kuin ensimmäinen on taas elossa maailmassa.

## 4. Unseen Aid ja rangaistus etäpelaajan kuollessa (d)

- Rangaistuksen ja Unseen Aidin tekee `0x11D3430` (§1.3). Laukaisija on `0xA740D0`, joka katsoo
  vain `WCM+0x88`:aa. **Etäpelaajan kuolema ei aja rangaistusta**: kaveri ei menetä sen/XP:tä
  isännän kuollessa, eikä hänen `pgd+0x148`-laskurinsa kasva [VAR]. Polku on siis jo
  "pääpelaaja"-rajattu.
- Ilmoituksen todennäköisin syy: isännän `SetEventFlag(9505, aid)` ja `(9510, spread)` menevät
  modin hookista kaverille. `should_sync(9505)` = true (top 0, k = 9). Kaverin EMEVD/ESD näyttää
  "Unseen Aid" lipun perusteella [ARV, korkea].
- Haitat:
  - 9510 voi levittää lohikäärmetautia kaverin NPC:ille isännän kuolemista [ARV].
  - 9500/9501/9502/9511/9512 (rangaistuksen ohjausliput, kaikki k = 9) kulkeutuisivat kaverille ja
    muuttaisivat **kaverin oman** seuraavan kuoleman rangaistusta [VAR luku, ARV vaikutus].
- Huom: liput 9000–9999 ovat **yksi tietue** (group 0, k = 9). Join-dumppi kopioi tietueen
  kokonaisena, eli myös 95xx-bitit. Myös 98xx (vuorokaudenaika) kuuluu samaan tietueeseen.
- "Aina"-havainto ei selity pelkällä `aid=1`-arvolla. Tarkista lokista, mitkä 95xx-liput ja arvot
  saapuvat isännän kuollessa. Mahdollisesti data reagoi lippukirjoitukseen arvosta riippumatta
  [ARV, ei varmistettu].

## EHDOTETTU PATCH

### P1. Lippusynkan rajaus (pakollinen, pieni, vain modi)
- `world.rs::should_sync`: palauta `false` kaikille `9500..=9599` molempiin suuntiin.
- Join-dumpissa (`shared_ks` / tietuekopio): kun tietueessa on lippu `id/1000 == 9`, säilytä
  vastaanottajan omat bitit 500–599 (maskaa ne, älä kopioi isännän bittejä).
- Riski: pieni. Nämä ovat pelaajakohtaisia, ja natiivi DS3-sääntö jakaisi ne vain siksi, että k = 9.
- Loki: `world: skip death flag {id}` vastaanottopäässä. Isännän kuolema ei enää näytä kaverille
  Unseen Aidia, ja kaverin sen/XP pysyy ennallaan.

### P2. Lepo resetoi myös kaverin viholliset (pieni)
- `world.rs::apply_rest` ja `silent_rest`: kirjoita idoli-id sekä `0x3AFEB98`:aan että
  `0x3AFEBA0`:aan (sama kuin `0x69B030`). Tai, jos olemme eri blokissa tai idolia ei ole ladattuna,
  kutsu pelisäikeessä:
  - `0xA2BE40(player, 100)`
  - `0xA4C0F0(*(base+0x3D7A1E0))`
  - `0x899500(*(base+0x3D5D600))`
  - ja tarvittaessa `0x7A95B0()`.
- Tunnistus omalle levolle on luotettavampi `WCM+0x67C` 0 → 1 kuin `REST_NOW`. Valikkolepo
  (`0x11C9370`) ei kulje postilaatikon kautta [VAR].
- Riskit:
  - `EFM+0x220 == 1` (remnant) estää respawnin: älä lähetä silloin.
  - Kaiku: merkitse vastaanotettu reset, ettei sitä lähetetä takaisin.
- Loki: `wcm+0x67C` käy 1 → 3 → 4 → 5 → 0 kaverilla, ja vihollisten HP on täynnä.

### P3. Yhden pelaajan kuolema ei resetoi maailmaa (vain modin logiikkaa)
Kaverin kuolema toimii jo: kaveri latautuu, ja isännän HP-peili tappaa isännällä kuolleet (corpse-
sääntö sallii elävä → kuollut). Isännän kuolema ei toimi. Korjaus:
1. Isäntä lähettää `DEATH_COMMIT`, kun `0x3EA0D74` muuttuu 0 → 1. Kaveri lähettää omansa samoin.
2. Jos kaveri on elossa (`hp > 0` ja oma `0x3EA0D74 == 0`), kaverista tulee isännän latauksen ajaksi
   väliaikainen auktoriteetti. Kaveri palauttaa omat vihollisensa paikalliselle AI:lle (slot 37
   "host gone" -polku) ja ottaa `netId → (hp, dead)`-taulun.
3. Kun isäntä on maailmassa (latauksen jälkeen, ≥ 3 s), kaveri lähettää taulun. Isäntä tappaa ne,
   jotka ovat kaverilla kuolleita (elävä → kuollut on sallittu), ja asettaa HP:t. Sitten NetAI-
   omistus palaa isännälle.
4. **Wipe:** jos toisen `DEATH_COMMIT` saapuu ennen kuin ensimmäinen kuollut on taas maailmassa,
   tai toinen on kuollut, kun ensimmäinen sitoutuu, kohtaa 3 ei tehdä. Jos ensimmäisenä kuollut on
   jo saanut tilan peilattuna, se kutsuu `0xA4C0F0(WCM)` + `0x899500(...)` saadakseen tuoreen
   maailman.

Riskit:
- Isännän viholliset palaavat spawn-paikoilleen ja AI nollautuu. Tämä näkyy kaverilla hyppäyksenä,
  koska NetAI seuraa isäntää.
- HP-kirjoitus 0 voi antaa isännälle XP:tä tai pudotuksia uudelleen [ARV]. Tarkista, ja tarvittaessa
  käytä `0x9EE7E0(chr, 0)` tai disable-polkua.
- Bossit: jos boss on kesken ja isäntä kuolee, latauksen jälkeen boss on täysillä isännän päässä.
  Taulun HP-peili korjaa tämän, mutta areenan EMEVD-tila (sumuseinä) on alussa [ARV].

Loki:
- `join: host death committed (client alive) -> adopt client enemy table`
- isännällä `enemies: applied N dead / M hp from friend`
- `HP mismatch` -rivin listat tyhjenevät 10 s:n sisällä.

### P4 (ei suositella nyt). In-place-respawn ilman latausta
Hookkaa `0xA8F7E0`:n alku. Prologi `40 53 55 56 57` = 5 tavua (5-tavuinen jmp relokoi tasan nämä),
14-tavuiselle 20 tavua `...41 56 48 83 EC 30 48 C7 44 24 20 FE FF FF FF`, ei RIP-relatiivinen [VAR].
Kun kaveri elää ja idoli on samassa blokissa, ohita alkuperäinen ja tee:
- `0xA2CCF0(player)` (elvytys, chr-tyyppi takaisin, HP = max)
- `0xA2BE40(player, 100)`, `0x7A95B0()`
- teleport idolille
- `0x3EA0D74 = 0`

Tuntemattomat:
- häivytys ja `DeadScreen`-valikko jäävät päälle
- EMEVD 2003[95]:n kutsuja odottaa latausta
- `CSDeadStep`in elinkaari (`step+0x320`, tila 3 → `0xA70CE0`)
- idolin koordinaatit (`0x676CE0` antaa vain pisteen id:n)

Kokeile vasta, kun P3 on todettu riittämättömäksi. Varmistus: lokiin hookin osuma ja paluuosoite
(`0x9076AB` EMEVD / `0x11C8FF7` ESD).

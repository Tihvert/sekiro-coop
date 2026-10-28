# Vihollisten kohteenvalinta ja aktivointi vs. kaverin natiivi PlayerIns (2026-10-23)

Kysymys: miksi isännän pelissä viholliset eivät reagoi kaveriin (natiivi remote-PlayerIns, chr type 1, team `+0x74 = 2`), ja mikä on pienin muutos.
Lähde: staattinen analyysi, IDA 9.3 + Hex-Rays, oma kopio `F:\Temp\re_enemy\db.i64` (sekiro.exe 1.06, image base `0x140000000`). Ajot: `F:\Temp\re_enemy\o1` … `o24` (`summary.txt` + `f_<ea>.c`). Peliä ei käynnistetty, mitään ei muokattu.
Merkinnät: **[VAR]** = luettu dekompiloidusta koodista. **[ARV]** = päätelmä. Osoitteet ovat RVA:ita (`0x140xxxxxx - 0x140000000`).

## Lyhyt vastaus

1. **Kohteenvalinta ei suodata remote-pelaajaa pois.** Ehdokaslista rakennetaan pelaajajoukosta `WCM+0x40` (johon remote kuuluu). Tiimitaulukon mukaan vihollistiimit pitävät tiimiä 2 vihamielisenä aivan kuten tiimiä 1. Esto voi tulla vain chr-lipuista `+0x1A15` (bitit 0 ja 7) tai aktiivisuustietueesta `*(chr+0x18)`. Ne pitää lukea pelistä, koska staattisesti ne näyttävät täyttyvän remotelle. [VAR + ARV]
2. **Aktivointi ja päivitystiheys lasketaan VAIN isännän pääpelaajasta.** `0xA4E8E0` lajittelee chr:t etäisyyden mukaan pääpelaajaan `*(WCM+0x88)`. Lähimmät saavat täyden päivityksen budjetin puitteissa (interval 0/2), muut 5 tai 30 ruudun välein. Kartta-entiteetin passiiviset viholliset saavat arvon −1, eli ne eivät päivity lainkaan. Kaverin sijainti ei vaikuta tähän mitenkään. Tämä selittää havainnon: kaukana isännästä vihollinen joko ei päivity ollenkaan tai ajaa AI:ta 0,5 s välein. [VAR]
3. **Istunnon jäsenmäärä ei vaikuta kohteenvalintaan lainkaan.** Se vaikuttaa vain siihen, kumpi manipulaattori ajaa vihollista (Com vai NetAI), sekä moneen online-polkuun. Istunnon väärentäminen ei siis korjaa ongelmaa, ja isännällä se jopa jäädyttäisi kaikki viholliset. [VAR]

Pienin patch: hookkaa `0x9F6270` (chr:n update interval -setteri) ja pakota interval 0 vihollisille, jotka ovat R metrin säteellä kaverista. Jos loki näyttää vihollisen entiteetin passiiviseksi (interval −1), tämä ei riitä. Silloin ainoa todellinen korjaus on jakaa auktoriteetti: kaveri ajaa omaa AI:ta lähellään oleville vihollisille (patch 3).

---

## a) Kohteenvalinta (target search)

### Luokat [VAR, RTTI]
`SprjTargetingSystem` vt `0x29635C0` (14 slottia), `SprjAiTargetingSystemOwner` vt `0x2961690`, `TargetSearchSysSlot_*` (NormalEnemy vt `0x2965430`, NormalFriend `0x2965458`, SoundPos, LastSightPos, …), `SprjChrInsHandleTargetAccessor` vt `0x2962030`, `CSTeamType` vt `0x295CA78`. Lua-sidonnat (rekisteröinnit): `GetDist` `0x837E0`, `IsSearchTarget` `0x843B0`, `IsBattleState` `0x85920`, `IsFindState` `0x85A70`, `AddTeamRecord` `0x885C0`. Merkkijonot `TARGET_SELF/ENE_0/FRI_0` `0x59F040`.

### Kulku [VAR]
```
SprjTargetingSystem::Update  (vt slot 2)                0x61FF40
 └ 0x623CA0  (a1 = TargetingSystem, a1+8 = ThinkOwner)
    ├ 0x626430  tietoisuuslista a1+0x40..0x48 (40 B/entry): päivittää awarenessin
    │           0x612400:lla, kohde valitaan kun awareness >= 100.0 (suurin voittaa)
    └ if 0x625F30 (hakuajastin a1+0x28 laukesi):
        0x6250A0  SearchHostile
          0x62BF30(selfHandle, selfTeam, &hostile, &friend, mode=0)
          0x6244F0(hostile)  -> havainto per ehdokas: 0xBD3E40(*(*(cand+0x1FF8)+0x38))
                                + 0x617280 (näkö 0x61BB70 / kuulo 0x61C940)
        0x624850  (toissijainen haku mode=1)
```

**Ehdokassuodatin `0x62BF30`** iteroi globaalia listaa `L = *(0x143D547D8)`, vektori `L+0x08..L+0x10`:
```
for chr in L.vec:
   if !(chr+0x1A15 & 0x01) || (u8)chr+0x1A15 < 0x80: skip      // 0x9F37C0 + cmp 80h
   if !0x612B20(chr, mode): skip   // ent=*(chr+0x18): !0x9E5BB0(ent) && (mode0: !0x9E5C00(ent))
   if chr+0x08 == selfHandle: skip
   t = 0x9F1510(chr)               // = u32 chr+0x74, ellei SpEffect 302/132 tai temporary team
   if TeamRel(selfTeam, t).hostile  (0xC09660) -> hostile.push(chr)
   elif friendly (0xC097F0)        -> friend.push(chr)
```
Tässä ei ole etäisyys- eikä chr type -suodatinta eikä tarkistusta "onko main player". [VAR]

**Ehdokaslistan rakentaja `0x614DD0`** (ajetaan jakson `L+0x20` sekunnin välein; kutsuja `0xA74560`) [VAR]:
1. kaikki `WCM+0x40`-pelaajajoukon chr:t, joilla `+0x1A15 & 1` (**remote kuuluu tähän joukkoon**)
2. pääpelaajan nykyisen blokin chr-joukko `WCM+0x30 + 320*idx` (`+0x80`), `idx = *(0x143D5C0A0+0x28)`
3. kahvalista `L+0xA0..0xA8`.

**Tiimisuhde** `0xC09660(a,b)`: `obj = ((void***)0x143B0CAA0)[34*a.team + b.team]`. Objekteja on kaksi: `0x143B0A660` (vt `0x2A93E50`, slot 0 `0xC09630` palauttaa arg[0] = **tosi**) ja `0x143B0A658` (epätosi). Matriisi (rivi = hakijan tiimi, X = vihamielinen):
```
    0123456789012345678901234567890123
 6  .XX.X..........X...XXXX.X...XX.X..
 7  .XX.X..........X...XXXX.....XX.X..
13  .XX.X..........X...XXXX.....XX.X..
24  .XX.X..........X...XXXX.....XX.X..
25  .X..X..........X...........X.X....   <- 1 vihamielinen, 2 EI
27  .X..X..........X.........X...X....   <- 1 vihamielinen, 2 EI
```
Tavalliset vihollistiimit (6, 7, 13, 24, 33) näkevät tiimin 2 vihamielisenä. Poikkeus ovat tiimit 25 ja 27, jotka hyökkäävät isäntään mutta eivät tiimiin 2. Niiden merkitys on tuntematon. [VAR taulukko, ARV että tavallinen vihollinen = 6]

**Havainto** `0xBD3E40` täyttää ehdokkaan tiedot ehdokkaan moduulista `*(cand+0x1FF8)+0x38`. Kenttä `+0xC3` on pääpelaajan debug-lippu (`byte_143D7A36F`), joka koskee vain pääpelaajaa. Remotelle ei ole erillistä estoa. [VAR]

### Mitä remotelta vaaditaan, jotta se on kohde
| Ehto | Remote | Taso |
|---|---|---|
| `WCM+0x40`-joukossa | kyllä (log: natiivi spawn) | VAR |
| `u8 chr+0x1A15` bitti 0 | asettaa ChrIns vtable **slot 9** `0x9F47E0` (PlayerIns-override `0xA2C2B0`), purkaa slot 11 `0x9F4700` | ARV: asettuu |
| `u8 chr+0x1A15` bitti 7 | asettaa ChrIns **slot 25** `0xA05060` (`or 80h`), purkavat `0x9F4600` ja `0x9E88E0` | ARV: asettuu |
| `ent=*(chr+0x18)`: `+0x10 == 4`, `+0x20 & 7 == 0`, `ent+0x30 == 0` → `0x9E5C00 = 0` | pelaajalla oma `0x9E5B30`-objekti, state 4 asettuu `0x9F71D0`:ssa | ARV: remote päivittyy chr-silmukassa, joten state on 4 |
| tiimi 2 vs vihollistiimi | vihamielinen | VAR |

**Johtopäätös a:** kohteenvalinnassa ei näy rakenteellista estoa remotelle. Isännän lähellä aktiivinen vihollinen voi valita kaverin kohteeksi, mikä vastaa havaintoa "aktiiviset hyökkäävät". "Bugisuus" johtuu todennäköisemmin siitä, että remoten HP on paketin omistama (`0xB729E0` → `0xB68FF0`, remote = 1), ja viholliset päivittyvät harvakseltaan (kohta b). [ARV]

---

## b) Aktivointi ja päivitystiheys (etäisyys)

### Chr-silmukka `0xA4DB20` (WCM) [VAR]
```
ent = *(chr+0x18); f = u8 ent+0x20
active = !(f&1) && (!(f&2) || 0x9E5C00(ent)) && u32 ent+0x10 == 4
if !active || i32 chr+0x110 <= -1 || (chr+0x1F41 & 0x10):  -> 0x9F51A0 (ei päivitystä)
elif chr+0x110 == 0: täysi päivitys joka ruutu
else: päivitys kun (idx % n == frame % n), dt*n; muuten kevyt 0xA48730
```
`i32 chr+0x110` = update interval. Arvot: −2 = odottaa, −1 = pois, 0 = joka ruutu, 2/5/30 = joka n:s ruutu.

### Kuka kirjoittaa `chr+0x110`: `0xA4E8E0` (WCM, joka ruutu ennen silmukkaa) [VAR]
- `P = pos(*(WCM+0x88))` = **pääpelaajan** sijainti (`0x9F0870`). Kirjoitukset tehdään **vain** setterillä `0x9F6270(chr, n)`, kuudesta kutsukohdasta, jotka kaikki ovat `0xA4E8E0`:ssa:
  ```
  void 0x9F6270(chr, n){ if(chr+0x1A10 & 1) return;
     if(n==30 && (*(*(chr+0x1FF8))+0x10 & 0x2000000)) n=5; chr+0x110 = n; }
  ```
- Passiivinen entiteetti (yllä oleva `active == false`) tai `+0x1F41 & 0x10` antaa **−1**.
- Muut chr:t: `chr+0x1BE0 = |pos−P|²` (välimuisti, nollataan −1:ksi joka ruutu), lajittelu etäisyyden mukaan (`0xA46900`), sitten budjetti. Ensimmäiset `*(D+0x2C)` chr:ää saavat 0, seuraavat `*(D+0x30)` saavat 2, loput 5 tai 30 etäisyyskynnysten mukaan. Kynnykset ovat neliöinä `D = *(0x143D7A388)`, floatit `+0x34..+0x4C`. Pääpelaaja saa aina 0. Näkökentässä olo `chr+0x1A10 & 0x10` vaikuttaa myös.
- **Kaverin sijaintia ei käytetä missään.** Etäinen remote itsekin saa arvon 5 tai 30. [VAR]

### Muut "verkko-etäisyys"-kentät [VAR]
- `0x9F0230` laskee `chr+0x1FC8` = verkkopäivitysprioriteetin etäisyydestä pääpelaajaan (paketeille, ei AI:lle).
- `0x9F33F0` (forceLocal-lähde, ks. c) = `0x9E5C00(ent) || (ent+0x20 & 2) || (*(*(chr+0x1FF8))+0x54 & 1)`. Tämä EI ole etäisyysehto. [VAR] Hypoteesi "is_left_behind" (WEB-OPEN-SOURCE-COOP-2) kumoutuu.

### Johtopäätös b [ARV, vahva]
Isännän koneella vihollisen AI ajaa sitä harvemmin, mitä kauempana se on isännästä. Kaukana olevat:
- **interval 5/30**: AI ja havainnot päivittyvät 2–12 kertaa sekunnissa. Reaktio on hidas ja nykivä, ja NetAI-tietueet kaverille päivittyvät yhtä harvoin.
- **interval −1**: kartta-entiteetti on passiivinen (`ent+0x20` bitti 1 ilman `0x9E5C00`-ehtoa, tai bitti 0). Vihollinen on jäässä eikä havaitse ketään.

Entiteetin passiivisuus kytkeytyy todennäköisesti isännän sijaintiin, esim. map-hit/backread-ryhmiin. Lippuobjektia `ent+0x30` (+0x8C bitti 0) ei tunnistettu staattisesti. [ARV]

---

## c) Istunnon jäsenmäärä ja "väärennetty istunto"

### Lähde [VAR]
`0x8E3E60()` palauttaa `(*(S+0x168) − *(S+0x160)) / 56`, `S = *(0x143D948E8)` (`SprjSessionManagerImp`), eli 56 B:n jäsenvektorin pituuden. Chr-silmukka: `0xA4DB20` @ `0xA4DC38` = `E8 23 62 E9 FF` (call), sitten `cmp eax,1; setle` → `forceLocal = count<=1 || 0x9F33F0(chr)` → EnemyIns slot 37 `0xA14B50`.

### Mitä jäsenmäärä muuttaa (kutsukohdat) [VAR kutsu, ARV merkitys]
| Funktio | Ehto → vaikutus |
|---|---|
| `0xA4DB20` | ≤1 → forceLocal (Com-AI); >1 → omistus `WCM+0x630` ratkaisee |
| `0xA4A680` IsOwnedHere | ≤0 → tosi; muuten omistustaulu `B+0x18 i16 >= 0` |
| `0x7C4580` | `a1+0x9A9 = clamp(count−2,1,3)` (moninpeliskaalaus?) |
| `0xA73430`, `0x9B2D00` | ≤1 ja istunnossa → `0x8E4C80` (istunnon purku) |
| `0x89B8D0`, `0x6BE270`, `0x781340`, `0xA2E1B0` | >0 → "online"-ehdot (EMEVD/ESD, valikko, pelaaja) |
| `0x6DDC70`, `0x90D5C0`(case 9), `0xA77BB0`, `0x67A4B0` | muut online-haarat |
| `0x8E6FA0`/`0x8E7210`/`0x8E77E0`/`0x8E2440` | paketit/päivitys iteroivat jäsenvektoria |

### Arvio: väärennetty 2 jäsenen istunto (host) [ARV]
- **Se ei korjaa kohteenvalintaa eikä aktivointia.** Kumpikaan ketju (`0x61FF40`→`0x62BF30`→`0x614DD0`, `0xA4E8E0`) ei lue jäsenmäärää.
- Isännällä `count = 2` tekisi `forceLocal = 0`:n. Omistustaulun alkiot ovat −1 (`0xA19A50`), ja vain verkon vastaanotto `0xA1DD00` asettaa ne. Siksi **kaikki isännän viholliset vaihtuisivat NetAI:hin ja jäätyisivät**, ellei jokaisen blokin `B+0x18`-alkioita merkitä omiksi.
- Jos vektoriin lisätään väärä alkio, lähetys- ja jäsenhakupolut (`0x8E4170`, `0x8E6FA0`) dereferoivat DLNRD-jäsenosoittimia, mikä aiheuttaa kaatumisriskin. Lisäksi kytkeytyvät päälle moninpeliskaalaus, EMEVD:n online-ehdot ja Steam-polut.
- Kirurginen vaihtoehto `0xA4DC38` → `B8 02 00 00 00` (`mov eax,2`) on 5 tavua, mutta se hyödyttää vain clientiä. Asiakaspuolella sama tehdään jo slot 37 -vtable-hookilla. **Ei suositella.**

---

## EHDOTETTU PATCH

### 1. (ensisijainen, pieni) Interval-override kaverin lähellä: hook `0x9F6270`
- **Mitä**: trampoliinihook `0x1409F6270`. Ensimmäinen käsky `F6 81 10 1A 00 00 01` (`test byte [rcx+1A10h],1`, 7 B) ei ole RIP-relatiivinen.
  ```
  fn hook(chr, n): if n > 0 && is_enemy(chr) && natiivi remote olemassa
                   && |pos(chr) − pos(remote)|² < R²  (R ≈ 40 m)  { n = 0 }
                   orig(chr, n)
  ```
  `is_enemy` = chr ei ole `WCM+0x40`-joukossa (tai vtable == EnemyIns `0x2A27F28`). Arvoihin −1/−2 ei kosketa. Positio luetaan `0x9F0870(chr,&v)` tai `*(*(chr+0x50))`-ketjusta, kuten modissa nyt.
- **Riskit**: CPU-kuorma kasvaa, kun useampi vihollinen päivittyy joka ruutu (budjetti ohitetaan). Rajaa enintään noin 16 viholliseen. Hook ajetaan pelisäikeessä `0xA4E8E0`:n sisällä, joten lukitusta ei tarvita. Ei auta, jos entiteetti on passiivinen (−1).
- **Varmistus lokista** (isäntä, 5 s välein, kaverin 40 m:n säteellä olevat viholliset): `chr+0x110`, `chr+0x1BE0`, `ent=*(chr+0x18)`: `+0x10`, `u8 +0x20`, `+0x30`, sekä NetAI-tietueen kohde (`+0x148` mode/netId → onko wire `0x11` "client player"). Ennen patchia odotetaan arvoja 5/30 tai −1, patchin jälkeen 0. Pelissä kaverin pitäisi saada lähellä olevat viholliset aggroamaan, kun isäntä on kaukana. "Unmapped targets" -laskurin ja client-player-kohteiden osuuden pitäisi kasvaa.

### 2. (diagnostiikka ensin, korjaa vain jos tarpeen) Remoten ehdokaskelpoisuus
- **Lue lokiin** spawnin jälkeen ja 3 s myöhemmin: `u8 remote+0x1A15` (bitit 0 ja 7 pitää olla päällä), `ent=*(remote+0x18)`: `+0x10 == 4`, `u8 +0x20 & 7 == 0`, `+0x30 == 0`. Lisäksi onko remote listassa `L=*(base+0x3D547D8)`: skannaa `*(L+8)..*(L+0x10)` (8 B osoittimia). Huomaa, että lista päivittyy vain `L+0x20` sekunnin jaksoin.
- **Jos bitti puuttuu**: aseta `remote+0x1A15 |= 0x81` FrameBegin-vaiheessa (pelisäie), mutta vasta kun `remote+0x50` (ChrCtrl) != 0. Riski: bitti 0 on "rekisteröity maailmaan" -lippu (slot 9/11). Sen keinotekoinen asettaminen voi vaikuttaa muihin järjestelmiin, joten kirjoita vain remotelle ja vain jos loki osoittaa sen puuttuvan.
- **Jos vihollinen silti valitsee vain isännän**: tarkista vihollisen tiimi `u8 enemy+0x74`. Tiimit 25 ja 27 eivät vihaa tiimiä 2. Kokeile silloin remoten tiimiksi 1 (kirjoitus `remote+0x74 = 1`). Se on riskialtis, koska kitka ja friendly fire riippuvat tiimistä (`0xBE3DA0`).

### 3. (oikea ratkaisu passiivisille vihollisille) Jaettu auktoriteetti per vihollinen
- Kun isännän kopio on passiivinen (`chr+0x110 == -1` tai `ent`-ehto epätosi) ja kaveri on lähempänä, **kaverin kone ajaa AI:n**: client lopettaa slot 37:n `0`-vastauksen kyseiselle viholliselle (Com-AI päälle) ja lähettää tietueet (movement/behavior/action, sama formaatti `WCM+0x630`) isännälle. Isäntä taas ajaa sen NetAI:lla (slot 37 -override isäntäpuolelle).
- Tämä on natiivi DS3-malli (omistustaulu `B+0x18`, `0xA4A650`/`0xA1BB10`). Modissa on jo kummankin suunnan koodi, mutta se pitää peilata.
- **Riskit**: omistuksen vaihdon hystereesi (esim. 10 m ja 2 s) on pakollinen. Myös HP:n auktoriteetti ja deathblow tarvitsevat yhden omistajan. Työ on suurempi (päiviä).
- **Varmistus**: lokiin per vihollinen `owner=host|client` ja vaihtojen määrä minuutissa. Pelissä kaverin pitää pystyä taistelemaan alueella, jossa isäntä ei ole.

---

## Avoimet kohdat (vaativat pelin lukua)
1. Remoten `+0x1A15` ja `*(+0x18)`-kentät (patch 2).
2. Vihollisen `ent+0x20`-bitit ja `ent+0x30`-objekti, kun vihollinen on "jäässä" kaukana isännästä. Mikä asettaa `*(ent+0x30)+0x8C` bitin 0? Kirjoittajaa ei löytynyt staattisesti (`0x9E5C00` lukee sen).
3. `D=*(0x143D7A388)`: `+0x2C`/`+0x30` budjetit ja `+0x34..+0x4C` kynnykset (lue floatit lokiin kerran).
4. `0x9F1510`: vihollisen todellinen tiimi (6?) ja SpEffect 302/132 -ohitukset.

## Varmuustaulukko
| Väite | Taso |
|---|---|
| Kohdehaku: `0x61FF40`→`0x623CA0`→`0x6250A0`→`0x62BF30`, lista `*(0x143D547D8)+8` | VAR |
| Lista sisältää koko `WCM+0x40`-pelaajajoukon (`0x614DD0`) | VAR |
| Tiimitaulu `0x143B0CAA0` 34×34, tiimi 2 vihamielinen tiimeille 6/7/13/24/33 | VAR |
| Update interval `chr+0x110` vain pääpelaajan etäisyydestä (`0xA4E8E0` → `0x9F6270`) | VAR |
| Passiivinen entiteetti → −1 → ei päivitystä (`0xA4DB20`) | VAR |
| Jäsenmäärä ei vaikuta kohdehakuun; vaikuttaa Com/NetAI-valintaan ja online-polkuihin | VAR |
| Remotella `+0x1A15`-bitit ja entity state 4 | ARV |
| Kaukaisten vihollisten "ei reagoi" = interval 5/30 tai entiteetti passiivinen isännän sijainnin takia | ARV (vahva) |

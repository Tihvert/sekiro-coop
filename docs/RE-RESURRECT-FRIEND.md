# RE: kaverin Resurrect co-opissa (2026-10-25)

Haara `claude/resurrect-friend-8a6ce0` (pohja `coop-v7.1` 7e59f53, v8.24). Ei testattu pelissä.
Merkinnät: **[VAR]** = luettu koodista, datasta tai lokista. **[ARV]** = päätelmä.
RVA:t ovat moduulin kannasta (IDA-osoite − 0x140000000). IDA-kanta `F:\Temp\re_resur\db.i64`,
skriptit `F:\Temp\re_resur\s*.py`, tulokset `o*`. Pelidata purettiin repon omalla
`tools/icons/bhd5.py`:llä kansioon `F:\Temp\re_resur\data` (EMEVD, c0000.anibnd/behbnd/hks).

## 0. Lyhyesti

1. **Lähtöoletus ei pitänyt.** Kaverille tarjottiin Resurrect klo 14:05, ja hän käytti sen. [VAR, kaverin loki]
   - `[1792929914] death: first dead frame; resurrection fields 0.000 / -1.000`
   - `[1792929916] resurrect: OFFERED (SpEffect 110090 on)`
   - `[1792929917] state: sp +[... 110015, 110031] ... alive false -> true (anim 110031)` = nousu.
   - `[1792929931]` uusi kuolema, `[1792929935] resurrect: NOT offered (SpEffect 110093 without 110090)`.
   Kenttäpari `0.000 / -1.000` ei kerro latauksesta mitään. Se on sama myös hostin onnistuneessa
   kuolemassa (`[1792929861]` → `OFFERED`). bossrules.rs sanoo jo, etteivät kentät ole luotettavia.
2. **Molemmat pelaajat käyttäytyivät samoin.** Bossissa 1500800 kumpikin sai yhden Resurrectin per
   yritys, ja toinen kuolema oli `NOT offered`. Toinen ylösnousemus vaatii lukon purkamisen (§1).
3. **Varsinainen co-op-puute:** lukko purkautui lokeissa vain pelaajan **omasta** deathblow'sta.
   Kun host tekee deathblow't, vain host saa toisen Resurrectin. Korjaus jakaa purun kaikille (§4).
4. **H1 osittain:** kaverin tapoista peli ei maksa hostin omistamia vihollisia kaverille (`loot: N died
   without our kill`), mutta modi maksaa ne jo itse (`0x9FFD50`). Lukko ei kuitenkaan pure tappopalkkiossa (§2).

## 1. Ylösnousemuksen ehto ja lukko [VAR, staattinen IDA]

- Ehto `0xA34B70(chr)`: debug-tavu `base+0x3D7A36B`, TAI kaikki seuraavat:
  - kykybitti 2 (`0x7A7FE0(0x79EAD0(PGD+0x278), 2)`), eli Resurrection-taito;
  - `f32 PGD+0x150 == 0` (lukko, debug-nimi "Revive Recast Point", `0x7C51A0`);
  - goods 1110 (`0x40000456`, väliaikainen solmu) > 0, TAI ilman SpEffect 400:aa goods 1100 (`0x4000044C`, solmut) > 0.
- PGD = `*(*(base+0x3D5AAC0)+8)` = `chr->vt[55]()`, sama olio (kentät `+0x8A8/+0x10E` molemmissa).
- **Nousu** `0xA36A60` (Resurrect-valinta, ikkunakentät `chr+0x212C/+0x2138`): `0xA2CD50`, `0xA324E0`
  (kuluttaa 1110:n tai 1100:n yhden), sitten lukko += 100.0 ja clamp [0, 100] (`dword_143B010A0 = 100.0`).
- **Respawn/latauksen jälkeen** `0xA2EE70`: lukko = 0 ja täyttö. Siksi jokaisen kuolemalatauksen jälkeen
  ensimmäinen kuolema saa taas tarjouksen.
- **Aikapurku:** `0xA36A60` vähentää lukkoa `chr+0x2130 * dt` elossa ollessa. `chr+0x2130` = "RevivalRecastGain",
  alustetaan 0:ksi (`0xA217A0`, `mov [rdi+2130h], esi`, esi = 0). Vanilla-arvolla ei siis aikapurkua.
- **SpEffect-purku:** joka ruutu `0xA30B50` → `0xBFDCE0(chr+0x11D0)` summaa aktiivisten SpEffectien
  `revivalRecastDamage` (paramirivin `+0x418`) → `0xA23730(PGD, -summa)`.
- SpEffectParamissa (gameparam, 3096 riviä) arvo on vain kolmella rivillä, kaikilla 99999:
  - **100** = lepo (`0xA2BE40` antaa 100..109);
  - **3292** = SpEffect 110015:n `replaceSpEffectId`. 110015 annetaan nousuanimaatioissa
    (c0000 TAE type 67: a00 110030/110031/111030/111031/113030/114030). 3292:lla on
    `invocationConditionsStateChange1 = 936`. Lokeissa 3292 ei koskaan näy, ja lukko pysyi nousun jälkeen
    (toinen kuolema `NOT offered`), joten ehto ei yleensä täyty. [VAR data, ARV merkitys]
  - **8100**: antajaa ei löytynyt mistään parametrista, EMEVD:n argumenteista, c0000:n tai 62 vihollisen
    TAE:n SpEffect-tapahtumista, HKS:n numeroista tai exen koodin vakioista. Ainoa osuma on viittaukseton
    .rdata-taulukko `0x142A42230` (7000, 7010, 7011, 8000, 8100, 8101, 9000). c0000 TAE:n type 128
    `(1, 8100)` on ääni (DS3-pohjassa PlaySound), ei SpEffect. [VAR]

## 2. Mikä purkaa lukon tapossa / deathblow'ssa

- **Varmistettu lokista:** hostilla oli kaksi toista Resurrectia samassa elämässä, kummassakin
  välissä hostin oma deathblow:
  - `1792915101` nousu → `1792915115 deathblow: enemy 0x38a7 locked by us (our throw 15684)` → `1792915152 OFFERED`;
  - `1792928018` nousu → `1792928032/068/094` deathblow't bossiin 1500800 (throw 57982) → `1792928159 OFFERED`.
  Deathblow-ruudussa pelaajalle tulee SpEffect 110030 (deathblow-animaatioiden a00 7000x0 / a200 51xxxx
  TAE type 67), mutta 110030:lla ei ole `revivalRecastDamage`a eikä korvausketjua. [VAR]
- **Ei löytynyt staattisesti:** tarkka kohta, joka purkaa lukon deathblow'ssa. Kaikki tunnetut
  kirjoittajat on käyty läpi: `0xA23730` (kutsujat `0xA30B50`, `0xA36A60`), `0xA2EE70` (respawn)
  sekä `0x7A..0xA5` -alueen `[reg+150h]`-kirjoitukset. Ketju kulkee todennäköisesti 3292:n ehdon 936
  kautta (state-ehto, joka toteutuu deathblow'ssa) tai 8100:n kautta. [ARV]
- **Siksi korjaus ei riipu laukaisijasta:** se tarkkailee pelaajan omaa lukkoa. Kun peli purkaa
  sen elossa ollessa, purku jaetaan.
- **Tappokrediitti (H1):** pelin oma tappopolku `0x9EDA50` → `0x9FFF50` (dropit, `chr+0x1A12` bitti 1) ja
  `0x9FFD50` (sen/XP, bitti 2; sisältä `0x9FFB40` = DS3:n "estus recovery" -arpa NpcParam `+0x25E..+0x26E`,
  ja NpcParam `+0x348` skill_point). Se maksaa aina **paikalliselle** pelaajalle (`WorldChrMan+0x88`).
  Etätappo (`0x9EE7E0(chr, remote=1)`) ja ForceDead(noReward) ohittavat polun. modin `loot_step` kutsuu
  silloin itse `0x9FFF50`/`0x9FFD50`:n. Mikään tästä polusta ei kirjoita lukkoa. [VAR]
- Avoin [ARV]: täyttävätkö tapot Resurrect-solmuja (goods 1100) ja tapahtuuko se `0x9FFB40`:ssa.
  Uudet lokirivit näyttävät tämän (solmut joka tapossa).

## 3. Muut modit

- Vanilla-FromSoftin co-op ja Seamless-modit jakavat tappopalkkiot paikallisesti joka koneella. Jokainen
  saa omat runensa/sielunsa, eikä palkkiota lähetetä verkossa. Bossin palkkion ERSC paikkaa hookkaamalla
  `GetBlockClearBonus`in (0x871E0) ja lähettämällä tapahtuman verkkoon (`docs/RE-ERSC.md` §4). [VAR ERSC]
- DS3SC/ERSC/BBSC:ssä ei ole Resurrectin kaltaista resurssia, joten suoraa mallia ei ole. Lähimmät ovat
  "event to everyone, apply locally" (ERSC:n bossipalkkio, lippusynkka tyyppi 17/18) ja oma
  `restsync` (lepo peilataan kaikille). Korjaus käyttää samaa mallia: tapahtuma kaikille ja
  vastaanottaja tekee paikallisesti saman, minkä peli tekisi. [ARV]

## 4. Korjaus (`crates/coop-dll/src/resurrect.rs`)

- Joka ruutu (`world.rs on_frame`) luetaan pelaajan lukko `f32 PGD+0x150`, onko pelaaja elossa (HP > 0),
  ChrIns-osoite ja onko SpEffect 110030 päällä.
- **Lähetys** `EV_REVIVE_UNLOCK` (tapahtuma 44, luotettava, sessio välittää kaikille 2–6 pelaajalle),
  enintään yksi per 3 s:
  - (a) **oma deathblow alkaa**: SpEffect 110030 tulee päälle elävälle pelaajalle. 110030 on c0000 TAE:n
    type 67 jokaisessa deathblow-animaatiossa (a200–a250 510xxx–516xxx / 600xxx, a00 7000x0; 140
    animaatiota). Tämä toimii, vaikka oma lukko olisi 0 (deathblow'n tekijä ei ole kuollut).
  - (b) **oma lukko > 0 → 0 pelin toimesta**: pelaaja elossa samassa hahmossa, ruutujen väli < 1,5 s
    (ei latausta), ei omaa lepoa 5 s sisällä. Kattaa purun, jonka (a) ohitti.
- **Vastaanotto:** jos elossa ja oma lukko > 0, kirjoitetaan `PGD+0x150 = 0.0` (sama tila, johon pelin
  `0xA23730` sen vie). Detektori merkitään, jotta purkua ei lähetetä takaisin. Kuollut, ladannut tai
  lukoton pelaaja ei muutu.
- **Ei anna mitään:** ei goods-esineitä, solmuja eikä taitoa. Pelaaja, jolta solmut puuttuvat, ei vieläkään
  voi nousta. Ei tallennusmuokkausta.
- Asetus `[GAMEPLAY] share_resurrect_unlock = 1` (0 = vain oma, kuten vanilla). Vanhan buildin peeri
  kirjaa vain `world: unknown event 44`, joten protokollaa ei tarvinnut nostaa.
- Testit: `resurrect.rs` 7 yksikkötestiä (purku elossa jaetaan, levon purku ei jaeta, lataus tai kuolema
  ei ole purku, nousu nostaa ja vastaanotettu purku ei kaiu, vastaanotto vain elävälle lukitulle, oma
  deathblow jaetaan myös ilman omaa lukkoa ja kuolleen 110030 ei, yksi tapahtuma per 3 s) ja
  `settings.rs`:n ini-testi.
- Rajaus [ARV]: vanilla-lokien perusteella deathblow purkaa lukon. Jos myös tavallinen tappo ilman
  deathblow'ta purkaa sen, se jaetaan vain silloin, kun tappajan oma lukko oli ylhäällä (b).

## 5. Lokirivit (kaikki `resurrect:`)

| Rivi | Milloin |
|---|---|
| `resurrect: we rose: our lock 0.0 -> 100.0 ...; nodes (goods 1100) N, temporary node (1110) N, Resurrection skill (2000) yes` | nousu |
| `resurrect: we died: our lock X (LOCKED... / not locked); nodes ...` | joka kuolema: miksi tarjottiin tai ei |
| `resurrect: our deathblow started (SpEffect 110030), our lock X; told every player (unlock #n)` | oma deathblow, lähetetty |
| `resurrect: our lock 100.0 -> 0 by the game (our deathblow / kill) ...; told every player / already told ...` | oma purku |
| `resurrect: unlock #n from peer P (its deathblow / kill): our lock 100.0 -> 0, Resurrect usable again here` | toisen purku otettiin käyttöön |
| `resurrect: unlock #n from peer P: nothing to do (we are dead/alive, our lock 0.0)` | ei tarvittu |
| `resurrect: enemy ID died (our kill / not our kill): our lock A (3 s ago) -> B now ...; nodes ...` | joka vihollisen kuolema (max 400) |
| `resurrect: our lock 100.0 -> 0 by our rest` | lepo |

## 6. Testiohje Tihvertille ja kaverille

Molemmilla sama build. Kaverin ini: `share_resurrect_unlock = 1` (oletus).

1. **Perustesti, tavalliset viholliset ensin:**
   - Kaveri kuolee ja valitsee Resurrect. Kaverin lokissa `resurrect: we rose: our lock 0.0 -> 100.0`.
   - **Host** tekee deathblow'n mihin tahansa viholliseen. Hostin lokissa
     `resurrect: our deathblow started (SpEffect 110030), our lock 0.0; told every player (unlock #n)`.
   - Kaverin lokissa `resurrect: unlock #n from peer 0 (its deathblow / kill): our lock 100.0 -> 0`.
   - Kaveri kuolee uudelleen: `resurrect: we died: our lock 0.0 (not locked)` ja `resurrect: OFFERED`.
2. **Bossi tai minibossi (esim. 1500800):** sama, mutta deathblow on bossin vaihe. Toista päinvastoin
   (host nousee, kaveri tekee deathblow'n), jotta molemmat suunnat tulevat testatuiksi. Kolmella
   pelaajalla: yhden deathblow purkaa kahden muun lukon (`unlock #n from peer X` molemmilla).
3. Jos hostin deathblow'sta ei tule `our deathblow started` -riviä, signaali 110030 ei toimi siinä
   animaatiossa. Kerro kellonaika, niin katson lokin `state: sp` -rivit.
4. **Solmut:** katso `we died` -riviltä `nodes (goods 1100)`. Jos lukko on 0 mutta solmuja 0 ja
   väliaikaisia 0, `NOT offered` on oikein. Tapporivit kertovat, täyttävätkö toisen pelaajan tapot solmuja.
5. **Mitä lähettää:** host `%LOCALAPPDATA%\sekiro-coop\log.txt`, kaverin loki tulee automaattisesti.
   `grep "resurrect:"` riittää. Kellonaika, jolloin Resurrect puuttui, auttaa.
6. Jos jotain hajoaa: `share_resurrect_unlock = 0` molemmilla palauttaa vanilla-käytöksen (lokit jäävät).

## 7. Varmistettu / päätelty

- **Varmistettu (staattinen IDA + data + loki):** ehto `0xA34B70`, lukko `PGD+0x150`, nousu asettaa 100
  (`0xA36A60`), respawn nollaa (`0xA2EE70`), SpEffect-purku (`0xA30B50` / `0xBFDCE0` / `0xA23730`, kenttä
  `+0x418`), rivit 100/3292/8100, 110015 → 3292 nousuanimaatioissa. Kaverin Resurrect tarjottiin ja
  käytettiin 14:05. Toinen Resurrect tuli hostille kahdesti oman deathblow'n jälkeen. SpEffect 110030
  on c0000:n deathblow-animaatioissa (TAE), ja hostin lokissa se tuli deathblow'n alkaessa
  (`1792915116 state: sp +[9281, 9481, 109250, 110030]`).
- **Päätelty:** että juuri deathblow (eikä esim. aika) purki lukon näissä kahdessa tapauksessa (ajoitus
  sopii, mekanismia ei nähty); 3292:n ehdon 936 merkitys; solmujen täyttö tapoista.
- **Ei testattu pelissä:** koko korjaus. Build ja yksikkötestit, katso commit.

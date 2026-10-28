# ER Seamlessin Themida-suojaus: tarvitaanko purkua? (2026-10-23)

Kysymys: onko ersc.dll:n Themida/WinLicense-suojauksen purkamisesta tai analyysin helpottamisesta konkreettista hyötyä Sekiro-co-op-modillemme?

**Lyhyt vastaus: ei nyt.**
- Tarvitsemamme tieto on joko jo luettavassa koodissa, saatavilla suojaamattomasta Sekiro Onlinesta (sama tekijä, sama peli), tai selvitettävissä omalla ajonaikaisella lokituksella Sekirossa.
- Suojatut osat ovat pääosin Elden Ring -kohtaisia osoitteita ja tunnisteita. Ne eivät siirry Sekiroon.

Pohja: `docs\RE-ERSC.md` ja agentin työtiedostot `F:\Temp\re_ersc\` (`ersc.i64`, `ersc.asm`, `o1`, `o2\all.c`). Alkuperäinen DLL on muuttamaton (`extract\SeamlessCoop\ersc.dll`, SHA-256 FCD11A18…). Tähän arvioon ei ajettu uusia IDA-analyyseja. Mittaukset on tehty olemassa olevasta IDA-listauksesta (`ersc.asm`) tekstihauilla. Modia tai sen testiversiota ei muutettu.

Merkinnät: **[VAR]** = mitattu tiedostoista, **[ARV]** = päätelmä.

## 1. Mitä ER Seamlessista pystytään jo tutkimaan

| Osa | Tila | Peruste |
|---|---|---|
| `.text` (1,6 Mt, MSVC-koodi) | **Luettavissa** | IDA tunnistaa 4 954 funktiota, ja `.pdata` (≈ 4 900 merkintää) vastaa niitä. Hex-Rays dekompiloi ne. [VAR] |
| Suojattu `ERSC`-osio (11 Mt, RWX) | Suojaimen koodia | IDA näkee siellä noin 4 984 "funktiota", jotka ovat Themidan VM-käsittelijöitä ja mutatoitua koodia, eivät modin logiikkaa. [VAR määrä, ARV luonne] |
| `.text`-funktiot, jotka hyppäävät `ERSC`-osioon | **15 / 4 954** (0,3 %) | Haettu `call`/`jmp`-kohteet `0x180240000…0x180D30000` funktioittain. Esim. `0x180029F40` (lokalisaatio/viestirepo), `0x180096740`, `0x1800A9280`, `0x1800DE6D0`. [VAR] |
| Merkkijonot | Pääosin selväkielisiä | Lähdepolut, lokalisaatioavaimet, singletonien ja pelin luokkien nimet. xorstr-salattuja ovat ini-avaimet ja URL:t. [VAR] |
| Asetukset ja säännöt | Täysin luettavissa | `ersc_settings.ini` ja `locale\english.json` kertovat kaikki ominaisuudet ja oletusarvot. [VAR] |
| Launcher | Selväkielinen | Klassinen injektio: `CreateRemoteThread` + `LoadLibraryA`, EAC-startteri ohitetaan. [VAR] |

Tästä on jo saatu mekanismit, ja ne on kirjattu RE-ERSC.md:hen:
- pelin oma `CSSessionManager` + Steam-P2P
- `EcTestMultiplayState`-hookit (skriptit näkevät yksinpelin)
- lepo = reset kaikille, ja potku areenalta
- katsojatila bossissa
- lippuviestit 17 ja 18, sekä "kaikkien oltava paikalla" -lippuportti
- SpEffect-skaalaus ja Rot-debuffi param-riveillä

## 2. Mitkä tiedot ovat edelleen saavuttamattomissa, ja miksi

Nämä ovat 15 suojattuun koodiin hyppäävän funktion ja VM-osion takana [VAR/ARV]:

1. **AOB-hakukuviot** (signatures). Kertoisivat ER:n osoitteet, joihin modi tarttuu. → **Eivät siirry Sekiroon**, koska eri exe ja eri osoitteet.
2. **RTTI-hakija** eli se, mihin pelin vtableihin hookit kohdistuvat. Luokkien nimet näkyvät silti (`EcTestMultiplayState`, `EcTestIsMyWorld`, `EcTestChrType`, `EcTestChrDead`, `EcTestDistance`, `CS::ChrSet`, `PlayerIns`…). → Tiedämme **mitä** hookataan. Tarkat kohteet ovat ER-kohtaisia.
3. **Ini-avainten käsittely** (xorstr). → Avaimet ja arvot näkyvät jo ini-tiedostossa.
4. **Modin viestien vastaanottodispatch** eli mihin viestityypit 17/18/21/31… ohjautuvat. → Lähetyspuoli ja tyyppien merkitys luettiin jo. Dispatchin sisältö olisi lähinnä protokollasuunnittelun yksityiskohtia.
5. **Lista EMEVD-eventeistä**, joille `MultiplayState` pakotetaan yksinpeliksi. → ER:n event-id:t. Sekirossa eventit ovat eri, joten lista ei siirry. Sekiro Online teki saman Sekirolle (`0x69D310`:n kuusi kutsua, `0x6A2F8E`, ESD env `0x8522B0`; RE-SEKIRO-ONLINE §5), ja ne on jo luettu.

Ne ovat saavuttamattomissa, koska Themida korvaa nämä funktiot virtuaalikoneen tavukoodilla tai mutatoidulla koodilla. Staattinen purku vaatisi VM-käsittelijöiden kääntämistä, mikä on päiviä tai viikkoja työtä ilman takuita. Ajonaikainen purku taas vaatisi ER:n asennuksen ja modin ajamisen (§4).

## 3. Onko purku tarpeellista tavoitteidemme kannalta?

**Ei.** Perustelut:

- **Siirrettävyys.** Suojatut tiedot ovat ER-binäärin osoitteita ja ER:n event-id:itä. Sekirossa ne pitää joka tapauksessa löytää uudelleen, eikä ER:n osoite auta.
- **Parempi lähde samaan kysymykseen.** Sekiro Online on saman tekijän modi *Sekirolle*, eikä sitä ole pakattu.
  - Siitä on jo ratkaistu 84/85 kutsuttua pelin funktiota ja 119/121 hook-kohtaa 1.06-osoitteiksi (RE-SEKIRO-ONLINE.md, `F:\Temp\re_so\gsig2.txt`).
  - Se kattaa juuri ne asiat, joita ERSC:n suojattu osa piilottaa: session väärennyskohdat, vihollispakettien send/recv-kohdat, lippupaketti 26, EMEVD-moninpeliehdot.
- **Mekanismit tunnetaan jo** luettavasta koodista ja julkisista lähteistä. Arkkitehtuuripäätökseen riittää tieto *miten* ERSC toimii, eikä tarkkoja ER-osoitteita tarvita.
- **Kustannus ja riski.**
  - Ajonaikainen purku vaatii Elden Ringin, ERSC:n launcherin (ohittaa EAC:n) ja muistidumpin. Themida tarkistaa debuggerit ja virtuaalikoneet, joten työtä on 1–3 päivää ilman varmuutta.
  - Tuntemattomia unpacker-työkaluja ei voi käyttää turvallisesti.
  - Hyöty olisi pieni, koska saatu tieto koskisi ER:ää.

## 4. Suositeltu analyysimenetelmä

Järjestys hyödyn ja riskin mukaan:

1. **Ajonaikainen havainnointi Sekirossa omalla modilla (suositus).** Lisätään testiversioon pelkkä havaintotila, joka ei muuta pelin käytöstä:
   - lokitetaan `FrpgSessionManager`in send/recv-kutsujen pakettityypit, koot ja tahdit;
   - lokitetaan jäsenmäärä- ja IsHost/IsClient/InSession-kyselyt Sekiro Onlinen listaamissa kohdissa.

   Tämä vastaa Sekiro Onlinen avoimiin kysymyksiin suoraan omasta pelistä (prio-arvo, tyyppien 20/60/61/8 sisältö, vihollisen HP:n ja kuoleman reitti, `EventFlagMan+0x220`). Riski on pieni, koska tila on vain lukeva ja se kytketään päälle asetuksella.
2. **Kohdistettu staattinen RE.** Sekiron exe (oma IDA-kanta) ja Sekiro Onlinen DLL (suojaamaton), aina yksi kysymys kerrallaan. Näin on edetty tähänkin asti (RE-FINDINGS-2, RE-ENEMY-TARGETING, RE-BACKSTAB, RE-DEATH-RESET).
3. **ERSC:n luettava `.text`** suunnittelumallien lähteenä: katsojatila, lippuportti, lepo + areenapotku, SpEffect-skaalaus. Ei vaadi purkua.
4. **(Vain jos jokin kysymys jää auki eikä 1–3 ratkaise sitä.)** ERSC:n ajonaikainen muistidumppi eristetyssä ympäristössä, ER:n kanssa. Ei nyt.

## 5. Mitä tietoja tarvitsemme seuraavaksi

Nämä ovat arkkitehtuuripäätöksen ("pelin oma sessio meidän UDP:llä" vai nykyinen malli) ja Sekiro Online -tutkimuksen jatkon tarpeet:

1. **Pelin vihollispaketit:**
   - Mitä tyypit 4 (liike), 70 (käytös), 71 (action), 22 (ehdokaslista) ja 23 (omistuslista) sisältävät?
   - Millä tahdilla ne lähtevät, kun sessio on väärennetty Sekiro Onlinen kohdista (RE-SEKIRO-ONLINE §2: `0x1072FE0`, `0x1072F80`, `0x1072F00`/`0x8E4B10`, `0x832050`, jäsenmäärä `0xA4DC38`, `0xA1CF13`, `0xA1DD5E`, `0xA4E5BE`, `0xA734D8`)?
   - Kulkevatko ne kaikki send/recv-kohtien kautta (`0xA1C4F9`/`0xA1BF0B` type 4, `0xA1B4BA`/`0xA1B07B` 70, `0xA1DC74` 71, `0xA1D1FC` 22…)?
2. **Vihollisen HP, kuolema ja vahinko** natiivissa sessiossa: mikä pakettityyppi, ja miten se suhtautuu nykyiseen damage-raporttiimme?
3. **Omistuksen "prio"** (`0xA1D550`/`0xA1C590`): onko se etäisyys vai aggro, ja miten omistus vaihtuu, kun kaveri on kaukana isännästä? Tämä ratkaisee kaverin lähellä olevien vihollisten ongelman natiivisti.
4. **Efektit ja combat artit:** mikä pakettityyppi kantaa bullet- ja SFX-tapahtumat (combat artien tuuliefekti puuttuu nyt kaverilta)? Lisäksi Sekiro Onlinen SpEffect-synkkakohta `0x9E9F53`.
5. **Liittymisdata** 20/60/61/8 ja etäpelaajan luonti `0xA7A840`: tarvitaanko niitä, vai riittääkö nykyinen natiivi etäpelaajamme? Meidän 60/120 Hz -sijaintipakotus kannattaa pitää.
6. **Skriptit:** missä Sekiron EMEVD/ESD-moninpeliehdot pitää pakottaa yksinpeliksi, kun pelin sessio on päällä? Sekiro Onlinen kohdat ovat valmiina (§5). Lisäksi bossien aloitus- ja porttiliput "kaikkien oltava paikalla" -portille.
7. **Lippusynkka:** pelin oma tyyppi 26 (`0x8D9F46`), mutta meidän luotettavalla ja järjestetyllä kanavalla. Esineliput (per pelaaja) pitää luokitella ItemLotParamin perusteella, jotta idolit ja maailman tapahtumat synkkaavat mutta poiminnat eivät.

**Seuraava askel (ehdotus):** kohdan 4.1 havaintotila testiversioon. Se on vain lukeva, eikä se muuta nykyistä toimivaa modia. Sen jälkeen päätetään, siirrytäänkö pelin omaan sessioon.

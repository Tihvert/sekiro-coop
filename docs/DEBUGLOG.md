# Sekiro co-op – DEBUGLOG

Yksi merkintä per bugi, uusin ylimpänä. Tarkoitus: seuraava tili ei kokeile samoja umpikujia uudelleen. Syvä RE-tieto kuuluu `docs/RE-*.md`-tiedostoihin ja suunnittelu `IMPLEMENTATION-NOTES.md`:hen; tänne vain tiivis oire → syy → korjaus -ketju ja linkki.

## Pohja
### YYYY-MM-DD – lyhyt otsikko
- Oire:
- Kokeiltu (ei toiminut):
- Syy (todennettu / oletus):
- Korjaus (commit, versio):
- Lisätietoa: NOTES §xx / RE-xxx.md

## Merkinnät
### 2026-10-25 – kaverin Resurrect ei toiminut toisella kerralla
- Oire: ensimmäinen Resurrect toimi (14:05), toinen ei.
- Syy (staattinen IDA, ei livetodistetta): lukko `PGD+0x150` purkautui vasta 100 nousun jälkeen.
- Korjaus: 39b93e5 (jaettu Resurrect-lukon avaus kaikille elossa oleville). Ei testattu pelissä.
- Lisätietoa: NOTES §58, `docs/RE-RESURRECT-FRIEND.md`.

### 2026-10-25 – bossin desync ja HP kuoleman jälkeen (avoin TODO)
- Oire: bossi eri paikassa/tilassa eri koneilla; kuoleman jälkeen ei täysiä HP:ita ilman lepoa.
- Tila: katso `docs/JATKA-TASTA-MULTIPLAYER.md` ylin UUSI TODO.

# Sekiro co-op – BRIEF (pysyvä, muuttuu harvoin)

## Mikä
Sekiro-moninpelimodi (2–6 pelaajaa, UDP, ei Steam-verkkoa). Rust-DLL, jossa peliin kytkeytyvä koodi, natiivisessio, bossi- ja vihollisskaalaus sekä kuolema- ja Resurrect-säännöt. Kaverin kanssa pelataan oikeasti, joten testi = pelitesti.

## Valmis tarkoittaa
- Testit menevät läpi (`test.cmd`, lib-testit).
- Muutos on kokeiltu pelissä Tihvertin ja kaverin kanssa. Pelkkä build ei riitä, koska oraakkeli on pelitesti.
- Lokista (`state:`, `audit:`, `claimed by both machines`) näkee, ettei vika toistu.

## Rajat (omistaja)
- Moninpeli on pääsuunta, ei vain kaksinpeli.
- Ei Anthropic API -avainta, ei maksullisia palveluja.
- Ei pelin tai Steamin käynnistystä Claudesta. Ei tallennusmuokkauksia pelin ollessa auki tai ilman lupaa.
- Ei mod-sisäisiä esineiden antohuijauksia.
- Repo yksityinen. Julkaisun yhteydessä: vendor/sekiro-lisenssit mukaan, Steam ID pois dokumenteista.

## Työnjako
- Sonnet kirjoittaa koodin, Opus tarkistaa ja suunnittelee.
- DeepSeek vain lokien lukuun. Ei protokollaan, muistirakenteisiin, offsetteihin eikä hookkeihin.
- Kaksi sessiota voi työskennellä vuorotellen: aja `git status` ja `git log -5` ennen muokkausta, äläkä koskaan kaksi kirjoittajaa samoihin tiedostoihin (ks. `CLAUDE.md`).

## Julkaisu
`release.cmd`, commit, ff-merge multi-worktreehen, push `coop-v7.1` ja `coop-multi` (ks. `CLAUDE.md`).

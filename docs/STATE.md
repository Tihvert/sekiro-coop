# Sekiro co-op – STATE (lyhyt tilannekuva; päivitä jokaisen osatehtävän jälkeen)

Päivitetty: 2026-10-26 (pohja)

## Missä totuus on
- Yksityiskohtainen tila ja TODO: `docs/JATKA-TASTA-MULTIPLAYER.md`. Sen otsikko sanoo v8.15, mutta commit-historia on pidemmällä (v8.24 näkyy), joten otsikko on todennäköisesti vanhentunut. Tarkista `git log -5`.
- Suunnittelu: `docs/IMPLEMENTATION-NOTES.md`. Vanha siirtomuistio (polut, työkalut): `docs/HANDOFF.md`.
- Vault: `C:\Users\<user>\Documents\ObsidianVault\AI\Projects\SEKIRO-COOP\HANDOFF.md`.

## Haarat
- Aktiivinen: `coop-v7.1` (kansio `sekiro-seamless`), HEAD 60fdfc3. Sisältää kaiken `coop-multi`-haarasta.
- `coop-multi` (worktree `../sekiro-seamless-multi`) on 3 committia jäljessä. Se tuodaan ajan tasalle julkaisussa: `git -C ../sekiro-seamless-multi merge --ff-only coop-v7.1`. Älä poista worktreeä, julkaisuohje käyttää sitä.
- Muut: `claude/bugsweep-effa81`, `claude/resurrect-friend-8a6ce0` (Resurrect-korjaus, yhdistetty 39b93e5:llä).
- Remote: github.com/Tihvert/sekiro-seamless-coop (yksityinen, ei julkiseksi ilman lupaa).

## Nyt kesken
- (ei kirjattuna; katso JATKA-TASTA ylin UUSI TODO)

## Seuraava askel
- (yksi konkreettinen asia)

## Avoimet kysymykset Tihvertille
- (tyhjä)

# Speksi: TODO 2 – kaksi bossia wipen jälkeen (Claude Opus, 2026-10-24 16:15, VAIN analyysi, ei koodimuutoksia)

## Oire (host-loki 2026-10-24)
`[1792843000] death: both died (wipe): no restore` → `[1792843002] persist: boss 1110800 restored hp 2397 lives 1 after a death reload (record from the friend, ok = true)` → vaihe 2 + vaihe 1 yhtä aikaa.

## Juurisyy (luettu koodista, ei ajettu)
Kaksi eri wipe-määritelmää, jotka eivät keskustele keskenään:
- `crates/coop-dll/src/puppet.rs` `death_sync` (~r. 1196) käyttää `peer_dead_for_wipe` (r. 1263): kuollut = EV_DEATH 90 s sisällä **TAI snapshot-HP ≤ 0 TAI `world.peer_down`** (kuolemaruudulla odottava). → päätti "wipe".
- `crates/coop-dll/src/persist.rs` tyhjentää bossirekordit vain `is_wipe` (r. 94, vaatii jokaisen peerin vahvistetun kuoleman ikkunassa) tai `note_wipe` (r. 369, kutsutaan nyt vain `bossrules.rs:1587` "boss wipe").
- Kaverin kuolema vahvistui vasta 843065 → persist ei nähnyt wipeä → `boss_step` (persist.rs ~r. 620) palautti kaverilta tulleen rekordin (`from_friend`).

## Korjausehdotus (Sonnet toteuttaa, Opus tarkistaa)
1. `puppet.rs` `death_sync`: `if all_dead(&dead)` -haaraan `crate::persist::note_wipe("death_sync wipe");` ennen lokiriviä.
2. **Tarkista `note_wipe`:n `hold_load = true` -semantiikka:** se tarkoittaa "wipe kun me olemme kuolleena → ei mitään ennen omaa latausta". Tässä host on JO ladannut (death_sync ajetaan lataus + death_restore_delay_s jälkeen). Jos hold_load estää tallennuksen seuraavaan lataukseen asti, tarvitaan variantti `note_wipe_after_reload` joka vain `clear()`:aa + AFTER_REST_IGNORE 13 s kaverin viestien ohitus (jotta kaverin myöhässä lähettämä rekordi ei palaa).
3. Testi `persist`-moduuliin: rekordi from_friend + note_wipe → `boss_step` ei palauta; kaverin rekordi-viesti 13 s sisällä wipestä ohitetaan.
4. Bosses.rs-vaihetaulukko: varmista että wipe-clear poistaa myös vaihe-entiteettien (1110800/1110801) rekordit (clear() tyhjentää koko `bosses`-mapin, r. 321 → pitäisi riittää).

## Yhteys TODO 1:een (hypoteesi, ei varmistettu)
`peer_dead_for_wipe` laskee `peer_down` = kaveri kuolemaruudulla **resurrection-valinnan aikana** kuolleeksi. Sama tulkinta bossrules.rs:n spectate-polussa ("peer 1 is out of the fight (wipe)" 15 s jälkeen) selittäisi miksi kaveri ei voinut resurrectata: modi julisti wipen ennen kuin kaveri ehti valita. Tarkista bossrules.rs spectate "out of the fight" -ehto: HP 0 ≠ kuollut Sekirossa, ennen EV_DEATH:ia (death committed).

## TODO 1 -lisäselvitys (16:20, luettu koodista)
- `EV_DOWN(1)` lähetetään `bossrules.rs:1319` heti kun pelaaja **aloittaa spectaten** (kuolemaruutu pidätetty, "waiting for ..."). Vastaanottaja `world.rs:833–841` → `peer_out(from, "wipe")`.
- Ketju 14:57: host kuoli → spectate (phase B) odottaa kaveria. Kaveri meni HP 0:aan → kaverin modi aloitti OMAN spectaten ja lähetti EV_DOWN → host: "peer 1 is out of the fight (wipe)" → "release wipe after 14 s" → kumpikin ladataan.
- **Todennäköinen vika:** kun kaikki muut tarkkailtavat ovat jo kuolleita (host spectatessa), kaverin HP 0 EI saa aloittaa spectatea vaan pelin oma kuolemaruutu + Resurrect-valinta pitää sallia normaalisti. Sekirossa HP 0 ≠ kuollut ennen kuin pelaaja valitsee "Death" (EV_DEATH = death committed).
- Korjaussuunta: (a) spectate aloitetaan vasta kun resurrection ei ole saatavilla TAI pelaaja valitsi Death; tai (b) EV_DOWN ei laske wipeksi ennen EV_DEATHia, jos kaverilla on resurrection-solmuja. Tarvitaan: kaverin loki `friend-uploads\log-Friend2-20261024-145834.txt` riveiltä 1792843040–843066 (spectate start -rivi ja syy), ja resurrection-tilan muistiosoite (IMPLEMENTATION-NOTES, hae "resurrect").
- Sama `peer_down` → `peer_dead_for_wipe` (puppet.rs:1264) tekee TODO 2:n wipen. Korjaa molemmat samalla säännöllä: "down" = odottaa, ei kuollut.

## KORJAUS edelliseen (16:25): kaverin loki kumoaa EV_DOWN-hypoteesin
Kaverin loki (`log-Friend2-20261024-145834.txt`, kello ~6 s edellä): EI spectate-aloitusta, EI EV_DOWN-lähetystä. Rivit: 843047 "the friend (peer 0) is down" (= host), sitten hiljaista kunnes 843065 "we died (death committed)" → persist-wipe-clear kaverin päässä.
**Oikea laukaisija:** `bossrules.rs` ~r. 1418 `watch_step`: tarkkailtava peer **0 HP:ssa `DEAD_AFTER` (5 s) ilman reviveä = kuollut**. Sekirossa Resurrect-valinta odottaa pelaajaa rajattomasti, ja kuolemaruudun animaatio + valinta vie helposti yli 5 s → host julistaa wipen (843054) → "release wipe after 14 s" → host lataa → kaverin resurrection-mahdollisuus menee (kaveri committasi kuoleman 843065 ≈ host 843059).
**Korjaus:** HP 0 -peeriä ei julisteta ulos ajastimella, vaan vasta EV_DEATH (death committed) tai poistuminen blokista. Jos ajastin halutaan varalle (yhteyskatko), sen pitää olla pitkä (≥ 60 s) ja nollautua kun snapshotit jatkuvat. Sama sääntö `puppet.rs:1264` `peer_dead_for_wipe`: `hp <= 0` ei yksin riitä → TODO 2 korjautuu samalla (wipe vasta kun kaikki oikeasti committasivat kuoleman), ja `note_wipe` kannattaa silti kutsua death_syncin wipe-haarassa.

## hold_load selvitetty (16:50)
`persist.rs`: `frozen() = hold_load || ignoring_friend()` (r. 340); `hold_load` vapautuu vasta seuraavassa latauksessa (r. 405). Jos host kutsuu `note_wipe` death_syncissä (host on JO ladannut), tallennus jäätyy koko loppualueen ajaksi seuraavaan lataukseen asti → yksinkuoleman jälkeen ei bossirekordia. **Siis ÄLÄ käytä `note_wipe`:a sellaisenaan**: lisää `pub fn note_wipe_loaded(why)` = `s.clear(...)` ilman `hold_load`:ia. `clear()` asettaa `cleared_at` → `ignoring_friend()` hylkää kaverin myöhässä tulevat rekordit 13 s (AFTER_REST_IGNORE), mikä estää juuri tämän bugin. Testi: from_friend-rekordi + note_wipe_loaded → boss_step ei palauta; tallennus jatkuu 13 s jälkeen.

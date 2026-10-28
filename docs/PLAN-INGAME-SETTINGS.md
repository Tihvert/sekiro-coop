# Suunnitelma: pelin sisäiset vaikeusasetukset (2026-10-26, EI toteutettu)

Tavoite: vaikeutta (ja pari muuta perusasetusta) voi säätää kesken pelin ilman ini-tiedoston käsin muokkausta.
Isäntä päättää arvot, ja ne pätevät molemmilla koneilla (kuten skaalaus jo nyt).

## Mitä on jo olemassa (ei tehdä uudelleen)
- `sekiro_coop_settings.ini` ladataan uudelleen livenä (`settings::tick`), kun tiedosto muuttuu.
- Skaalaus (`enemy_/boss_health_scaling`, `enemy_/boss_posture_scaling`) kirjoitetaan viholliskohtaisesti `scaling.rs`:ssä joka ruutu, joten muutos tulee voimaan elävillä vihollisilla.
- Isäntä lähettää skaalauskonfiguraation kaverille (`puppet.rs` pitää kirjaa viimeksi lähetetystä, `scale_sent`), joten ini-muutos kulkee kaverille. Tarkistetaan pelitestissä, että se kulkee kesken session eikä vain liittyessä.
- Overlay (ImGui, `ui.rs`) näyttää jo tilarivin ja ilmoituksia, mutta `no_inputs`: se ei ota syötettä.

## Mitä voisi säätää kesken pelin (rajaus ehdotuksena)
1. Vihollisten HP ja asento (+ %), bossien HP ja asento (+ %).
2. Kuolemadebuffi päälle/pois (ja pinojen enimmäismäärä).
3. Ei vielä: asennon palautumisnopeus (vaatii RE:n), vihollisten vahinko (vaatii RE:n).

## Vaiheet (halvimmasta kalleimpaan)
**Vaihe 1: esiasetukset yhdellä näppäimellä, toimii ohjaimella.** Isäntä vaihtaa esiasetusta (Helppo / Normaali / Vaikea / Oma = ini) pikanäppäimellä tai ohjainyhdistelmällä (esim. Back + D-pad ylös/alas, samalla `pad_buttons_down`-lukijalla kuin F11). Ruudulle tulee ilmoitus ("Vaikeus: Normaali, boss HP +50 %, asento +50 %"), uudet arvot menevät samaan skaalauspolkuun kuin ini ja lähetetään kaverille. Esiasetukset ovat iniin kirjoitettavia (`[PRESETS]`), joten käyttäjä voi muuttaa niitä. Arvio: pieni, vain mod-puoli, ei protokollamuutosta jos nykyinen skaalausviesti riittää.
**Vaihe 2: ImGui-paneeli hiirellä/näppäimistöllä.** Pikanäppäin avaa paneelin (liukusäätimet prosenteille, valintaruutu debuffille). Vaatii syötteen sieppauksen (`no_inputs` pois paneelin aikana, pelin kamera/hyökkäys estettävä, kursori näkyviin). Ohjaimella ei toimi ilman vaihetta 1:n tapaa. Arvio: keskisuuri, riski syötteen jumittumisesta, joten oma kytkin (`ingame_menu = 1`).
**Vaihe 3 (myöhemmin):** per-pelaaja-asetukset (kaverin oma debuffi), asennon palautumisen skaalaus, vihollisen vahinko, tallennus jokaiselle pelaajalle.

## Päätökset, jotka tarvitaan sinulta
- Riittääkö vaihe 1 (esiasetukset + ilmoitus) vai halutaanko heti paneeli?
- Mitkä esiasetukset ja mitkä arvot (ehdotus: Helppo 0/0, Normaali 50/50 boss, 60/60 viholliset, Vaikea 100/100)?
- Saako kaveri säätää omia asetuksiaan vai vain isäntä (ehdotus: vain isäntä, kaveri näkee ilmoituksen)?
- Pitääkö vaikeuden muutos vaikuttaa jo käynnissä olevaan bossitaisteluun (ehdotus: ei, vasta seuraavaan tai kun boss on kuollut)?

## Testi (kun toteutettu)
Host vaihtaa esiasetusta: lokiin `settings: preset X`, kaverin lokiin sama skaalausarvo, F11:n `scale:`-rivit näyttävät uuden `want`-arvon ja `ours true`, ei vihollista jonka max-HP eroaa koneiden välillä.

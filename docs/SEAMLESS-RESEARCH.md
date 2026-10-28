# Seamless co-op -modien tutkimus (Sekiro-modia varten)

Päiväys: 2026-10-22. Menetelmä: vain verkkohaku/WebFetch. Nexus-sivu ja GitHub-sivut palauttivat osin 403, joten moni kohta perustuu hakutulosten tiivistelmiin. Merkinnät: [V] = useammasta lähteestä / suoraan dokumentista, [?] = yhdestä lähteestä tai varmistamaton.

## 1. Elden Ring Seamless Co-op (ERSC, LukeYui/yuiamoroll)

Lähteet: https://www.nexusmods.com/eldenring/mods/510 (ei haettavissa suoraan, 403), https://ersc-docs.github.io/seamless-modding/ , https://ersc-docs.github.io/how-to-install-and-update/ , https://www.pcgamer.com/is-elden-ring-co-op-multiplayer/ , https://www.exitlag.com/blog/elden-ring-seamless-co-op/ , Steam-keskustelut.

**Sessiomalli**
- Pysyvä sessio koko pelin ajan, ei summonausta per alue. Host + vierailijat; useimmat lähteet 4 vierasta (yhteensä 6) [?] (lähteet ristiriitaisia: 4 vs 5 pelaajaa).
- Salasana yhdistää pelaajat (`cooppassword`). Liittyminen itemeillä: Tiny Great Pot avaa session, Effigy of Malenia hakee session salasanalla, Separation Mist lopettaa session ja lähettää kotiin, Judicator's Rulebook vaihtaa friendly fire / PvP [V, PC Gamer/exitlag].
- Oma erillinen tallennustiedosto (`.co2`), EAC pois, ei FromSoftin palvelimia, ei ban-riskiä [V]. Ei yhteensopiva vanilla-pelaajien kanssa.
- Kaikilla oltava samat modit; host-asetukset määräävät maailman säännöt [V].

**Kuolema**
- Vieras/pelaaja kuolee: respawn omalle viimeiselle gracelle, sessio jatkuu [V].
- Bossitaistelussa kuollut ei respawnaa vaan spectaa, kunnes boss kuolee, kaikki kuolevat tai joku lepää gracella [V, Steam]. Ini-vaihtoehto `always_spectate_on_death` (spectate joka kuolemalla) [?, yksi lähde].
- Rot Essence -debuff kuolemasta, kasautuu, poistuu graceen lepäämällä; `death_debuffs`-asetus [V, ERSC docs].
- "Shared death" -toggle (kaikki lähellä kuolevat) [?, vain yksi yhteensopivuussivu, mahdollisesti lisämodi].

**Bossit, sumuseinät**
- Sumuseinien ja monipelialuerajoitusten poistot; telepotit pois [V]. Se, tarvitseeko kaikkien astua sumuseinän läpi yhdessä, jäi varmistamatta [?].
- Jos joku lepää gracella kesken ei-open-world-bossin, taistelija heitetään sumuseinän ulkopuolelle ja boss resetoituu [?, yksi Steam-postaus].

**Grace / lepääminen**
- Yhden pelaajan lepo resetoi maailman tilan kaikille (viholliset respawnaavat) → estää desyncin [?, yksi lähde mutta uskottava].

**Maailman eteneminen ja flagit**
- Bossit, tyhjennetyt alueet, avatut arkut, questit synkronoituvat (lähteet sanovat "enimmäkseen") [V]. Maailmankartta muistuttaa hostin tilaa liittyessä.
- Boss-flagit: kaikki saavat saman edistyksen ja boss-droppi (taattu) menee jokaisen inventaarioon; Great Rune seuraa "krediittiä": jos tapat bossin toisen maailmassa, saat Great Runen avauksen hostin versiosta, ja oman maailmasi boss antaa sitten vain runet [V, hakutulos Nexus-changelogista].
- Changelog-viitteet: korjauksia "vieraan flagit vuotivat hostin maailmaan" ja "maailman event-flagit jäivät voimaan uudelleenlatauksessa" (Fire Giant, Radagon -bugit) [V]. Opetus: flagien synkka on suurin bugilähde.
- Kauppojen varasto on pelaajakohtainen, ei jaettu [V].

**Esineet, runet**
- World-esineet ja vihollisdropit annetaan kaikille; talisman pouch -tyyppiset boss-droppi kaikille [V]. Runejakoa ei vahvistettu [?].

**Skaalaus ja PvP**
- `ersc_settings.ini` (HUOM: oikea nimi, ei `seamless_coop_settings.ini`) [V, ERSC docs]:
  - [GAMEPLAY] `allow_invaders` (0/1, oletus 1), `death_debuffs` (0/1), `allow_summons` (0/1, henkiavut), `overhead_player_display` (0 normaali, 1 ei mitään, 2 ping, 3 taso, 4 kuolemamäärä).
  - [SCALING] (% per pelaaja): `enemy_health_scaling` 35, `enemy_damage_scaling` 0, `enemy_posture_scaling` 15, `boss_health_scaling` 100, `boss_damage_scaling` 0, `boss_posture_scaling` 20.
  - [PASSWORD] `cooppassword`; [SAVE] `save_file_extension` (co2); [LANGUAGE] `mod_language_override`.
- Invasiot valinnaisia; PvP/friendly fire vaihdettavissa esineellä. Nimikyltit (overhead display), Torrent molemmille [V].

## 2. Muut LukeYui-modit

- **DS3 Seamless Co-op** (julk. 2025-02-25) [V]: https://www.nexusmods.com/darksouls3/mods/1574 (vanha Enhanced Co-op), https://soulsmods.com/mods/cmkspr1aa002w09l7l6borm3v/seamless-co-op-dark-souls-iii , https://www.windowscentral.com/gaming/dark-souls-3-seamless-co-op-is-here-bringing-elden-rings-best-mod-to-one-of-fromsoftwares-best-games . Jopa 6 pelaajaa, Steam P2P (uusi networking API), erilliset save-tiedostot, NPC-dialogi synkattu, multiplayer-edistys siirtyy omaan maailmaan, invasiot pois oletuksena, vihollis-/boss-HP skaalaus 5-6 hengelle. Kuolema bossitaistelussa/invaasiossa → limbo/spectate kunnes kaikki kuolleet tai joku lepää bonfirella.
- **DS2 Seamless Co-op** [V]: https://www.pcgamesn.com/dark-souls-ii-0/seamless-co-op-mod , https://www.patreon.com/yuiamoroll/posts/announcement-2-161261612 . Max 6 pelaajaa, skaalaus enemmille, valinnainen "world tenancy" -vaikeusmekaniikka, proximity chat PvP:ssä, vanilla-bugikorjauksia, duel-areenat/invasiot toimivat (Rat King -tyyppiset erikoiskovenantit ei).
- **Dark Souls Remastered / AC6 / Nightreign**: Nightreign-mod lisää duo-tilan (vanilla vain solo/trio), oma .exe, erilliset savet, ei matchmaking-palvelinta [V]: https://www.dsogaming.com/mods/elden-ring-nightreign-gets-seamless-co-op-unlocked-framerate-mods/ . DSR/AC6-modeista ei löytynyt yksityiskohtia [?].

Ero ERSC:hen: DS3/DS2 ovat vanhempia pelejä, joissa area-lataukset ja ei-open-world-rakenne tekevät "kaikki samassa maailmassa" -säännöistä helpompia; Sekiro on lähempänä DS3:a (lineaariset alueet, idolit = bonfiret).

## 3. Bloodborne ja Sekiro Online

**BBSC** (Bloodborne PC port / shadPS4) [V]: https://github.com/CLSAMG22/BBSC , https://seamlesshunt.com/wiki , https://github.com/deadinside28/bloodborne_pc/blob/master/docs/BBSC.md
- Isäntä + 4 vierasta; vieraat seuraavat hostia alueiden ja lamppujen välillä ja respawnaavat ryhmän mukana. Jokainen pitää oman tasonsa/echoesinsa/itemit.
- Jaettu eteneminen valinnainen: hostin boss-tapot ja oikotiet lisätään vieraan maailmaan; vieras saa boss-palkinnot ja vihollisdropit, mutta jo kerätyt itemit eivät palaa.
- Vihollisskaalaus pelaajamäärällä; co-op-itemit (Hunt Bell, Kin Bell, Bloodlust Bell, Parting Blank, Duel Bell = PvP-toggle) ilman Insight-hintaa.
- Arkkitehtuuri: shadPS4-fork + mod-DLL + oma Python-peliserveri (`bbsc_gameserver.py`) + launcher. Eli oma palvelinvälitteinen verkko.
- Rajoitukset: v0.0.1 early test, Chalice-kryptat ei tuettu, tallennukset varmuuskopioitava.

**Sekiro Online** (LukeYui, 2021) [V]: https://www.siliconera.com/sekiro-online-mod-adds-co-op-and-pvp-on-pcs/ , https://kotaku.com/sekiro-multiplayer-mod-lets-friends-die-horribly-toget-1843265922
- P2P, max 6 (host + 5), itemit Coward's Bell / Cursed Emblem / Spirit Essence / Vengeful Spirit; invader/phantom-tasot skaalataan paikalliseen pelaajaan.
- Rajoitukset: sessio päättyy alueenvaihdossa, maailmaa ei synkata liittyessä (vihollinen kuollut toiselle, elossa toiselle), pelaajat eivät despawnaa kuollessaan, liukumista liittyessä, ei taukoa, versioiden oltava samat, "5000 pingin Dark Souls" -tunne PvE:ssä. Tutoriaalialue softlock-riski.
- Tämä on suoraan meidän lähtökohtamme: Sekiro Online = vanilla-NPC-verkkokoodi (phantom-mekaniikka), ei oikeaa seamless-mallia.

## 4. Avoimen lähdekoodin uudelleentoteutukset

- **micthiesen/unseamless-coop** (Rust, ER) https://github.com/micthiesen/unseamless-coop [V, README]: täysin uusi toteutus `fromsoftware-rs`-SDK:lla, dinput8.dll + oma launcher. Salasanalla Steam-lobby → autentikoitu sivukanava, hostin asetukset synkataan peereille, nimikyltit/toastit. EI VIELÄ pelattava: "game-session layer" (pelaajien sijoitus toistensa maailmaan) on reverse-engineering-vaiheessa. Opetus: ER:ssä natiivi session-/phantom-koneisto on vaikein osa.
- **mstampfli/sekiro-coop** (Rust, Sekiro v1.06) https://github.com/mstampfli/sekiro-coop [V, README; sama kohde kuin meillä]:
  - Oma raaka P2P UDP. Pelaajasnapshotit 60 Hz epäluotettavasti molempiin suuntiin; pelitapahtumat (flag, effect, item, XP, warp) vain muutoksesta, luotettavasti (sliding-window ACK + retransmit).
  - Viholliset: host lähettää 5 Hz, 64-bittinen per-entiteetti-digest suodattaa muuttumattomat (~97 % skipattu idlenä); client soveltaa HP:n vain laskevana.
  - Host omistaa viholliset ("authority", proximity-handoff suunniteltu); 60 framen state-hash desync-tunnistus, 3 strikea → sessio päättyy.
  - Hookit (MinHook): SetFlag, ApplyEffect, DeleteEffect, GiveItem, AddExperience, WarpBonfire. Vihollisrekisteri syötetään hook-argumenteista (WorldChrMan-sweep kaatui vanhentuneeseen dataan, tärkeä oppi).
  - Avoimet: etäpelaajaa ei renderöidä (suunnitelma: puppet-NPC `common.emevd`-patchilla), ei damage-hookia, rollback ei käytössä, vain v1.06-offsetit, vain suora IP.
- Muuta: Mod Engine 2 / me3 -injektio, ImGui-overlay ovat tavallisia. Muita Sekiro-co-op-reposeja en löytänyt [?].

## 5. Ei-FromSoft-vertailu

Hakutulokset olivat ohuita; arkkitehtuuridokumentteja ei saatu haettua.
- **HKMP (Hollow Knight)** https://github.com/Extremelyd1/HKMP : pelaajasijainnit/animaatiot server-välitteisesti; vihollis-/boss-sync tuli 3.0:ssa (lähteet ohuet) [?]. Ei tarkkaa authority-mallia varmistettu.
- **Skyrim Together Reborn** (TiltedPhoques: TiltedOnline/TiltedEvolution, wiki.tiltedphoques.com – ei luettu) [?]: party leader = maailman auktoriteetti; questit etenevät johtajan mukana; jokainen client omistaa oman hahmonsa tilan, muut näyttävät sen proxy-actorina; NPC/maailmatila synkataan johtajan kautta; palvelimella sync-togglet (kalenteri = edistynein pelaaja, XP, misc-questit; item drop pois koska kaatoi pelin). Opetus: yksi "johtaja" quest-/maailmatilalle, jokainen omistaa oman hahmonsa.
- Yleinen malli (oma tieto, ei lähteistä): host-authoritative NPC:t, 10-30 Hz tila + client-puolen interpolointi (~100 ms puskuri), eventit luotettavina, tilasnapshotit epäluotettavina.

## 6. Tarkistuslista Sekiroon

Ensisijaisuus: M = must, S = should, N = nice. "Nyt" = jo meillä.

| Ominaisuus | ERSC-käytös | Tyypillinen toteutus | Prio | Sekiro-huomio |
|---|---|---|---|---|
| Salasana-sessio + join code | password + itemit | oma/Steam-lobby | nyt | Meillä UDP+UPnP. Lisää: hostin asetusten synkka liittyessä (S) |
| Pelaajaghost + sijainti | natiivi phantom | natiivi netcode / oma | nyt | Lisää interpolointi ja animaatio-/asenteen synkka (S) |
| Idol-warp seuraus | vieras seuraa hostia | oma paketti + warp-hook | nyt | mstampfli hookaa WarpBonfire |
| Kuolema: oma idoli | respawn omalle gracelle | oma | nyt | Sekirossa Resurrection (kuolemattomuus) kuluttaa; sovittava säännöt (M) |
| Boss-kuolema = spectate | spectate kunnes boss/kaikki kuolleet | oma kamera + tila | M | Sekirossa ei ole co-op-bossia: kamera hostin/elävän kohteeseen, lopetus kun boss kuolee tai kaikki kuolleet tai joku lepää |
| Viholliset host-auktoriteetti | host | oma paketit / natiivi | nyt (1. versio) | Käytä mstampflin digest + 5 Hz -opetusta: delta + hash; meillä 20 Hz hyvä; lisää interpolointi (S) |
| Vahinko clientiltä hostille | host laskee | oma (cumulative) | nyt | Lisää kuittaus + idempotenssi (M) |
| Viholliset kuolevat/respawnaavat samoin | grace-lepo resetoi kaikille | event flagit + oma | M | Idolilepo resetoi viholliset: molempien tila synkattava, muuten desync |
| Boss-state: fog gate, HP, vaihe, posture | skaalattu, sumuseinät auki | oma + flagit | M | Entity-ID:t samat → map by id; ei sumuseinäongelmaa (Sekirossa kaikki sumut ovat omia: pelaaja, joka ei ole areenalla, pitää saada sisään tai ei) |
| Posture/deathblow synkka | posture-skaalaus | oma | M | Sekirossa taistelu perustuu posturen; host laskee, clientin deflect/parry-eventit hostille. Deathblow-animaatio (kaksi tilaa) vaikea (S) |
| Mikiri/prosthetic/combat-eventit | n/a | oma eventit | S | Clientin osumat hostille eventteinä, ei vain HP-summina |
| Event flagit (boss kuollut, NPC, oikotiet) | host-maailma + jaettu kredit | SprjEventFlagMan, oma flag-diff | M | Hookaa SetFlag (mstampfli) ja lähetä luotettavasti; kaksi suuntaa; filtteri paikallisille/UI-flageille (ERSC-bugit: vuotavat flagit) |
| Maailman ensimmäinen synkka liittyessä | hostin tila | flag-snapshot | M | Sekiro Online ei tehnyt tätä; lähetä flag-diff liittyessä |
| Itemit ja jaettu loot | droppi kaikille | give-item-hook + oma | S | Boss-droppi (memories, Prayer Beads, Gourd Seed) molemmille; hookaa GiveItem |
| XP/Sen jako | kukin omaansa/droppi | AddExperience-hook | S | Valitse: Sen+XP molemmille, skill points |
| Vihollisskaalaus per pelaaja | ini % per pelaaja | HP/posture-kerroin | S | Sekirossa posture + HP; lisää ini-asetukset (enemy_hp/posture, boss_hp/posture) |
| Asetustiedosto (ini) | ersc_settings.ini | oma | S | Host-asetukset voittavat ja synkataan |
| Nimikyltti / ping / kuolemalaskuri | overhead_player_display | oma overlay/ImGui | N | Helppo, hyvä debugissa |
| Kuoleman jälkiseuraukset (Rot) | death_debuffs | effect-hook | N | Sekirossa Dragonrot-tyyppinen = olemassa; vaihtoehto Unseen Aid -logiikka |
| Pause / menu | n/a | oma | S | Sekiro Online: ei voi pausettaa; päätä miten käsitellään (ei oikeaa pausea) |
| Cutscenet ja NPC-dialogit | DS3: NPC-dialogi synkattu | flagit/EMEVD | S | Cutscene-tilassa toinen pelaaja pitää odottaa tai ohittaa; riski (S) |
| Alueenvaihto / lataus | pysyvä sessio | oma | M | Entity-id-rekisteri tyhjennettävä ja rakennettava uudelleen (mstampfli: hook-argumenteista, ei sweep) |
| Desync-tunnistus | host resetoi graceen | hash 60 framen välein | S | mstampfli: 60 framen hash, 3 strikea |
| PvP / invaasiot | valinnainen toggle | natiivi | N | Ei prioriteetti |
| Spirit ashes -vastine (prosthetics / Spirit Emblems) | allow_summons | oma | N | Ei tarvita; prosthetic-vahinko ok kunhan osuu |
| Pelaajamäärä >2 | 4-5 | geneerinen | N | Meillä 2 riittää |
| Versionhallinta / yhteensopivuus | samat modit vaaditaan | versiotarkistus handshakessa | S | Sekiro Online: versiot oltava samat |
| Savet erillään | .co2 | tiedostopääte | S | Estää vanilla-tallennuksen sotkemisen ja bannit (Sekirossa ei online-banneja, mutta suojaa tallennusta) |

## Top 10 puuttuvaa ominaisuutta (tiivistelmä)
1. Event flag -synkka kahteen suuntaan + liittymishetken flag-snapshot (SetFlag-hook, luotettava kuljetus).
2. Boss-spectate-sääntö (kuolleena spectataan kunnes boss kuolee / kaikki kuolleet / joku lepää).
3. Idolilepo resetoi viholliset molemmille ja pitää tilat synkassa.
4. Posture/deathblow/parry-eventit (Sekiron ydin) clientiltä hostille, ei vain HP-summat.
5. Alueenvaihdon käsittely: entiteettirekisterin uudelleenrakennus (ei WorldChrMan-sweepiä).
6. Interpolointi ja animaatiosynkka vihollisille/ghostille (puskuroi ~100 ms).
7. Loot/XP/Sen/boss-droppien jako molemmille (GiveItem/AddExperience-hookit).
8. Vihollisskaalaus ini-asetuksilla (HP, posture; boss erikseen), host-asetukset synkattuna.
9. Desync-tunnistus (state hash 60 framen välein) ja versiotarkistus handshakessa.
10. Cutscene-, pause- ja menu-käsittely sekä erillinen save-tiedosto.

## Varauksia
- ERSC:n Nexus-sivu ja GitHub-sivut eivät olleet suoraan luettavissa; ERSC-kohdat ovat sekundäärilähteistä (PC Gamer, exitlag, Steam, Nexus-changelog-tiivistelmä). `always_spectate_on_death`, "shared death", grace-reset ja sumuseinäkäyttäytyminen ovat yhden lähteen varassa [?].
- HKMP/Skyrim Together: arkkitehtuurityötä ei löytynyt; ehdotus lukea TiltedPhoques-wiki ja HKMP-lähdekoodi (client addon entity sync) jatkossa.
- DSR/AC6-seamless-modeista ei saatu yksityiskohtia.

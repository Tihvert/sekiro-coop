# SPEC: orpojen Goods-esineiden siivous + palkinnot kaverin tapoista (Sekiro 1.06)

Tila 2026-10-23. Lähde: IDA-kanta `quick_re7.i64` (kopio `F:\Temp\re_items\items.i64`), purut `F:\Temp\re_items\o1..o12`.
Kaikki osoitteet ovat RVA:ita (VA − 0x140000000). [VAR] = luettu purusta. [PÄÄT] = päätelty, varmista ajossa lokilla.

## 1. Tunnistus: mikä on "?GoodsName?"

- `0x10AA1F0 GetGoodsName(MsgRepository*, u32 goodsId) -> const wchar_t*` [VAR] kokeilee FMG-pankit
  järjestyksessä 111, 10, 210, 250 (`0x2603620 MsgRepo_Lookup(repo, 0, bank, id)`, palauttaa 0 jos riviä ei ole).
  Jos mikään ei löydy, se palauttaa vakio-osoittimen `L"?GoodsName?"` (RVA `0x2B955E8`). Sama kaava on
  Weapon-, Protector-, Accessory-, Gem-, Magic-, Npc- ja PlaceName-hauissa (`0x2B95638`...).
- Yleinen hakija `0x913C60 GetItemName(u32 categoryShifted, u32 paramId)` [VAR] kytkee 0x10000000/0x20000000/0x40000000
  (Goods → `0x10AA1F0`). MsgRepository-singleton: `qword [0x3D964C8]` (`NS_SPRJ::MsgRepositoryImp`, vtable 0x2B95590).
- **Tarkistus "nimi puuttuu":** `GetGoodsName(*(0x3D964C8), id) == base+0x2B955E8` (vertaa osoitinta, ei tekstiä).
  Tämä on täsmälleen sama ehto, jolla valikko näyttää "?GoodsName?".
- **Param-tarkistus:** `0x10BF630 GetEquipParamGoods(out[16], i32 id)` [VAR]: `out+8` = rivin osoitin tai 0
  (SoloParamRepository `qword [0x3D978B0]`). Vendor-crate: `SoloParamRepository::get::<EquipParamGoods>(id)` → `None`.
  Punainen ikoni johtuu luultavasti puuttuvasta rivistä (iconId puuttuu) [PÄÄT].
- **Luotettavin sääntö:** orpo = kategoria Goods (`id & 0xF0000000 == 0x40000000`) JA nimi puuttuu JA param-rivi puuttuu.
  Pelkkä nimen puuttuminen on vasta "ehdokas" (vanilla voi sisältää nimettömiä testirivejä [PÄÄT]).
  Kaksi riippumatonta ehtoa yhdessä tarkoittaa käytännössä nollaa vääriä osumia.

## 2. Inventaarion läpikäynti

- `GameDataMan` = `qword [0x3D5AAC0]`; `+0x08` → PlayerGameData (paikallinen pelaaja).
- PlayerGameData `+0x278` = EquipGameData (koodi käyttää `v8+632`) [VAR]; EquipGameData `+0x168` = EquipInventoryData
  (koodissa `a1+360`) [VAR], mikä vastaa vendor-cratea (`equip_inventory_data`, koot assertoitu).
- Käy läpi `InventoryItemsData::items()` (avainesineet ensin, sitten normaalit; harva taulukko, tyhjät ohitetaan).
  Entry: `gaitem_handle u32, item_id u32, quantity u32, pad`.
- Kerää lista `(item_id, quantity, kind=key|normal, name_missing, param_missing)` ennen yhtäkään poistoa.
  Älä poista iteroinnin aikana (poisto muuttaa taulukkoa ja hash-ketjuja).

## 3. Poisto pelin omalla funktiolla

`0x67B230 LuaEventMan_RemoveItem(void* unused, u32 categoryShifted, u32 paramId, i32 count) -> bool` [VAR]
- Sama funktio, jota EMEVD 2003[24] RemoveItemFromPlayer käyttää: bank 2003 -dispatcher `0x904EF0`, `case 0x18` [VAR].
  Vendor-crate: `GameDataMan::remove_item(item, qty)`, joka välittää `category<<28` eli Goodsille `0x40000000` (oikein).
- Ensimmäistä argumenttia ei lueta. Funktio hakee GameDataManin itse globaalista.
- Silmukka per kierros [VAR]:
  1. `0x79CD00(EGD, cat, id)` → inventaarioindeksi (<0 → loppu)
  2. `0x79D7B0(EGD, idx)` etsii indeksin 25 varustepaikasta (`EGD+0x24`); jos löytyy → `0x79D100(EGD, slot, 1)` riisuu,
     `0x79FFF0(EGD)` päivittää. **Pikavalikko- ja varustepaikat siivoutuvat siis automaattisesti.**
  3. `0x79AFA0(EGD, &id, 0, 1)` → `0x79AB70(EGD, idx, -1, ...)` → `0x7A0E50`: poistaa merkinnän määrällä −1.
- **`count` tarkoittaa poistettavien merkintöjen (pinojen) määrää, ei kappaleita** [PÄÄT, koska määrä −1 = koko pino].
  Goodsilla on yksi merkintä per id, joten anna `count = 1` ja varmista lokista, että quantity on poiston jälkeen 0.
- Puuttuva param-rivi on turvallinen: `0x79AB70` ja `0x7A0E50` tarkistavat `row == 0` ennen lukemista [VAR].
- Kutsu vain modin frame-taskista (pääsäie, sama paikka kuin muutkin pelikutsut), kun PlayerGameData != null,
  ei latausruutua, ei menua auki [PÄÄT]. Max 1 poisto per frame. Kun lista on käyty, uudelleenskannaa ja lokita tulos.

## 4. Turvallisuus ja käyttöliittymä

- Aja vain co-op-tallennukselle (`S0000.coop.sl2` ladattuna). Älä koskaan aja tavalliselle tallennukselle.
- Ennen ensimmäistä poistoa modi kopioi `.coop.sl2` → `.coop.sl2.bak-YYYYMMDD-HHMM` (jos se ei vielä ole olemassa).
- Kaksivaiheinen: (A) `[items] scan` lokittaa ehdokkaat, (B) poisto vain, jos käyttäjä hyväksyy. Esimerkiksi
  `config.toml: item_cleanup = "scan" | "remove_confirmed"` ja hyväksytyt id:t tiedostossa `item_cleanup_allow.txt`.
- Lokirivi: `goods id=0x40XXXXXX param=NNN qty=Q list=key|normal name_missing=1 param_missing=1`.
- Peli tallentaa muutoksen itse seuraavalla autosavella. Modin ei tarvitse kirjoittaa tallennusta.

## 5. Jos tunnistus ei ole luotettava

Vaihtoehto: vaihe A lokittaa kaikki Goods-merkinnät ja niiden nimet (`GetItemName`). Käyttäjä kopioi
poistettavat id:t `item_cleanup_allow.txt`:hen. Vaihe B poistaa vain listan id:t, joilla on edelleen
`name_missing=1` (kaksoisvarmistus). Hyväksymislistaan ei saa päästää id:tä, jolla on nimi.

## 6. GiveItem (Sekiro Online, Liite A #17)

`0x91C970 MapItemMan_GrantItems(MapItemMan*, ItemBuffer*, void* out, bool, u32* outCount, bool)` [VAR] (vendor `grant_items`).
MapItemMan = `qword [0x3D6CDC0]`. Sitä ei tarvita siivoukseen, mutta se on kaikkien lot-antojen loppupää (kohta 7).

## 7. Palkinnot kaverin tapoista

### 7a. Miten tappo normaalisti antaa pudotuksen [VAR]
- Vihollisen kuolemakäsittelijät (`0x9F4220`, `0x9E9FC0`, `0x9EDA50`, `0x9EDAD0`, `0x9F4130`, `0x9F4280`) kutsuvat
  ehdolla `!0x9E5C00(chr+0x18)` (`*(*(chr+0x18)+0x30)+0x8C & 1`) kahta funktiota:
  - `0x9FFD50(ChrIns*)` = Sen (NpcParam `+0x28 getSoul`) ja kokemus (`row+0x348`). Lippu `chr+0x1A12 |= 2`.
  - `0x9FFF50(ChrIns*)` = pudotukset. NpcParam-rivi saadaan `vt[+376](chr)` → `{.., +8 row*}`. Se ajaa
    `0x918D40` loteille `row+0x2D0` ja `row+0x304`, ja sitten `0x918480(MapItemMan, chr+8 handle, npcAcc)`, joka ajaa
    `0x918090(MapItemMan, handle, row+0x2C itemLotId_1, ...)` ja (jos lippu 9500 ei ole päällä) bonuslotin `row+0x344 + 10*n`.
    Lippu `chr+0x1A12 |= 1` (ei tuplaa).
- Esto: `0x9F2DE0` palauttaa 1, jos `chr+0x1A12 & 0x10` (asetetaan `0x9EDD80 SetNoDrop(chr, bool)`) tai globaali ehto.
- `0x918090` vaatii lisäksi: `0x8E4730()==0` (ei pelin omaa nettisessiota), `EventFlagMan+0x220 != 1`, sekä että paikallisen
  pelaajan chr_type sallii esineet (`0xBF1B30(PGD+0x8C)`). Tappajaa ei tarkisteta, vain vihollisen paikallinen tila.
- Ruumiin aarre (MSB-tapahtuma, `SprjEventDropItemInfo`): `0x9190D0`, jota WorldChrMan-päivitys `0x891AF0` kutsuu.
- **Johtopäätös:** HP-kirjoituksella tai ForceDeadilla (`0x751F20`, kutsuu chr-vtablen +280/+288) kuollut vihollinen ei käy
  normaalia kuolemapolkua tai sillä on nettilippu päällä, joten `0x9FFD50`/`0x9FFF50` jäävät ajamatta [PÄÄT].
  Lokita `chr+0x1A12` ja `0x9E5C00(chr+0x18)` kuolinhetkellä, niin syy varmistuu.

### 7b. AwardItemLot [VAR]
- EMEVD 2003[4] AwardItemLot → `0x689FF0` → `0x918810(MapItemMan, lot, 1)`.
  2003[36] AwardItemsIncludingClients → `0x68A010` → `0x918810(MapItemMan, lot, 0)`.
- `0x918810 AwardItemLot(MapItemMan*, i32 lotId, bool hostOnly)`: arpoo lotin (`0x913A70` + `0x913DB0`), antaa jokaisen
  tuloksen `0x91C970`:lla (ponnahdusikkunan kanssa) ja asettaa tuloksen getItemFlagin (`0x6AAAB0` → `0x6C4520`).
  `hostOnly=1` vaatii, että paikallinen chr_type sallii esineet. Meille: käytä `hostOnly=0`.
- Lot elävästä tai kuolleesta chr:stä: NpcParam-rivi `vt[+376](chr)+8` tai `SoloParamRepository::get::<NpcParam>(npcParamId)`;
  `item_lot_id_1..6` alkavat offsetista `+0x2C` (vendor `NPC_PARAM_ST`). Vanilla käyttää vain `_1`:tä + bonusta.

### 7c. Estääkö getItemFlagId tuplan? [VAR]
Kyllä, AwardItemLotin kautta. Arvonta (`0x9128C0`/`0x911E10`) ohittaa paikan, jos tarkistus on päällä (`0x913A70(...,1)`,
kaikki kutsujat käyttävät arvoa 1), paikan lippu ≥ 0 (`get_item_flag_idNN`, muuten koko lotin `get_item_flag_id`
offsetissa `+0x80`) ja `GetEventFlag 0x6C3E60(EventFlagMan=[0x3D55FE8], flag, 0)` on päällä. Lotit ilman lippua
(−1) antavat joka kerta uudestaan. Ne pitää suodattaa pois.

### 7d. Jälkikäteinen paikkaus
- Tarvitaan staattinen taulukko `(minibossin kuolemalippu, lotId)`, joka puretaan offline-tilassa pelin
  `event/*.emevd.dcx`-tiedostoista (AwardItemLot-kutsut, joita edeltää kuolemalipun odotus). Sitä ei voi päätellä ajossa.
- Ajossa: `if GetEventFlag(deathFlag) && lot.get_item_flag_id >= 0 && !GetEventFlag(lot.flag) → 0x918810(MIM, lot, 0)`.
  Arvonnan lipputarkistus tekee tästä idempotentin, joten tuplariski on vain lipuittomissa loteissa.

### 7e. Toteutusehdotus
1. Kun vihollinen kuolee ja tappaja ei ole paikallinen pelaaja (meidän damage-raportti / HP-peili), lisää chr-handle jonoon.
2. Frame-taskissa: hae chr handlella. Jos `chr+0x1A12 & 1 == 0` ja `& 0x10 == 0`, kutsu `0x9FFF50(chr)` (pelin oma
   pudotus, idempotentti bitin 1 kautta). Halutessa kutsu myös `0x9FFD50(chr)` (Sen ja XP, bitti 2).
3. Fallback, jos 2 kaatuu tai ei anna mitään: `0x918810(MIM, row.item_lot_id_1, 0)` suoraan (ilman maahan pudotusta).
4. Molemmat koneet tekevät tämän omassa maailmassaan, joten kumpikin saa omat pudotuksensa ja omat lippunsa (kuten ER Seamless).
5. Lokita jokainen anto: `[loot] chr=.. npc=.. lot=.. flag=.. given=..`.

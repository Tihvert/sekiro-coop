# SPEC: modin omat esineet inventaarioon (Companion's Bell ym.), Sekiro 1.06

Tila 2026-10-23. Lähde: IDA-kanta `F:\Temp\re_customitems\items.i64` (kopio `quick_re7`-kannasta), purut `F:\Temp\re_customitems\o1..o12`.
Osoitteet ovat RVA:ita (VA − 0x140000000). [VAR] = luettu purusta/tavuista. [ARV] = päätelty, varmistetaan ajon lokilla.
Liittyy: SPEC-ITEM-CLEANUP.md (nimihaku, RemoveItem), RE-ERSC.md §5 (ERSC muokkaa EquipParamGoods-rivejä ajossa).

## 0. Päätös lyhyesti

1. **Rivi:** käytetään vanillan olemassa olevaa *nimetöntä ja käyttämätöntä* EquipParamGoods-riviä (suunnitelma A). Modi kopioi
   siihen pohjaksi toimivan vanilla-kulutustavaran rivin (0xA0 tavua) ja ylikirjoittaa ~20 kenttää (§1). Uutta riviä ei lisätä.
2. **Nimi, lyhyt kuvaus ja pitkä kuvaus:** kolme detouria (`0x10AA1F0`, `0x10AA170`, `0x10AA0F0`) palauttavat meidän UTF-16-merkkijonomme
   omille id:ille. **Ikoni** on rivin `icon_id`, ja se kopioidaan vanilla-esineestä nimihaulla (§2).
3. **Käyttö:** detour `0xA262D0` (PlayerIns::OnGoodsUsed, vtable-paikka 125) → atomic-lippu → frame-task ajaa toiminnon (§3).
4. **Anto:** `0x91C970` hiljaisessa tilassa (`a4=1`, out-puskuri), jolloin ponnahdusikkuna ohitetaan. Annetaan vain, jos esinettä ei ole (`0x79D7E0`) (§4).
5. Esineet ovat vain co-op-tallennuksessa (`S0000.coop.sl2`). Vanilla ei lataa sitä. Siivous `0x67B230`:lla, kun modi poistetaan (§5).

## 1. Esinerivi

### 1.1 Rivin valinta (suunnitelma A, ajonaikainen haku + kovakoodaus ensimmäisen lokin jälkeen)
- Lookup `0x10BF630 GetEquipParamGoods(out{i32 id; u32; row*}, i32 paramId)` [VAR]: binäärihaku lajiteltuun `(id, index)`-tauluun,
  joka on param-tiedoston perässä (`file + align16(*(u32*)(file-16))`, rivimäärä `*(u16*)(file+10)`). Kaikki noin 104 pelin kutsujaa käyttävät tätä funktiota [VAR].
- **Ehdokas** = rivi on olemassa JA `GetGoodsName(repo,id) == base+0x2B955E8` ("?GoodsName?") JA `GetGoodsInfo(repo,id) == base+0x2B95600`
  ("?GoodsInfo?") JA mikään ItemLotParam- tai ShopLineupParam-rivi ei viittaa siihen (Goods-kategoria) JA id ∉ {102, 111}.
  Id:t 102 ja 111 on kovakoodattu `0xA262D0`:ssa ja `0x7A0E50`:ssa [VAR].
- Vaihe 0 (ensimmäinen ajo): lokita kaikki ehdokkaat. Valitse kolme peräkkäistä korkeaa id:tä, ja kovakoodaa ne vakioiksi
  `ITEM_BELL`, `ITEM_SEVER` ja `ITEM_WAKE`. Ajossa varmistetaan joka kerta, että ehdot pätevät edelleen. Jos eivät päde, esineitä ei oteta käyttöön ja lokiin tulee virhe.
- **Suunnitelma B** (jos ehdokkaita ei löydy): detour `0x10BF630` palauttaa staattisen 0xA0-tavuisen rivin omalle id:lle, kun
  oikea haku antaa `row==0`. Riski [ARV]: koodipolut, jotka eivät käytä `0x10BF630`:aa (vendorin `rows()`/`find_index`, `0x10CE0E0`-suorahaut,
  inventaarion lajittelu), eivät näe riviä. Ilman modia esine on orpo ("?GoodsName?", punainen ikoni, sama kuin nyt).
- **Suunnitelma C** (ERSC-tyyli, ei nyt): uusi param-tiedosto + `(id,index)`-taulu, osoitin `*(*(res+112)+112)` vaihdetaan. Vaikea, ja
  muut välimuistiin tallennetut osoittimet ovat tuntemattomia.

### 1.2 Pohjarivi (template)
- Pohjaksi kopioidaan koko rivi vanilla-esineestä, jolla on lyhyt käyttöanimaatio ja SpEffect-käyttäytyminen. Ehdokkaat nimihaulla
  (englanniksi, `GetGoodsName`): "Pellet", sitten "Ako's Sugar" [ARV]. Asetus `custom_items_template_id` ohittaa haun.
  Homeward Idolia ei käytetä: sen animaatio on pitkä ja warp-bitti on päällä.
- Näin `goods_type`, `goods_use_anim` (+0x3E, HKS lukee `0x9EFBA0`:ssa [VAR]), `is_equip` ja tilabitit (`+0x6B` bitit 3–7, joita
  CanUse `0x8522B0` lukee [VAR]) ovat valmiiksi oikein Quick Items -käytölle.

### 1.3 Ylikirjoitettavat kentät (offsetit `EQUIP_PARAM_GOODS_ST`, vendor generated.rs; koko 0xA0) [VAR offsetit]
| Offset | Kenttä | Arvo | Peruste |
|---|---|---|---|
| 0x00 | ref_id_default | -1 | ei efektiä |
| 0x48 | ref_id_1 | -1 | |
| 0x3B | ref_category | 0 | `0x9E4CD0`: 1 = bullet, 2 = SpEffect, muu → `return 1` ilman efektiä, joten käyttö onnistuu [VAR] |
| 0x2C | icon_id | ikonilähteestä (§2.3) | |
| 0x1C | sort_id | template + 1..3 | sijoittuu pohjan viereen [ARV] |
| 0x36 | max_num | 1 | |
| 0x6C | max_repository_num | 0 | ei varastoon |
| 0x0C / 0x10 / 0x70 | basic_price / sell_value / sale_value | 0 / 0 / -1 | ei arvoa [ARV: Sekirossa ei myyntiä] |
| 0x38, 0x84 | consume_hero_point, consume_hp_rate | 0, 0.0 | |
| 0x74–0x76 | consume_resource_item_a/b/c | 0 | CanUse → `0x84DD10` lukee +116..118 [VAR] |
| 0x44 | bitit: live, gray, white, black, multi = 1; disable_offline = 0; is_equip = template; **is_consume = 0** (bit7) | | `0x7A0E50`: `row+0x44 >= 0` → määrä ei vähene [VAR] |
| 0x45 | is_auto_equip = 1 [ARV, valinnainen], is_only_one = 1, **is_discard = 0** (bit3), **is_deposit = 0** (bit4), supple = 0 | | `0x84DFD0`/`0x84DE50` [VAR] |
| 0x46 | is_full_supple_item = 0 (bit0), is_enhance = 0 (bit1) | | `0x7A0E50` +70&1, `0x9E4FE0`/`0x8522B0` +70&2 [VAR] |
| 0x68 | is_bonfire_warp_item = 0 (bit1), is_use_ref_id1_for_enchant = 0 (bit5) | | |
| 0x6B | **is_drop = 0** (bit0), **is_notice_log = 0** (bit1), is_notice_dialog = 0 (bit2), bitit 3–7 = template | | `0x84E200`, `0x84C010` [VAR] |
| 0x50/0x54/0x58 | vagrant_* lots | -1 | |
| 0x7C | first_get_event_flag_id | -1 | |
| 0x82 | use_get_item | 0 | `0xA262D0` antaisi muuten `+0x88` esineen [VAR] |
| 0x8E–0x97 | use_limit_sp_effect_type0..4 | 0 | [ARV] |

- Kirjoitus: `SoloParamRepository [0x3D978B0]` → vendor `get_mut::<EquipParamGoods>(id)`. Kirjoita frame-taskissa, kun repository on
  ladattu. Toista idempotentisti 5 s välein (vertaa ensin, kirjoita vain jos eroaa). Parametrit ovat vain muistissa, joten tiedostoa ei muuteta.
- Loki: `[items] row id=N template=T icon=I consume=0 drop=0 refcat=0 ok`.

## 2. Nimi, kuvaus ja ikoni

### 2.1 Funktiot [VAR]
| RVA | Mitä | FMG-pankit | Vakio, jos puuttuu |
|---|---|---|---|
| `0x10AA1F0` | GoodsName | 111, 10, 210, 250 | `0x2B955E8` "?GoodsName?" |
| `0x10AA170` | GoodsInfo (lyhyt kuvaus) | 110, 20, 217, 257 | `0x2B95600` "?GoodsInfo?" |
| `0x10AA0F0` | GoodsCaption (pitkä kuvaus) | 100, 24, 221, 261 | `0x2B95618` "?GoodsCaption?" |
- Allekirjoitus: `const wchar_t* f(MsgRepositoryImp* repo /*[0x3D964C8]*/, u32 paramId)`. Id on ilman kategoriabittejä (`0x913C60`
  välittää `a2` sellaisenaan) [VAR].
- Kutsujat [VAR]: Name ← `0x913C60`, `0xAAD450`, `0x10AC790`, `0x10BF520`. Info ← `0x10BF4D0`. Caption ← `0x10BF480`. `MsgRepo_Lookup 0x2603620`
  kutsutaan Goods-pankeille vain näistä käärefunktioista [VAR], joten kolme detouria kattaa kaiken.
- **Prologi (sama kaikissa kolmessa, 10 tavua, ei RIP-relatiivinen) [VAR]:** `48 89 5C 24 08 57 48 83 EC 20`
  (`mov [rsp+8],rbx; push rdi; sub rsp,20h`). `hook::install_detour(rva, &PROLOGUE, detour, name)`.

### 2.2 Detour
```
extern "C" fn goods_name_detour(repo: usize, id: u32) -> *const u16 {
    if let Some(s) = custom::name(id) { return s.as_ptr(); }   // &'static [u16], nollapäätteinen
    call_original(&NAME_TRAMP, repo, id)
}
```
Merkkijonot ovat `static`-UTF-16-taulukoita (esim. `wide!("Companion's Bell\0")`). Ne eivät saa koskaan vapautua.
Rivinvaihto kuvauksessa on `\n` [ARV, FMG käyttää samaa]. Ehdotetut tekstit:
- Companion's Bell. Info: "Ring to travel to your companion." Caption: "A small bell bound by a promise. Its sound always reaches
  the one you travel with.\n\nUse to travel to your co-op companion."
- Severing Bell (valinnainen). Info: "Ring to leave the shared world."

### 2.3 Ikoni
- Ikoni on rivin `icon_id` (+0x2C). Haetaan ajossa: käydään EquipParamGoods-rivit läpi (`rows::<EquipParamGoods>()`) ja verrataan
  `GetGoodsName`-tulosta englanninkieliseen nimeen. Companion's Bell → "Bell Charm" (pieni kello) [ARV nimi]. Severing Bell → "Bell Demon"
  [ARV nimi]. Varalla on asetus `custom_items_icon = [bell, sever]`.
- Loki: `[items] icon source 'Bell Charm' id=N icon=I`. Jos nimeä ei löydy (eri kieli), käytetään asetusta tai templaten ikonia.

## 3. Käytön tunnistus

### 3.1 Ketju [VAR]
1. `0xB57740` (TAE-tapahtuman käsittelijä, `a1+16` = chr): nykyinen pikaesine `0x8D6230(chr)` = `*(u32*)(chr+0x11C4)`. Silmukka
   `chr+0x11C8` kertaa: `0x9F3890` (CanUse → `0x84EE80` → `0x8522B0`), sitten `0x7D5D50`.
2. `0x7D5D50(ChrBehavior* = chr+0x10E0, i32 paramId, evt*, i32, bool)` → `0x9E44F0` → `0x9E4CD0`. Behavior-slot on vakio 2000002
   (`0x7D6510`). Rivin `ref_category` valitsee bullet- tai SpEffect-polun, ja 0 palauttaa 1 ilman efektiä.
3. Onnistuessa kutsutaan `chr->vt[125]` (+1000). PlayerIns-vtable `0x2A2B338` → **`0xA262D0 OnGoodsUsed(PlayerIns*, u32* fullId)`**:
   kulutus `0x79AFA0(PGD+0x278, &id, debugInfinite, 0)` → `0x79AB70` → `0x7A0E50` (is_consume-tarkistus), lopuksi use_get_item.
   Sama vtable-kutsu tulee myös polusta `0x848B40` ← `0xBDF560`.

### 3.2 Hook (ensisijainen)
- `0xA262D0`, **prologi 6 tavua [VAR]:** `40 57 48 83 EC 40` (`push rdi; sub rsp,40h`). Paluuarvo on `char`.
- Detour: kutsu ensin alkuperäistä. Sitten, jos `a1 == game::player()`-osoitin JA `(*a2 & 0xF0000000) == 0x40000000` JA
  `*a2 & 0x0FFFFFFF ∈ {ITEM_BELL, ITEM_SEVER, ITEM_WAKE}`, aseta `USE_PENDING.fetch_or(bit)`. Ei lukkoja, ei lokia, ei warppia detourissa.
- Frame-task: `swap(0)`. Bell → `ui::post(|c| c.go_to_friend = true)` (sama kuin F9; `join.rs::go_to_friend`, kulkee `safe_to_warp`-portin kautta).
  Sever → uusi `leave_session`-komento (`steam.rs::leave`) [ARV, toteutus erikseen]. Jäähy 3 s per esine.
- Loki: `[items] used id=N (Companion's Bell) -> go_to_friend` / `[items] use ignored: not connected`.
- Jos kaveria ei ole yhdistetty, näytä ilmoitus `bossrules::notice("No companion connected", 3)`.

### 3.3 Varatie
Jos 3.2 ei laukea testissä T4, hookataan esikäsittelynä `0x7D5D50`. **Prologi 9 tavua [VAR]:** `40 55 53 56 57 41 54 41 55`.
`a2` = paramId, chr = `*(usize*)(a1+0x98)`. Kutsutaan alkuperäistä ja lipun asetus tehdään vain, kun paluuarvo != 0. SpEffect-pollausta
ei suositella: se vaatisi vapaan SpEffect-rivin, ja `0x9E9D10`:n käytös id:llä −1 on selvittämättä.

### 3.4 Wake katsojalle
Katsoja on kuollut (kuolemaruutu pidätetty, `bossrules` SPEC_WAITING). Kuollut PlayerIns ei aja HKS-esineanimaatiota, joten käyttö
ei pääse `0xB57740`:aan [ARV]. **Wakea ei tehdä esineenä.** Pidetään overlay/F8 (`Commands::wake`). Rivi ITEM_WAKE varataan, mutta sitä ei anneta.

## 4. Anto kerran

- `0x91C970 MapItemMan_GrantItems(MapItemMan* [0x3D6CDC0], ItemBuffer* buf, void* a3_unused, bool a4_silent, u32* a5_out, bool a6)` [VAR]:
  - `ItemBuffer = { u32 count; entry[count] }`, `entry = { u32 id = 0x40000000|param; u32 qty = 1; i32 dur = -1; u32 0 }` (vendor ItemBufferEntry).
  - **`a4=1` ja `a5 != null`**: tulokset kirjoitetaan `a5`:een (`a5[0]` = määrä, merkinnät 16 tavua alkaen +4), eikä ponnahdus-
    ja ilmoituspolkuja (`0x91FB70`, `0xC3EBE0 [0x3D88928]`) ajeta. Ne ovat vain `a4==0`-haarassa [VAR]. Puskurille ei ole kokoparametria:
    varaa 0x100 nollattua tavua. `a6=0`.
  - Lisäksi `is_notice_log=0` estää `0x91FB70`:n myös `a4=0`-tilassa (`0x84C010` palauttaa `row+0x6B & 2`) [VAR].
  - Vendorin `grant_items` käyttää `a4=false`, joten **tehdään oma kutsu**: `f(mim, buf, 0, true, out.as_mut_ptr(), false)`.
- **Onko jo inventaariossa:** `0x79D7E0(EquipGameData* = PGD+0x278, u32* fullId) -> i32 index` (= `0x7A7AF0(EGD+0x168, id)`), jossa
  < 0 = ei ole [VAR]. Vaihtoehtoisesti vendorin `InventoryItemsData::items()`, kuten SPEC-ITEM-CLEANUP §2.
- Milloin: co-op-tallennus ladattu, PGD != null, ei latausta eikä menua, rivi patchattu (§1) ja nimihookit asennettu. Kerran per lataus
  tarkista ja anna puuttuvat (`ITEM_BELL`, valinnaisesti `ITEM_SEVER`). Koska `is_discard=0`, `is_drop=0` ja `max_repository_num=0`,
  pelaaja ei voi hävittää esinettä. Uusi anto tulee vain, jos se poistetaan.
- Loki: `[items] give id=0x40XXXXXX qty=1 out_count=K` / `[items] has id=… idx=I`.

## 5. Riskit

- **Tallennus:** esineet ovat vain `S0000.coop.sl2`:ssa (world.rs). Jos tiedosto ladataan ilman modia, rivi on vanillassa olemassa
  (suunnitelma A): ei kaatumista eikä punaista ikonia. Nimi näkyy "?GoodsName?", ikoni on vanillan, ja käyttö ajaa vanilla-dummyrivin
  käyttäytymisen [ARV]. Suunnitelmassa B esine on orpo kuten nykyiset, ja SPEC-ITEM-CLEANUP siivoaa sen.
- **Poisto:** asetus `custom_items = "on" | "remove"`. `remove` ajaa kerran `0x67B230 RemoveItem(0, 0x40000000, id, 1)` jokaiselle omalle
  id:lle ja lokittaa. Poistoa modin sulkeutuessa ei tehdä, koska se ei ole luotettavaa kaatumisessa. Seuraava autosave tallentaa poiston.
- **Dummy-rivi voi olla pelin käytössä:** ehdot §1.1 (ei nimeä, ei lottia, ei kauppaa) + kovakoodatut 102/111 pois. Loppuriski [ARV]: EMEVD antaa
  rivin suoraan. Testaa ne alueet, joissa pelaaja on.
- **Detourit kuumilla poluilla:** nimihaut ajetaan joka menuruudulla. Detourin pitää olla O(1) (`match id`). OnGoodsUsed ajetaan pääsäikeessä, joten käytä vain atomiceja.
- **Etä-PlayerIns** (natiivi remote) voi ajaa saman vtable-polun. Siksi osoitinvertailu `game::player()`:iin on pakollinen.
- **Versio:** kaikki hookit tarkistavat prologitavut (`install_detour`). Jos ne eivät täsmää, hookkia ei asenneta, toiminto on pois
  päältä ja esinettä ei anneta.
- **Kieli:** nimihaku (§1.2, §2.3) toimii vain englanniksi. Muille kielille asetuksen id:t.

## 6. Testi (ei pelin käynnistystä tässä vaiheessa; ajetaan myöhemmin, kaksi konetta)

| # | Toimenpide | Odotus / loki |
|---|---|---|
| T0 | ensimmäinen ajo, `custom_items = "scan"` | `[items] candidate id=…` -lista, valitaan id:t |
| T1 | lataa co-op-tallennus | `[items] row … ok` ×2, `[items] give …` kerran, ei ponnahdusta |
| T2 | avaa Inventory → Goods | oikea nimi, lyhyt ja pitkä kuvaus, kelloikoni, määrä 1 |
| T3 | lisää Quick Items -paikkaan, käytä | lyhyt animaatio, `[items] used … -> go_to_friend`, warp kaverin luo |
| T4 | käytä 5× peräkkäin | määrä pysyy 1:nä (ei kulu), jäähy estää tuplawarpin |
| T5 | yritä pudottaa tai varastoida | ei vaihtoehtoa |
| T6 | lataa uudelleen | `[items] has …`, ei uutta antoa |
| T7 | kaveri käyttää omaa kelloaan | vain hänen koneensa lokissa `used`, ei minun |
| T8 | käytä templatea (Pellet) | toimii kuten ennen (parantaa, kuluu) |
| T9 | `custom_items = "remove"` | esineet poistuvat, `[items] removed id=… qty_after=0` |

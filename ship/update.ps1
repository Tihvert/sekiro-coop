# Testiversio: hakee hostilta uusimmat tiedostot ennen pelin kaynnistysta: sekiro_coop.dll,
# play.cmd, update.ps1, upload-log.ps1, sekiro-coop.me3 ja OHJE.txt. Ei koskaan asetustiedostoa
# (sekiro_coop_settings.ini) eika test_server.txt:a. Hostin osoite = test_server.txt:n eka rivi
# (ip tai ip:portti) tai UDP-yhteydella sekiro_coop_settings.ini:n "udp_host_address".
# Host listaa tiedostot (GET /manifest: nimi, sha256, koko); vain eroavat ladataan, tarkistetaan
# (sha256 + koko) ja vasta sitten otetaan kayttoon. Jos ei onnistu, jatketaan nykyisilla.
# play.cmd kirjoitetaan nimella play.cmd.new (cmd lukee ajettavaa .cmd:ta rivi kerrallaan, joten
# sita ei saa korvata kesken ajon); play.cmd vaihtaa sen paikalleen lopussa. Jos peli pitaa DLL:aa
# lukittuna, uusi DLL jaa kansioon update\ ja play.cmd ottaa sen kayttoon seuraavalla kaynnistyksella.
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ini = Join-Path $dir 'sekiro_coop_settings.ini'
$srv = Join-Path $dir 'test_server.txt'
$legacy = Join-Path $env:LOCALAPPDATA 'sekiro-coop\coop.ini'
$allowed = @('sekiro_coop.dll', 'play.cmd', 'update.ps1', 'upload-log.ps1', 'laheta-lokit.cmd', 'stop-me3.ps1', 'sekiro-coop.me3', 'OHJE.txt')
function Get-Setting($path, $key) {
  if (-not (Test-Path $path)) { return '' }
  $m = Select-String -Path $path -Pattern ('^\s*' + $key + '\s*=\s*(\S*)') | Select-Object -First 1
  if ($m) { return $m.Matches[0].Groups[1].Value }
  return ''
}
function Get-Sha($path) {
  # ReadWrite sharing: also works while the game has the DLL mapped.
  $fs = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
  try { return ([BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($fs)) -replace '-', '').ToLower() }
  finally { $fs.Close() }
}
# Downloads $name to $tmp and checks size + sha256 (+ MZ header for the DLL). Throws on mismatch.
function Get-Verified($base, $name, $tmp, $sha, $size) {
  Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
  Invoke-WebRequest -Uri ($base + '/file/' + $name) -OutFile $tmp -TimeoutSec 60 -UseBasicParsing
  $len = (Get-Item -LiteralPath $tmp).Length
  if ($len -ne $size) { throw ('koko {0} != {1}' -f $len, $size) }
  if ((Get-Sha $tmp) -ne $sha) { throw 'sha256 ei tasmaa' }
  if ($name -eq 'sekiro_coop.dll') {
    $head = [IO.File]::ReadAllBytes($tmp)[0..1]
    if ($head[0] -ne 0x4D -or $head[1] -ne 0x5A) { throw 'ladattu tiedosto ei ole DLL' }
  }
}
try {
  # Host address: test_server.txt first, else the UDP host address (old behaviour).
  $addr = ''
  if (Test-Path $srv) { $addr = (Get-Content $srv -TotalCount 1).Trim() }
  if ($addr) {
    if ($addr -notmatch ':') { $addr = $addr + ':28101' }
  } else {
    if ((Get-Setting $ini 'transport').ToLower() -eq 'steam') {
      Write-Host 'Paivitys: ei hostin osoitetta (transport = steam, ei test_server.txt:a). Jatketaan nykyisella versiolla.'
      exit 0
    }
    $udp = Get-Setting $ini 'udp_host_address'
    if (-not $udp -and -not (Test-Path $ini)) { $udp = Get-Setting $legacy 'host' }
    if (-not $udp) {
      Write-Host 'Paivitys: ei hostin osoitetta (test_server.txt tai udp_host_address), jatketaan.'
      exit 0
    }
    $addr = $udp.Split(':')[0] + ':28101'
  }
  $base = 'http://' + $addr

  # Manifest. A host running an older serve.ps1 has none: then only the DLL, as before.
  $manifest = $null
  try {
    $manifest = (Invoke-WebRequest -Uri ($base + '/manifest') -TimeoutSec 6 -UseBasicParsing).Content
    if ($manifest -is [byte[]]) { $manifest = [Text.Encoding]::ASCII.GetString($manifest) }
  } catch {
    $manifest = $null
  }
  $items = @()
  if ($manifest) {
    foreach ($line in ($manifest -split "`n")) {
      if ($line.Trim() -match '^([A-Za-z0-9_.-]+) ([0-9a-fA-F]{64}) (\d+)$' -and ($allowed -contains $Matches[1])) {
        $items += , @($Matches[1], $Matches[2].ToLower(), [int64]$Matches[3])
      }
    }
  }
  if ($items.Count -eq 0) {
    # Legacy: the old server only knows /sekiro_coop.dll (no hash list, so compare by content).
    $dll = Join-Path $dir 'sekiro_coop.dll'
    $tmp = Join-Path $env:TEMP 'sekiro_coop_update.dll'
    Invoke-WebRequest -Uri ($base + '/sekiro_coop.dll') -OutFile $tmp -TimeoutSec 20 -UseBasicParsing
    $b = [IO.File]::ReadAllBytes($tmp)
    if ($b.Length -lt 100000 -or $b[0] -ne 0x4D -or $b[1] -ne 0x5A) { throw 'ladattu tiedosto ei ole DLL' }
    if ((Get-Sha $tmp) -ne (Get-Sha $dll)) {
      Copy-Item $tmp $dll -Force
      Write-Host 'Paivitetty: sekiro_coop.dll'
    } else {
      Write-Host 'Paivitys: kaikki ajan tasalla.'
    }
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
    exit 0
  }

  $updated = @(); $staged = @(); $failed = @()
  foreach ($it in $items) {
    $name = $it[0]; $sha = $it[1]; $size = $it[2]
    $path = Join-Path $dir $name
    try {
      if ($name -eq 'play.cmd') {
        # Never overwrite the running play.cmd: write play.cmd.new, play.cmd swaps it in at the end.
        $new = $path + '.new'
        if ((Test-Path $path) -and ((Get-Sha $path) -eq $sha)) {
          Remove-Item -LiteralPath $new -Force -ErrorAction SilentlyContinue
          continue
        }
        if ((Test-Path $new) -and ((Get-Sha $new) -eq $sha)) { continue }
        $tmp = $new + '.tmp'
        Get-Verified $base $name $tmp $sha $size
        Move-Item -LiteralPath $tmp -Destination $new -Force
        $updated += $name
        continue
      }
      if ((Test-Path $path) -and ((Get-Sha $path) -eq $sha)) { continue }
      if ($name -eq 'sekiro_coop.dll') {
        $stg = Join-Path $dir 'update\sekiro_coop.dll'
        $tmp = Join-Path $env:TEMP 'sekiro_coop_update.dll'
        Get-Verified $base $name $tmp $sha $size
        try {
          Copy-Item -LiteralPath $tmp -Destination $path -Force -ErrorAction Stop
          Remove-Item -LiteralPath $stg -Force -ErrorAction SilentlyContinue
          $updated += $name
        } catch {
          # The game holds the DLL: stage it, play.cmd applies it on the next start.
          New-Item -ItemType Directory -Force -Path (Join-Path $dir 'update') | Out-Null
          Copy-Item -LiteralPath $tmp -Destination $stg -Force
          $staged += $name
        }
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        continue
      }
      # update.ps1 itself is already loaded by PowerShell; temp + move keeps the file whole.
      $tmp = $path + '.tmp'
      Get-Verified $base $name $tmp $sha $size
      Move-Item -LiteralPath $tmp -Destination $path -Force
      $updated += $name
    } catch {
      $failed += ('{0} ({1})' -f $name, $_.Exception.Message)
      Remove-Item -LiteralPath ($path + '.tmp') -Force -ErrorAction SilentlyContinue
      Remove-Item -LiteralPath ($path + '.new.tmp') -Force -ErrorAction SilentlyContinue
    }
  }
  if ($updated.Count) { Write-Host ('Paivitetty: ' + ($updated -join ', ')) }
  if ($staged.Count) { Write-Host 'Paivitys: uusi sekiro_coop.dll odottaa (peli pitaa sita auki), otetaan kayttoon seuraavalla play.cmd-kaynnistyksella.' }
  if ($failed.Count) { Write-Host ('Paivitys: ei onnistunut: ' + ($failed -join '; ') + ' - jatketaan nykyisilla.') }
  if (-not $updated.Count -and -not $staged.Count -and -not $failed.Count) { Write-Host 'Paivitys: kaikki ajan tasalla.' }
} catch {
  Write-Host ('Paivitys: ei onnistunut ({0}), jatketaan nykyisella versiolla.' -f $_.Exception.Message)
}

# v8.56: one-time save fixes from the host (GET /savefix, needs the co-op password key). A line is
# "<id> <player_name> <slot> <offset hex> <add>": once per id (savefix-done.txt), only for the
# named player, only while Sekiro is closed, adds <add> to the u32 at <offset> of that slot of
# the newest S0000.coop.sl2 (capped at 999999999), keeps the slot's MD5 right and copies the
# save to save-backups first. Used for sen lost to a mod bug (2026-10-27 Divine Dragon).
function Invoke-SaveFix($base) {
  $ld = Join-Path $env:LOCALAPPDATA 'sekiro-coop'
  $done = Join-Path $ld 'savefix-done.txt'
  $pw = ''
  $m = Select-String -Path $ini -Pattern '^\s*password\s*=\s*(.*?)\s*$' -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($m) { $pw = $m.Matches[0].Groups[1].Value }
  $sha = [Security.Cryptography.SHA256]::Create()
  $key = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('sekiro-coop-log:' + $pw))) -replace '-', '').Substring(0, 16)
  try {
    $txt = (Invoke-WebRequest -Uri ($base + '/savefix') -Headers @{ 'X-Coop-Key' = $key } -TimeoutSec 6 -UseBasicParsing).Content
    if ($txt -is [byte[]]) { $txt = [Text.Encoding]::ASCII.GetString($txt) }
  } catch { return }
  $me = Get-Setting $ini 'player_name'
  if (-not $me) { $me = $env:USERNAME }
  $applied = @(if (Test-Path $done) { Get-Content $done })
  foreach ($line in ($txt -split "`n")) {
    if ($line.Trim() -notmatch '^([A-Za-z0-9_-]+) (\S+) (USER_DATA\d{3}) 0x([0-9A-Fa-f]+) (\d+)$') { continue }
    $id = $Matches[1]; $who = $Matches[2]; $slotName = $Matches[3]; $off = [Convert]::ToInt64($Matches[4], 16); $add = [int64]$Matches[5]
    if ($who -ne $me -or $applied -contains $id) { continue }
    if (Get-Process -Name sekiro -ErrorAction SilentlyContinue) { Write-Host 'Tallennuskorjaus: peli on auki, tehdaan seuraavalla kerralla.'; return }
    try {
      $sd = Join-Path $env:APPDATA 'Sekiro'
      $save = Get-ChildItem $sd -Recurse -Filter 'S0000.coop.sl2' -ErrorAction Stop | Sort-Object LastWriteTime -Descending | Select-Object -First 1
      if (-not $save) { throw 'S0000.coop.sl2 puuttuu' }
      $d = [IO.File]::ReadAllBytes($save.FullName)
      if ([Text.Encoding]::ASCII.GetString($d, 0, 4) -ne 'BND4') { throw 'ei BND4' }
      $cnt = [BitConverter]::ToInt32($d, 0x0C); $hsize = [BitConverter]::ToInt64($d, 0x20)
      $slot = $null
      for ($i = 0; $i -lt $cnt; $i++) {
        $o = 0x40 + $i * $hsize
        $size = [BitConverter]::ToInt64($d, $o + 8); $doff = [BitConverter]::ToUInt32($d, $o + 16); $noff = [BitConverter]::ToUInt32($d, $o + 20)
        $j = $noff; while ($d[$j] -ne 0 -or $d[$j + 1] -ne 0) { $j += 2 }
        if ([Text.Encoding]::Unicode.GetString($d, $noff, $j - $noff) -eq $slotName) { $slot = @($doff, $size) }
      }
      if (-not $slot) { throw "$slotName puuttuu" }
      $doff = [int64]$slot[0]; $size = [int64]$slot[1]
      $md5 = [Security.Cryptography.MD5]::Create()
      $sum = $md5.ComputeHash($d, $doff + 16, $size - 16)
      for ($k = 0; $k -lt 16; $k++) { if ($sum[$k] -ne $d[$doff + $k]) { throw 'tarkistussumma ei tasmaa' } }
      if ($off + 4 -gt $size - 16) { throw 'offset yli' }
      $at = $doff + 16 + $off
      $old = [BitConverter]::ToUInt32($d, $at)
      $new = [Math]::Min([int64]$old + $add, 999999999)
      $bk = Join-Path $ld 'save-backups'
      New-Item -ItemType Directory -Force -Path $bk | Out-Null
      Copy-Item -LiteralPath $save.FullName -Destination (Join-Path $bk ('S0000.coop.{0}-savefix-{1}.sl2' -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $id)) -Force
      [BitConverter]::GetBytes([uint32]$new).CopyTo($d, $at)
      $md5.ComputeHash($d, $doff + 16, $size - 16).CopyTo($d, $doff)
      [IO.File]::WriteAllBytes($save.FullName, $d)
      New-Item -ItemType Directory -Force -Path $ld | Out-Null
      Add-Content -Path $done -Value $id
      Write-Host ('Tallennuskorjaus {0}: {1} -> {2} (varmuuskopio save-backups-kansiossa).' -f $id, $old, $new)
    } catch {
      Write-Host ('Tallennuskorjaus {0} ei onnistunut ({1}), tallennukseen ei koskettu.' -f $id, $_.Exception.Message)
    }
  }
}
try {
  $fixAddr = ''
  if (Test-Path $srv) { $fixAddr = (Get-Content $srv -TotalCount 1).Trim() }
  if ($fixAddr) {
    if ($fixAddr -notmatch ':') { $fixAddr = $fixAddr + ':28101' }
    Invoke-SaveFix ('http://' + $fixAddr)
  }
} catch {}

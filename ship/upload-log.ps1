# Testiversio: kun peli on suljettu, lahettaa lokin (log.txt, pakattuna log.old.txt + log.txt, varalla 8 Mt lopusta) hostin
# testipalvelimelle (test_server.txt tassa kansiossa). Ei tee mitaan ilman test_server.txt:a.
# Palvelin hyvaksyy lokin vain samalla co-op-salasanalla (vain sen tiiviste lahetetaan).
# laheta-lokit.cmd ajaa taman: -Now = ei odoteta pelin sulkeutumista, -Zip = tee lisaksi pienen zipin
# (lokit + tallennus + tila) tyopoydalle, jonka voi lahettaa kasin (esim. Discordiin). Zip ei sisalla
# asetustiedostoa eika salasanaa.
param([switch]$Now, [switch]$Zip)
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
# v8.53: the DLL writes a new play.cmd as play.cmd.new while the game runs. An old play.cmd has no
# swap step, so this does it too once the game has closed (never with -Now: play.cmd may still be
# running): a detached cmd waits ~20 s (play.cmd has ended by then) and moves it in place. The new
# play.cmd swaps as well; whichever runs first wins, the other finds no file.
function Swap-PlayCmd {
  if ($Now) { return }
  $newPlay = Join-Path $dir 'play.cmd.new'
  if (-not (Test-Path $newPlay)) { return }
  $playPath = Join-Path $dir 'play.cmd'
  Start-Process -WindowStyle Hidden cmd -ArgumentList ('/c ping -n 21 127.0.0.1 >nul & move /y "{0}" "{1}" >nul' -f $newPlay, $playPath)
}
$srv = Join-Path $dir 'test_server.txt'
$haveSrv = Test-Path $srv
if (-not $haveSrv -and -not $Zip) { exit 0 }
$addr = ''
if ($haveSrv) { $addr = (Get-Content $srv -TotalCount 1).Trim() }
if (-not $addr -and -not $Zip) { exit 0 }
if ($addr -and $addr -notmatch ':') { $addr = $addr + ':28101' }
$ini = Join-Path $dir 'sekiro_coop_settings.ini'
function Get-Setting($path, $key) {
  if (-not (Test-Path $path)) { return '' }
  $m = Select-String -Path $path -Pattern ('^\s*' + $key + '\s*=\s*(.*?)\s*$') | Select-Object -First 1
  if ($m) { return $m.Matches[0].Groups[1].Value }
  return ''
}
function New-LogZip {
  try {
    Add-Type -AssemblyName System.IO.Compression
    $name = Get-Setting $ini 'player_name'
    if (-not $name) { $name = $env:USERNAME }
    $safe = $name -replace '[^A-Za-z0-9_-]', '_'
    $out = Join-Path ([Environment]::GetFolderPath('Desktop')) ('sekiro-coop-lokit-{0}-{1}.zip' -f $safe, (Get-Date -Format 'yyyyMMdd-HHmm'))
    $files = New-Object System.Collections.ArrayList
    $ld = Join-Path $env:LOCALAPPDATA 'sekiro-coop'
    foreach ($f in @('log.txt', 'log.old.txt', 'coop_state.json')) {
      $p = Join-Path $ld $f
      if (Test-Path $p) { [void]$files.Add(@($p, $f)) }
    }
    $bk = Join-Path $ld 'save-backups'
    $nb = if (Test-Path $bk) { Get-ChildItem $bk -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike '*.bak' -and $_.Name -notlike 'MANUAL*' } | Sort-Object LastWriteTime -Descending | Select-Object -First 1 } else { $null }
    if ($nb) { [void]$files.Add(@($nb.FullName, ('backup-' + $nb.Name))) }
    $sd = Join-Path $env:APPDATA 'Sekiro'
    $cs = if (Test-Path $sd) { Get-ChildItem $sd -Recurse -Filter 'S0000.coop.sl2' -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1 } else { $null }
    if ($cs) { [void]$files.Add(@($cs.FullName, 'S0000.coop.sl2')) }
    if ($files.Count -eq 0) { Write-Host 'Zip: ei loytynyt mitaan pakattavaa.'; return }
    $fs = [IO.File]::Open($out, 'Create')
    $zip = New-Object IO.Compression.ZipArchive($fs, [IO.Compression.ZipArchiveMode]::Create)
    foreach ($e in $files) {
      $entry = $zip.CreateEntry($e[1], [IO.Compression.CompressionLevel]::Optimal)
      $es = $entry.Open()
      $src = [IO.File]::Open($e[0], 'Open', 'Read', 'ReadWrite')
      try { $src.CopyTo($es) } finally { $src.Dispose(); $es.Dispose() }
    }
    $zip.Dispose(); $fs.Dispose()
    Write-Host ('Zip valmis: {0} ({1:N1} Mt)' -f $out, ((Get-Item $out).Length / 1MB))
    Write-Host 'Laheta se kasin (esim. Discordiin) hostille.'
    try { Start-Process explorer.exe -ArgumentList ('/select,"' + $out + '"') } catch { }
  } catch {
    Write-Host ('Zip epaonnistui: {0}' -f $_.Exception.Message)
  }
}
try {
  if ($Zip) { New-LogZip }
  if (-not $addr) { Write-Host 'test_server.txt puuttuu: automaattista lahetysta ei tehda, laheta zip kasin.'; exit 0 }
  if (-not $Now) {
    # me3 may return before the game has closed: wait for sekiro.exe to exit (max 12 h).
    $deadline = (Get-Date).AddHours(12)
    Start-Sleep -Seconds 5
    while ((Get-Process -Name sekiro -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 3 }
  }
  # The friend's co-op save and state go to the host too (so a lost item such as the Spirit Emblems
  # can be repaired by hand): S0000.coop.sl2 (newest under %APPDATA%\Sekiro\<id>\), coop_state.json
  # and the newest save backup. Gzipped, POST /file.gz, the host unpacks. A failure here never
  # stops the log upload below.
  function Send-File($path, $label) {
    try {
      if (-not $path -or -not (Test-Path $path)) { return }
      $pw0 = Get-Setting $ini 'password'
      $sha0 = [Security.Cryptography.SHA256]::Create()
      $key0 = ([BitConverter]::ToString($sha0.ComputeHash([Text.Encoding]::UTF8.GetBytes('sekiro-coop-log:' + $pw0))) -replace '-', '').Substring(0, 16)
      $name0 = Get-Setting $ini 'player_name'
      if (-not $name0) { $name0 = $env:USERNAME }
      $ms0 = New-Object IO.MemoryStream
      $gz0 = New-Object IO.Compression.GZipStream($ms0, [IO.Compression.CompressionLevel]::Optimal, $true)
      $fs0 = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
      try { $fs0.CopyTo($gz0) } finally { $fs0.Dispose() }
      $gz0.Dispose()
      $z0 = $ms0.ToArray()
      if ($z0.Length -le 0 -or $z0.Length -gt 38MB) { Write-Host ('Tiedosto {0}: liian suuri tai tyhja, ei lahetetty.' -f $label); return }
      Invoke-WebRequest -Uri ('http://' + $addr + '/file.gz') -Method Post -Body $z0 -ContentType 'application/octet-stream' `
        -Headers @{ 'X-Coop-Key' = $key0; 'X-Coop-Name' = $name0; 'X-Coop-File' = $label } -TimeoutSec 120 -UseBasicParsing | Out-Null
      Write-Host ('Tiedosto lahetetty hostille: {0} ({1} kt pakattuna).' -f $label, [int]($z0.Length / 1024))
    } catch {
      Write-Host ('Tiedosto {0}: lahetys ei onnistunut ({1}).' -f $label, $_.Exception.Message)
    }
  }
  $saveDir = Join-Path $env:APPDATA 'Sekiro'
  $coopSave = if (Test-Path $saveDir) { Get-ChildItem $saveDir -Recurse -Filter 'S0000.coop.sl2' -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1 } else { $null }
  if ($coopSave) { Send-File $coopSave.FullName 'S0000.coop.sl2' }
  Send-File (Join-Path $env:LOCALAPPDATA 'sekiro-coop\coop_state.json') 'coop_state.json'
  $bk = Join-Path $env:LOCALAPPDATA 'sekiro-coop\save-backups'
  $newestBk = if (Test-Path $bk) { Get-ChildItem $bk -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike '*.bak' -and $_.Name -notlike 'MANUAL*' } | Sort-Object LastWriteTime -Descending | Select-Object -First 1 } else { $null }
  if ($newestBk) { Send-File $newestBk.FullName ('backup-' + $newestBk.Name) }
  $log = Join-Path $env:LOCALAPPDATA 'sekiro-coop\log.txt'
  if (-not (Test-Path $log)) { Write-Host 'Loki: log.txt puuttuu, ei lahetettavaa.'; Swap-PlayCmd; exit 0 }
  $pw = Get-Setting $ini 'password'
  $sha = [Security.Cryptography.SHA256]::Create()
  $key = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('sekiro-coop-log:' + $pw))) -replace '-', '').Substring(0, 16)
  $name = Get-Setting $ini 'player_name'
  if (-not $name) { $name = $env:USERNAME }
  $hdr = @{ 'X-Coop-Key' = $key; 'X-Coop-Name' = $name }
  # 1) gzip of log.old.txt + log.txt (the whole last two sessions; text packs about 10:1). The
  #    host's server unpacks it. An older server answers 404 and we fall back to the plain tail.
  $sent = $false
  try {
    $ms = New-Object IO.MemoryStream
    $gz = New-Object IO.Compression.GZipStream($ms, [IO.Compression.CompressionLevel]::Optimal, $true)
    foreach ($f in @((Join-Path $env:LOCALAPPDATA 'sekiro-coop\log.old.txt'), $log)) {
      if (Test-Path $f) {
        $fs = [IO.File]::Open($f, 'Open', 'Read', 'ReadWrite')
        try { $fs.CopyTo($gz) } finally { $fs.Dispose() }
      }
    }
    $gz.Dispose()
    $z = $ms.ToArray()
    if ($z.Length -gt 0 -and $z.Length -le 15MB) {
      Invoke-WebRequest -Uri ('http://' + $addr + '/log.gz') -Method Post -Body $z -ContentType 'application/octet-stream' `
        -Headers $hdr -TimeoutSec 120 -UseBasicParsing | Out-Null
      Write-Host ('Loki lahetetty hostille pakattuna ({0} kt).' -f [int]($z.Length / 1024))
      $sent = $true
    }
  } catch {
    Write-Host ('Loki: pakattu lahetys ei onnistunut ({0}), yritetaan tavallista.' -f $_.Exception.Message)
  }
  # 2) Fallback: the last 8 MB of log.txt as plain bytes.
  if (-not $sent) {
    $bytes = [IO.File]::ReadAllBytes($log)
    $max = 8MB
    # Typed copy of the tail: a range slice ($bytes[a..b]) makes an object[] and Invoke-WebRequest then
    # sends it as text (about 3.6x bigger), so a log over 8 MB was refused with 413 Too Large.
    if ($bytes.Length -gt $max) {
      $tail = New-Object byte[] $max
      [Array]::Copy($bytes, $bytes.Length - $max, $tail, 0, $max)
      $bytes = $tail
    }
    if ($bytes.Length -eq 0) { Write-Host 'Loki: log.txt on tyhja, ei lahetettavaa.'; Swap-PlayCmd; exit 0 }
    Invoke-WebRequest -Uri ('http://' + $addr + '/log') -Method Post -Body $bytes -ContentType 'application/octet-stream' `
      -Headers $hdr -TimeoutSec 60 -UseBasicParsing | Out-Null
    Write-Host ('Loki lahetetty hostille ({0} kt).' -f [int]($bytes.Length / 1024))
  }
} catch {
  Write-Host ('Loki: lahetys ei onnistunut ({0}).' -f $_.Exception.Message)
}
Swap-PlayCmd

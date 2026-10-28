# Pelin sulkeuduttua sulkee taustalle jaaneet me3-prosessit (me3-launcher.exe / me3.exe), jotka play.cmd
# kaynnisti. Vain uudet prosessit: ne jotka olivat jo kaynnissa ennen peliä (-Snapshot) jatkavat ennallaan
# (esim. toinen me3-peli). Ei tee mitaan jos pelia ei kaynnistetty tasta ikkunasta.
param([switch]$Snapshot, [switch]$WaitForStart)
$store = Join-Path $env:TEMP 'sekiro-coop-me3-old.txt'
if ($Snapshot) {
  $ids = @(Get-Process -Name 'me3-launcher', 'me3' -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
  Set-Content -Path $store -Value ($ids -join ',') -Encoding ASCII
  exit 0
}
$old = @()
if (Test-Path $store) { $old = @((Get-Content $store -TotalCount 1) -split ',' | Where-Object { $_ }) }
# play.cmd odottaa pelin kaynnistymista vain jos lokin lahetys ei jo odottanut sen sulkeutumista.
if ($WaitForStart) {
  $t = Get-Date
  while (-not (Get-Process -Name sekiro -ErrorAction SilentlyContinue) -and ((Get-Date) - $t).TotalSeconds -lt 180) { Start-Sleep -Seconds 2 }
}
while (Get-Process -Name sekiro -ErrorAction SilentlyContinue) { Start-Sleep -Seconds 3 }
Start-Sleep -Seconds 3
Get-Process -Name 'me3-launcher', 'me3' -ErrorAction SilentlyContinue |
  Where-Object { $old -notcontains [string]$_.Id } |
  ForEach-Object { Write-Host ('Suljetaan me3: ' + $_.ProcessName + ' ' + $_.Id); Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
Remove-Item $store -ErrorAction SilentlyContinue

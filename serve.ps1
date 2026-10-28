# Host only (test builds). Serves dist\sekiro-coop\sekiro_coop.dll (or the copy staged in
# update\ while the game holds the DLL) on TCP 28101 so the friend's play.cmd (update.ps1)
# gets the newest published build before their game starts, and accepts the friend's log
# after their game closes (POST /log, upload-log.ps1). Reads the DLL fresh for every request.
# Without -Persistent: exits a minute after Sekiro has closed (or after 10 minutes if the game
# never started). With -Persistent (test-server.cmd): runs until stopped.
# Logs need the co-op password: X-Coop-Key = first 16 hex of SHA-256("sekiro-coop-log:" + password),
# password read from dist\sekiro-coop\sekiro_coop_settings.ini. Uploads go to
# %LOCALAPPDATA%\sekiro-coop\friend-uploads\ (max 16 MB each, newest 30 kept), each followed by
# report-<name>-<time>.txt from the coop-audit analyzer when it is built (IMPLEMENTATION-NOTES §32).
# Auto-update for friends (update.ps1): GET /manifest lists "<name> <sha256> <size>" for the shipped
# files (play.cmd update.ps1 upload-log.ps1 sekiro-coop.me3 OHJE.txt + the DLL) and GET /file/<name>
# serves exactly those names (strict whitelist). Like GET /sekiro_coop.dll they need no key (as before).
# Never served: sekiro_coop_settings.ini and test_server.txt (per player). -Port is for local tests.
param([switch]$Persistent, [int]$Port = 28101)
$dll = Join-Path $PSScriptRoot 'dist\sekiro-coop\sekiro_coop.dll'
$staged = Join-Path $PSScriptRoot 'dist\sekiro-coop\update\sekiro_coop.dll'
$ini = Join-Path $PSScriptRoot 'dist\sekiro-coop\sekiro_coop_settings.ini'
$uploads = Join-Path $env:LOCALAPPDATA 'sekiro-coop\friend-uploads'
$hostLog = Join-Path $env:LOCALAPPDATA 'sekiro-coop\log.txt'
$port = $Port
$shipDir = Join-Path $PSScriptRoot 'dist\sekiro-coop'
$shipNames = @('sekiro_coop.dll', 'play.cmd', 'update.ps1', 'upload-log.ps1', 'laheta-lokit.cmd', 'stop-me3.ps1', 'sekiro-coop.me3', 'OHJE.txt')
# Strict whitelist: only an exact (case-insensitive) match of the fixed list maps to a path; the
# requested string is never joined into a path. The DLL comes from update\ when staged.
function Get-ShipPath($name) {
  foreach ($n in $shipNames) {
    if ($n -ieq $name) {
      if ($n -eq 'sekiro_coop.dll' -and (Test-Path $staged)) { return $staged }
      $p = Join-Path $shipDir $n
      if (Test-Path $p) { return $p }
      return $null
    }
  }
  return $null
}
function Send-Bytes($s, $bytes, $type) {
  $h = [Text.Encoding]::ASCII.GetBytes("HTTP/1.0 200 OK`r`nContent-Type: $type`r`nContent-Length: $($bytes.Length)`r`n`r`n")
  $s.Write($h, 0, $h.Length); $s.Write($bytes, 0, $bytes.Length)
}
function Get-Manifest {
  $sha = [Security.Cryptography.SHA256]::Create()
  $out = New-Object System.Text.StringBuilder
  foreach ($n in $shipNames) {
    $p = Get-ShipPath $n
    if (-not $p) { continue }
    try {
      $bytes = [IO.File]::ReadAllBytes($p)
      $hex = ([BitConverter]::ToString($sha.ComputeHash($bytes)) -replace '-', '').ToLower()
      [void]$out.Append(('{0} {1} {2}' -f $n, $hex, $bytes.Length) + "`n")
    } catch {}
  }
  return , [Text.Encoding]::ASCII.GetBytes($out.ToString())
}
# After a friend log upload: the host-only analyzer (coop-audit, built by build-dist.cmd,
# copied to dist\tools by release.cmd) compares it with our log.txt and writes
# friend-uploads\report-<name>-<time>.txt. A missing or failing analyzer never fails the upload.
function Invoke-Audit($upload) {
  try {
    $exe = @(
      (Join-Path $PSScriptRoot 'dist\tools\coop-audit.exe'),
      (Join-Path $PSScriptRoot 'target-dist\release\coop-audit.exe'),
      (Join-Path $PSScriptRoot 'target\release\coop-audit.exe')
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $exe -or -not (Test-Path $hostLog)) { return }
    $name = [IO.Path]::GetFileNameWithoutExtension($upload) -replace '^log-', ''
    $report = Join-Path $uploads ('report-{0}.txt' -f $name)
    # 3-6 players (PLAN-MULTIPLAYER 5.19): the other players' newest uploads of the last 6 hours
    # go along; the analyzer keeps only those that overlap this log in time. None = the
    # 2-player call and report as before.
    $self = [IO.Path]::GetFileNameWithoutExtension($upload) -replace '-\d{8}-\d{6}$', ''
    $others = @(Get-ChildItem $uploads -Filter 'log-*.txt' |
        Where-Object { $_.FullName -ne $upload -and $_.LastWriteTime -gt (Get-Date).AddHours(-6) } |
        Sort-Object LastWriteTime -Descending |
        Group-Object { $_.BaseName -replace '-\d{8}-\d{6}$', '' } |
        Where-Object { $_.Name -ne $self } |
        ForEach-Object { $_.Group[0].FullName } |
        Select-Object -First 5)
    & $exe $hostLog $upload @others -o $report *> $null
    Get-ChildItem $uploads -Filter 'report-*.txt' | Sort-Object LastWriteTime -Descending | Select-Object -Skip 30 | Remove-Item -Force
  } catch {}
}
function Get-Key {
  $pw = ''
  if (Test-Path $ini) {
    $m = Select-String -Path $ini -Pattern '^\s*password\s*=\s*(.*?)\s*$' | Select-Object -First 1
    if ($m) { $pw = $m.Matches[0].Groups[1].Value }
  }
  $sha = [Security.Cryptography.SHA256]::Create()
  return ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('sekiro-coop-log:' + $pw))) -replace '-', '').Substring(0, 16)
}
function Send-Status($s, $code, $text) {
  $h = [Text.Encoding]::ASCII.GetBytes("HTTP/1.0 $code $text`r`nContent-Length: 0`r`n`r`n")
  $s.Write($h, 0, $h.Length)
}
try {
  $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, $port)
  $l.Start()
} catch {
  exit 0   # port taken (an older server or the mod's own one): nothing to do
}
$started = Get-Date
$seenGame = $false
$goneSince = $null
while ($true) {
  if ($l.Pending()) {
    $c = $l.AcceptTcpClient()
    $analyze = $null
    try {
      $c.ReceiveTimeout = 10000; $c.SendTimeout = 20000
      $s = $c.GetStream()
      # Header (up to the blank line), byte by byte so the body stays in the stream.
      $hdr = New-Object System.Collections.Generic.List[byte]
      while ($hdr.Count -lt 8192) {
        $b = $s.ReadByte()
        if ($b -lt 0) { break }
        $hdr.Add([byte]$b)
        $n = $hdr.Count
        if ($n -ge 4 -and $hdr[$n - 4] -eq 13 -and $hdr[$n - 3] -eq 10 -and $hdr[$n - 2] -eq 13 -and $hdr[$n - 1] -eq 10) { break }
      }
      $req = [Text.Encoding]::ASCII.GetString($hdr.ToArray())
      $line1 = ($req -split "`r`n")[0]
      if ($line1 -match '^GET /manifest HTTP/') {
        Send-Bytes $s (Get-Manifest) 'text/plain'
      } elseif ($line1 -match '^GET /savefix HTTP/') {
        # v8.56: one-time save fixes (dist\savefix.txt, update.ps1 Invoke-SaveFix), password key only.
        $key = ''
        foreach ($line in $req -split "`r`n") { if ($line -match '^X-Coop-Key:\s*([0-9A-Fa-f]+)') { $key = $Matches[1].ToUpper() } }
        $fixFile = Join-Path $PSScriptRoot 'dist\savefix.txt'
        if ($key -ne (Get-Key)) { Send-Status $s 403 'Forbidden' }
        elseif (Test-Path $fixFile) { Send-Bytes $s ([IO.File]::ReadAllBytes($fixFile)) 'text/plain' }
        else { Send-Status $s 404 'Not Found' }
      } elseif ($line1 -match '^GET /file/([^\s/\\?]+) HTTP/') {
        $fp = Get-ShipPath $Matches[1]
        if ($fp) { Send-Bytes $s ([IO.File]::ReadAllBytes($fp)) 'application/octet-stream' }
        else { Send-Status $s 404 'Not Found' }
      } elseif ($req.StartsWith('GET /sekiro_coop.dll') -and (Test-Path $dll)) {
        $b = [IO.File]::ReadAllBytes($(if (Test-Path $staged) { $staged } else { $dll }))
        $h = [Text.Encoding]::ASCII.GetBytes("HTTP/1.0 200 OK`r`nContent-Type: application/octet-stream`r`nContent-Length: $($b.Length)`r`n`r`n")
        $s.Write($h, 0, $h.Length); $s.Write($b, 0, $b.Length)
      } elseif ($req.StartsWith('POST /file.gz')) {
        # v8.15: a file of the friend's (co-op save, state, newest save backup) after the game
        # closed, gzipped, header X-Coop-File = its name. Saved unpacked as file-<name>-<time>-<file>
        # in friend-uploads (newest 40 kept). Same key check as the log.
        $len = 0; $key = ''; $name = 'friend'; $fname = 'file'
        foreach ($line in $req -split "`r`n") {
          if ($line -match '^Content-Length:\s*(\d+)') { $len = [int64]$Matches[1] }
          elseif ($line -match '^X-Coop-Key:\s*([0-9A-Fa-f]+)') { $key = $Matches[1].ToUpper() }
          elseif ($line -match '^X-Coop-Name:\s*(.+)$') { $name = $Matches[1].Trim() }
          elseif ($line -match '^X-Coop-File:\s*(.+)$') { $fname = $Matches[1].Trim() }
        }
        if ($key -ne (Get-Key)) { Send-Status $s 403 'Forbidden' }
        elseif ($len -le 0 -or $len -gt 40MB) { Send-Status $s 413 'Too Large' }
        else {
          $body = New-Object byte[] $len
          $got = 0
          while ($got -lt $len) {
            $r = $s.Read($body, $got, [int][Math]::Min(65536, $len - $got))
            if ($r -le 0) { break }
            $got += $r
          }
          $okFile = ($got -eq $len)
          if ($okFile) {
            try {
              $in = New-Object IO.MemoryStream (, $body)
              $gzs = New-Object IO.Compression.GZipStream($in, [IO.Compression.CompressionMode]::Decompress)
              $out = New-Object IO.MemoryStream
              $buf = New-Object byte[] 65536
              while (($n = $gzs.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                if ($out.Length -gt 128MB) { throw 'too big' }
              }
              $body = $out.ToArray()
            } catch { $okFile = $false }
          }
          if ($okFile) {
            New-Item -ItemType Directory -Force -Path $uploads | Out-Null
            $safe = ($name -replace '[^A-Za-z0-9_-]', '_'); if ($safe.Length -gt 32) { $safe = $safe.Substring(0, 32) }
            $fsafe = ($fname -replace '[^A-Za-z0-9_.-]', '_'); if ($fsafe.Length -gt 64) { $fsafe = $fsafe.Substring(0, 64) }
            $file = Join-Path $uploads ('file-{0}-{1}-{2}' -f $safe, (Get-Date -Format 'yyyyMMdd-HHmmss'), $fsafe)
            [IO.File]::WriteAllBytes($file, $body)
            Get-ChildItem $uploads -Filter 'file-*' | Sort-Object LastWriteTime -Descending | Select-Object -Skip 40 | Remove-Item -Force
            Send-Status $s 200 'OK'
          } else { Send-Status $s 400 'Bad Request' }
        }
      } elseif ($req.StartsWith('POST /log')) {
        $len = 0; $key = ''; $name = 'friend'
        foreach ($line in $req -split "`r`n") {
          if ($line -match '^Content-Length:\s*(\d+)') { $len = [int64]$Matches[1] }
          elseif ($line -match '^X-Coop-Key:\s*([0-9A-Fa-f]+)') { $key = $Matches[1].ToUpper() }
          elseif ($line -match '^X-Coop-Name:\s*(.+)$') { $name = $Matches[1] }
        }
        if ($key -ne (Get-Key)) { Send-Status $s 403 'Forbidden' }
        elseif ($len -le 0 -or $len -gt 16MB) { Send-Status $s 413 'Too Large' }
        else {
          $body = New-Object byte[] $len
          $got = 0
          while ($got -lt $len) {
            $r = $s.Read($body, $got, [int][Math]::Min(65536, $len - $got))
            if ($r -le 0) { break }
            $got += $r
          }
          # POST /log.gz (v8.15): gzip of log.old.txt + log.txt, unpacked here (max 256 MB) so the
          # analyzer and every old tool still see a plain text log. Plain POST /log keeps working.
          $okBody = ($got -eq $len)
          if ($okBody -and $req.StartsWith('POST /log.gz')) {
            try {
              $in = New-Object IO.MemoryStream (, $body)
              $gzs = New-Object IO.Compression.GZipStream($in, [IO.Compression.CompressionMode]::Decompress)
              $out = New-Object IO.MemoryStream
              $buf = New-Object byte[] 65536
              while (($n = $gzs.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                if ($out.Length -gt 256MB) { throw 'too big' }
              }
              $body = $out.ToArray()
            } catch { $okBody = $false }
          }
          if ($okBody) {
            New-Item -ItemType Directory -Force -Path $uploads | Out-Null
            $safe = ($name -replace '[^A-Za-z0-9_-]', '_')
            if ($safe.Length -gt 32) { $safe = $safe.Substring(0, 32) }
            $file = Join-Path $uploads ('log-{0}-{1}.txt' -f $safe, (Get-Date -Format 'yyyyMMdd-HHmmss'))
            [IO.File]::WriteAllBytes($file, $body)
            Get-ChildItem $uploads -Filter 'log-*.txt' | Sort-Object LastWriteTime -Descending | Select-Object -Skip 30 | Remove-Item -Force
            Send-Status $s 200 'OK'
            $analyze = $file
          } else { Send-Status $s 400 'Bad Request' }
        }
      } else {
        Send-Status $s 404 'Not Found'
      }
      $s.Flush()
    } catch {} finally { $c.Close() }
    # The friend already has its 200 OK (connection closed): analyse now.
    if ($analyze) { Invoke-Audit $analyze }
    continue
  }
  Start-Sleep -Milliseconds 200
  if ($Persistent) { continue }
  $running = [bool](Get-Process -Name sekiro -ErrorAction SilentlyContinue)
  if ($running) { $seenGame = $true; $goneSince = $null }
  elseif ($seenGame) {
    if (-not $goneSince) { $goneSince = Get-Date }
    elseif (((Get-Date) - $goneSince).TotalSeconds -gt 60) { break }
  } elseif (((Get-Date) - $started).TotalMinutes -gt 10) { break }
}
$l.Stop()

@echo off
rem Starts Sekiro with the co-op mod (me3). Steam must be running. Same file for everybody.
set "ME3=%LOCALAPPDATA%\Programs\garyttierney\me3\bin\me3.exe"
if not exist "%ME3%" (
  echo me3 puuttuu. Asenna se: https://github.com/garyttierney/me3/releases
  pause
  exit /b 1
)
rem Ilman Steamia me3 lopettaa heti ja ikkuna sulkeutuisi ennen kuin syyn ehtii lukea.
tasklist /FI "IMAGENAME eq steam.exe" 2>nul | find /I "steam.exe" >nul
if errorlevel 1 (
  echo Steam ei ole kaynnissa. Kaynnista Steam ja aja play.cmd uudelleen.
  pause
  exit /b 1
)
rem Vanhojen versioiden pelin sisainen lataus: ei enaa kayteta.
del "%LOCALAPPDATA%\sekiro-coop\update\sekiro_coop.dll" 2>nul
rem Uusi versio, joka julkaistiin pelin ollessa auki (DLL oli lukittu): otetaan kayttoon nyt.
if exist "%~dp0update\sekiro_coop.dll" (
  move /y "%~dp0update\sekiro_coop.dll" "%~dp0sekiro_coop.dll" >nul && echo Uusi versio otettu kayttoon.
)
rem Testiversio: haetaan hostin uusin versio (test_server.txt tai hostin UDP-osoite).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0update.ps1"
rem Muistetaan jo kaynnissa olevat me3-prosessit: pelin sulkeuduttua suljetaan vain uudet (stop-me3.ps1).
if exist "%~dp0stop-me3.ps1" powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0stop-me3.ps1" -Snapshot
"%ME3%" launch --game sekiro --profile "%~dp0sekiro-coop.me3"
rem me3 failed to start the game: keep the window open so the error can be read.
if errorlevel 1 (
  pause
  exit /b 1
)
rem Testiversio: pelin sulkemisen jalkeen loki lahtee hostille (vain jos test_server.txt on olemassa).
if exist "%~dp0test_server.txt" (
  echo Odotetaan pelin sulkemista, jotta loki voidaan lahettaa hostille. Ala sulje tata ikkunaa.
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0upload-log.ps1"
  timeout /t 3 >nul
  rem Peli on jo sulkeutunut: suljetaan taustalle jaaneet me3-prosessit.
  if exist "%~dp0stop-me3.ps1" powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0stop-me3.ps1"
) else (
  rem Ei lokin lahetysta: odotetaan itse pelin kaynnistymista ja sulkeutumista, sitten suljetaan me3.
  if exist "%~dp0stop-me3.ps1" powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0stop-me3.ps1" -WaitForStart
)
rem update.ps1 voi kirjoittaa uuden play.cmd:n nimella play.cmd.new (ajettavaa .cmd:ta ei saa korvata
rem kesken ajon). Vaihto tehdaan vasta nyt: irrotettu cmd odottaa ~2 s (tama ikkuna ehtii sulkeutua
rem exitilla) ja siirtaa tiedoston paikalleen. Jos vaihto ei onnistu, .new jaa odottamaan seuraavaa kertaa.
if exist "%~dp0play.cmd.new" (
  start "" /min cmd /c "ping -n 3 127.0.0.1 >nul & move /y "%~dp0play.cmd.new" "%~dp0play.cmd" >nul"
)
rem Ikkuna sulkeutuu itse, kun kaikki on tehty.
exit

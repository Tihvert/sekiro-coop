@echo off
rem Test release: ONE build and ONE folder for everybody (dist\sekiro-coop, the host plays
rem from it too) + dist\sekiro-coop.zip for friends. If the game is running from that
rem folder (DLL locked), the new DLL goes to update\ and play.cmd applies it on the next start.
rem The zip never contains personal files (settings with the password, join code, logs).
call "%~dp0build-dist.cmd" || exit /b 1
set "D=%~dp0dist\sekiro-coop"
set "NEW=%~dp0target-dist\release\sekiro_coop.dll"
if not exist "%D%" mkdir "%D%"
copy /y "%NEW%" "%D%\sekiro_coop.dll" >nul 2>nul && (
  if exist "%D%\update\sekiro_coop.dll" del "%D%\update\sekiro_coop.dll"
) || (
  if not exist "%D%\update" mkdir "%D%\update"
  copy /y "%NEW%" "%D%\update\sekiro_coop.dll" >nul || (echo kopiointi epaonnistui & exit /b 1)
  echo Peli on auki: uusi versio otetaan kayttoon seuraavalla play.cmd-kaynnistyksella.
)
rem The launcher files come from ship\ (in git; the DLL embeds the same copies, shipfiles.rs).
rem play.cmd may be running from dist (the host plays from it): a changed one goes to play.cmd.new,
rem which play.cmd swaps in after the game.
for %%F in (update.ps1 upload-log.ps1 laheta-lokit.cmd stop-me3.ps1 sekiro-coop.me3 OHJE.txt) do copy /y "%~dp0ship\%%F" "%D%\%%F" >nul
fc /b "%~dp0ship\play.cmd" "%D%\play.cmd" >nul 2>nul || copy /y "%~dp0ship\play.cmd" "%D%\play.cmd.new" >nul
rem Host only: the log analyzer serve.ps1 runs after every friend log upload (never in the zip).
if not exist "%~dp0dist\tools" mkdir "%~dp0dist\tools"
copy /y "%~dp0target-dist\release\coop-audit.exe" "%~dp0dist\tools\coop-audit.exe" >nul 2>nul || echo coop-audit.exe ei kopioitunut, serve.ps1 kayttaa target-dist\release\coop-audit.exe
set "S=%TEMP%\sekiro-coop-zip"
if exist "%S%" rmdir /s /q "%S%"
mkdir "%S%"
copy /y "%NEW%" "%S%\sekiro_coop.dll" >nul
for %%F in (play.cmd update.ps1 upload-log.ps1 laheta-lokit.cmd stop-me3.ps1 sekiro-coop.me3 OHJE.txt) do copy /y "%~dp0ship\%%F" "%S%\%%F" >nul
rem Test builds: the host's update + log server address (test-server.cmd), friends only.
if exist "%~dp0dist\test_server.txt" copy /y "%~dp0dist\test_server.txt" "%S%\test_server.txt" >nul
del "%~dp0dist\sekiro-coop.zip" 2>nul
powershell -NoProfile -Command "Compress-Archive -Path '%S%\*' -DestinationPath '%~dp0dist\sekiro-coop.zip' -Force"
rmdir /s /q "%S%"
echo valmis: dist\sekiro-coop ja dist\sekiro-coop.zip

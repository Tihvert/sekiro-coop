@echo off
rem Starts Sekiro with the co-op mod through me3. Steam has to be running.
set "ME3=%LOCALAPPDATA%\Programs\garyttierney\me3\bin\me3.exe"
if not exist "%ME3%" (
  echo me3 is not installed. Get it here: https://github.com/garyttierney/me3/releases
  pause
  exit /b 1
)
tasklist /FI "IMAGENAME eq steam.exe" 2>nul | find /I "steam.exe" >nul
if errorlevel 1 (
  echo Steam is not running. Start Steam and run play.cmd again.
  pause
  exit /b 1
)
"%ME3%" launch --game sekiro --profile "%~dp0sekiro-coop.me3"
if errorlevel 1 (
  pause
  exit /b 1
)

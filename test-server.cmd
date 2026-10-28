@echo off
rem HOST, test builds: keeps the update + log server (TCP 28101) running until this window is
rem closed. The friend's play.cmd fetches the newest DLL from it and sends its log after the
rem game closes. Logs land in %LOCALAPPDATA%\sekiro-coop\friend-uploads\.
echo Testipalvelin kaynnissa portissa 28101. Sulje tama ikkuna lopettaaksesi.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0serve.ps1" -Persistent

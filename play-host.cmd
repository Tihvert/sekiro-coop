@echo off
rem HOST (test builds): opens the update server (TCP 28101) so the friend's play.cmd can
rem update before the host is in the game, then starts the shared dist\sekiro-coop\play.cmd.
start "" /min powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "%~dp0serve.ps1"
call "%~dp0dist\sekiro-coop\play.cmd"

@echo off
rem Tekee pienen zipin (lokit + tallennus + tila) tyopoydalle ja lahettaa sen hostille, jos test_server.txt on.
rem Voit ajaa tamän milloin vain, myos pelin ollessa auki. Zip on yleensa 2-10 Mt (loki pakkautuu hyvin).
rem Jos automaattinen lahetys ei onnistu, laheta tyopoydalle ilmestynyt sekiro-coop-lokit-...zip kasin hostille.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0upload-log.ps1" -Now -Zip
echo.
pause

@echo off
rem Type-check without writing the DLL (works while the game holds it open).
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo check --release -p sekiro_coop

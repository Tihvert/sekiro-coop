@echo off
rem Build into a separate target dir (works while the game holds target\release\sekiro_coop.dll).
rem Also builds the host-only log analyzer target-dist\release\coop-audit.exe (never in the friend zip).
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "CARGO_TARGET_DIR=%~dp0target-dist"
cargo build --release -p sekiro_coop -p coop-audit

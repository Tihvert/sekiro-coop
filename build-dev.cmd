@echo off
rem Dev build WITH the experimental native remote player (target\release, used by profiles\sekiro-coop.me3).
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo build --release -p sekiro_coop --features dev-native

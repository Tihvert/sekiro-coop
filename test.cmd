@echo off
rem Run the host-side unit tests (no game needed).
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo test -p coop-net -p coop-audit

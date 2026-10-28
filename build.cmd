@echo off
rem Build the mod DLL inside a Visual Studio x64 developer environment.
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo build --release -p sekiro_coop

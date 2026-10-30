@echo off
rem Public release build (Nexus, GitHub Releases): dist\public\sekiro-seamless-coop-v<version>.zip.
rem Differs from the test builds (build-dist.cmd / release.cmd):
rem - no `testsync` feature: no log streaming, no DLL push between players, and none of the
rem   testers' PowerShell launcher scripts embedded in the DLL (shipfiles.rs);
rem - local paths remapped (no C:\Users\<name> in panic messages or the PDB path);
rem - no scripts in the zip: me3 opens sekiro-coop.me3 on a double-click (me3 launch -p).
rem The text files come from ship-public\ and the licenses from the public repo's files here.
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "ROOT=%~dp0"
set "ROOT=%ROOT:~0,-1%"
set "CARGO_TARGET_DIR=%ROOT%\target-public"
rem A TOML array, not RUSTFLAGS: the project path has a space. The last matching remap wins.
cargo build --release -p sekiro_coop --no-default-features --features dev-native --config "build.rustflags=['--remap-path-prefix=%USERPROFILE%=~', '--remap-path-prefix=%ROOT%=.', '-Clink-arg=/PDBALTPATH:sekiro_coop.pdb']" || exit /b 1
rem `pub const MOD_VERSION: &str = "1.0.2-beta";` -> 1.0.2-beta
for /f "tokens=2 delims==;" %%V in ('findstr /b /c:"pub const MOD_VERSION" "%ROOT%\crates\coop-dll\src\ui.rs"') do for %%A in (%%V) do set "VER=%%~A"
if "%VER%"=="" (echo MOD_VERSION not found & exit /b 1)
set "S=%ROOT%\dist\public\sekiro-seamless-coop"
if exist "%S%" rmdir /s /q "%S%"
mkdir "%S%\licenses"
copy /y "%CARGO_TARGET_DIR%\release\sekiro_coop.dll" "%S%\" >nul || exit /b 1
copy /y "%ROOT%\ship\sekiro-coop.me3" "%S%\" >nul || exit /b 1
copy /y "%ROOT%\ship-public\HOW-TO-PLAY.txt" "%S%\" >nul || exit /b 1
copy /y "%ROOT%\ship-public\LICENSE.txt" "%S%\" >nul || exit /b 1
copy /y "%ROOT%\ship-public\licenses\*" "%S%\licenses\" >nul || exit /b 1
set "Z=sekiro-seamless-coop-v%VER%.zip"
rem Windows tar (not Git's GNU tar on PATH) writes forward-slash paths; Compress-Archive's
rem backslashes break some unzippers.
pushd "%ROOT%\dist\public" || exit /b 1
if exist "%Z%" del "%Z%"
"%SystemRoot%\System32\tar.exe" -a -c -f "%Z%" sekiro-seamless-coop || (popd & exit /b 1)
popd
echo valmis: dist\public\%Z%

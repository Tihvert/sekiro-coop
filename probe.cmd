@echo off
rem UPnP diagnostic: lists routers answering SSDP. Opens no ports.
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul || exit /b 1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo run -q -p coop-net --example upnp_probe

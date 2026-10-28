@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
cd /d D:\QLight-Wallet-Windows\qlight-wallet\desktop-wallet
cargo %*

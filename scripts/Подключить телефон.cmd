@echo off
rem Connect to the phone over Wi-Fi and show its screen
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scrcpy-wifi.ps1" %*
if errorlevel 1 pause

@echo off
rem Pair this PC with the phone again (code from Wireless debugging)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scrcpy-wifi.ps1" -Pair
if errorlevel 1 pause

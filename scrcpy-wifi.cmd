@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scrcpy-wifi.ps1" %*
if errorlevel 1 pause

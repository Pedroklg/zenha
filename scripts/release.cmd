@echo off
rem Publica uma release do Zenha a partir deste PC: scripts\release.cmd 0.1.4
cd /d "%~dp0.."
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0release.ps1" %*
exit /b %errorlevel%

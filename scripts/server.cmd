@echo off
rem VM do servidor na Azure: server.cmd start^|stop^|status^|backup
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0server.ps1" %*
exit /b %errorlevel%

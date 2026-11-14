@echo off
rem Liga, desliga ou mostra o estado da VM do servidor na Azure: server.cmd start^|stop^|status
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0server.ps1" %*
exit /b %errorlevel%

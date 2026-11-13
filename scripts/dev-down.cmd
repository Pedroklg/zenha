@echo off
rem Encerra o livekit-server, o servidor de tokens e o cliente.
taskkill /im livekit-server.exe /f >nul 2>&1
taskkill /im rooms-token-server.exe /f >nul 2>&1
taskkill /im zenha.exe /f >nul 2>&1

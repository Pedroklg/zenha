@echo off
rem Sobe o livekit-server em modo dev e o servidor de tokens. Logs em dev-logs\.
cd /d "%~dp0.."
if not exist dev-logs mkdir dev-logs
if not exist .env copy .env.example .env >nul
set RUST_LOG=info
start "voice-rooms livekit" /min cmd /c "tools\livekit\livekit-server.exe --dev > dev-logs\livekit.log 2>&1"
start "voice-rooms token-server" /min cmd /c "cargo run -p rooms-token-server > dev-logs\server.log 2>&1"

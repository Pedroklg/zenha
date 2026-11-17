@echo off
rem Abre o cliente. Cada janela grava o proprio log em dev-logs\client-N.log.
rem Argumentos sao repassados, por exemplo: dev-client.cmd --demo
cd /d "%~dp0.."
if not exist dev-logs mkdir dev-logs
set RUST_LOG=info
set LOG=dev-logs\client-%RANDOM%.log
start "zenha client" /min cmd /c "cargo run -p zenha-client -- %* > %LOG% 2>&1"

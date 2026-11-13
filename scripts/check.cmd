@echo off
rem Roda fmt, clippy e testes; saida em dev-logs\ci.log.
cd /d "%~dp0.."
if not exist dev-logs mkdir dev-logs
cargo ci > dev-logs\ci.log 2>&1
set CI_RC=%errorlevel%
>> dev-logs\ci.log echo exit=%CI_RC%
exit /b %CI_RC%

@echo off
rem Roda o harness de testes ponta a ponta. Saida em dev-logs\e2e\console.log, relatorio em dev-logs\e2e\report.md.
rem Argumentos sao repassados, por exemplo: e2e.cmd --filter playout
cd /d "%~dp0.."
if not exist dev-logs\e2e mkdir dev-logs\e2e
set E2E_LOG=dev-logs\e2e\console.log
rem dev-logs\e2e-output.txt (opcional): parte do nome do dispositivo de saida usado nos testes, no lugar do padrao.
if not defined ZENHA_E2E_OUTPUT if exist dev-logs\e2e-output.txt set /p ZENHA_E2E_OUTPUT=<dev-logs\e2e-output.txt
echo Testes ponta a ponta em andamento (alguns minutos). Um tom de teste baixo sera tocado no dispositivo de saida de teste.
echo Resultado em dev-logs\e2e\report.md
> "%E2E_LOG%" echo e2e iniciado em %date% %time%
cargo build -p zenha-server -p zenha-e2e >> "%E2E_LOG%" 2>&1 && target\debug\zenha-e2e.exe run %* >> "%E2E_LOG%" 2>&1
set E2E_RC=%errorlevel%
>> "%E2E_LOG%" echo exit=%E2E_RC%
echo Concluido com exit=%E2E_RC%

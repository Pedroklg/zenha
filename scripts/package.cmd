@echo off
rem Compila a versao de release do Zenha, cria o .zip portatil e, se o Inno Setup estiver instalado, o instalador em dist\.
cd /d "%~dp0.."
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0package.ps1" %*
exit /b %errorlevel%

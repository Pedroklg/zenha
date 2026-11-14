# Liga, desliga ou mostra o estado da VM do servidor na Azure.
# Uso: server.cmd start|stop|status [-ResourceGroup zenha-rg] [-VmName zenha-vm]
param(
    [Parameter(Mandatory = $true)][ValidateSet('start', 'stop', 'status')][string]$Action,
    [string]$ResourceGroup = $(if ($env:ZENHA_AZURE_RG) { $env:ZENHA_AZURE_RG } else { 'zenha-rg' }),
    [string]$VmName = $(if ($env:ZENHA_AZURE_VM) { $env:ZENHA_AZURE_VM } else { 'zenha-vm' })
)
$ErrorActionPreference = 'Stop'

$az = (Get-Command az -ErrorAction SilentlyContinue).Source
if (-not $az) { $az = 'C:\Program Files\Microsoft SDKs\Azure\CLI2\wbin\az.cmd' }
if (-not (Test-Path $az)) { throw 'Azure CLI not found; install it and run az login' }

function Invoke-Az {
    & $az @args
    if ($LASTEXITCODE -ne 0) { throw "az $($args -join ' ') failed ($LASTEXITCODE)" }
}

function Show-Status {
    Invoke-Az vm get-instance-view -g $ResourceGroup -n $VmName `
        --query "{vm:name, power:instanceView.statuses[?starts_with(code,'PowerState/')].displayStatus | [0]}" -o table
}

switch ($Action) {
    'start' {
        Invoke-Az vm start -g $ResourceGroup -n $VmName -o none
        Show-Status
        $health = Invoke-Az vm run-command invoke -g $ResourceGroup -n $VmName --command-id RunShellScript `
            --scripts 'for i in $(seq 1 30); do curl -fsS http://127.0.0.1:8787/health >/dev/null 2>&1 && { echo healthy; exit 0; }; sleep 2; done; echo unhealthy' `
            --query 'value[0].message' -o tsv
        if ($health -match 'healthy' -and $health -notmatch 'unhealthy') { 'token server: ok' } else { 'token server: not responding yet (setup-server.sh already run?)' }
    }
    'stop' {
        # deallocate frees the compute; the static IP and disk are kept, so DNS keeps working.
        Invoke-Az vm deallocate -g $ResourceGroup -n $VmName -o none
        Show-Status
    }
    'status' { Show-Status }
}

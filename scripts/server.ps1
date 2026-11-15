# Liga, desliga ou mostra o estado da VM do servidor na Azure, controla a agenda ou baixa um backup do chat salvo.
# Uso: server.cmd start|stop|status|backup|schedule [on|off|status] [-ResourceGroup zenha-rg] [-VmName zenha-vm] [-OutDir backups]
param(
    [Parameter(Mandatory = $true, Position = 0)][ValidateSet('start', 'stop', 'status', 'backup', 'schedule')][string]$Action,
    [Parameter(Position = 1)][ValidateSet('on', 'off', 'status')][string]$ScheduleAction,
    [string]$ResourceGroup = $(if ($env:ZENHA_AZURE_RG) { $env:ZENHA_AZURE_RG } else { 'zenha-rg' }),
    [string]$VmName = $(if ($env:ZENHA_AZURE_VM) { $env:ZENHA_AZURE_VM } else { 'zenha-vm' }),
    [string]$OutDir
)
$ErrorActionPreference = 'Stop'
if (-not $OutDir) { $OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'backups' }

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

function Get-PowerState {
    Invoke-Az vm get-instance-view -g $ResourceGroup -n $VmName `
        --query "instanceView.statuses[?starts_with(code,'PowerState/')].code | [0]" -o tsv
}

function Get-ScheduleIds {
    $ids = @(Invoke-Az resource list -g $ResourceGroup --resource-type Microsoft.Logic/workflows `
        --query "[?starts_with(name, 'zenha-schedule-')].id" -o tsv) | Where-Object { $_ }
    if (-not $ids) { throw "no zenha-schedule-* workflows found in $ResourceGroup" }
    $ids
}

function Show-Schedule {
    $triggerIds = @(Get-ScheduleIds | ForEach-Object { "$_/triggers/Recurrence" })
    $triggers = Invoke-Az resource show --api-version 2019-05-01 -o json --ids @triggerIds | ConvertFrom-Json
    $triggers | ForEach-Object {
        [pscustomobject]@{
            workflow = ($_.id -split '/')[-3]
            state    = $_.properties.state
            nextRun  = if ($_.properties.nextExecutionTime) { ([datetime]$_.properties.nextExecutionTime).ToString('ddd dd/MM HH:mm') } else { '-' }
        }
    } | Format-Table -AutoSize
}

function Set-Schedule([string]$state) {
    foreach ($id in Get-ScheduleIds) {
        Invoke-Az resource update --api-version 2019-05-01 --set "properties.state=$state" -o none --ids $id
    }
    Show-Schedule
}

function Invoke-Remote([string]$script) {
    $file = Join-Path ([IO.Path]::GetTempPath()) "zenha-remote-$PID.sh"
    [IO.File]::WriteAllText($file, ("#!/bin/bash`nset -euo pipefail`n$script`n" -replace "`r", ''))
    try {
        (Invoke-Az vm run-command invoke -g $ResourceGroup -n $VmName --command-id RunShellScript `
            --scripts "@$file" --query 'value[0].message' -o tsv) -join "`n"
    } finally {
        Remove-Item $file -Force -ErrorAction SilentlyContinue
    }
}

function Backup-Chat {
    $remoteDir = '/home/azureuser/zenha-backup'
    $key = Join-Path $HOME '.ssh\zenha_azure'
    if (-not (Test-Path $key)) { throw "SSH key not found: $key" }

    $startedHere = $false
    if ((Get-PowerState) -ne 'PowerState/running') {
        'VM is off; starting it for the backup'
        Invoke-Az vm start -g $ResourceGroup -n $VmName -o none
        $startedHere = $true
    }
    $nsg = Invoke-Az network nsg list -g $ResourceGroup --query '[0].name' -o tsv
    $vmIp = Invoke-Az vm show -d -g $ResourceGroup -n $VmName --query publicIps -o tsv
    $myIp = (Invoke-RestMethod -Uri 'https://api.ipify.org' -TimeoutSec 10).Trim()
    if ($myIp -notmatch '^\d{1,3}(\.\d{1,3}){3}$') { throw "could not find this PC's public IPv4 (got '$myIp')" }

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $target = Join-Path $OutDir "chat-$((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')).db"
    try {
        $made = Invoke-Remote @"
c=`$(docker compose -f /opt/zenha/docker-compose.yml ps -q token-server)
[ -n "`$c" ] || { echo NO_CONTAINER; exit 0; }
install -d -m 700 -o azureuser -g azureuser $remoteDir
docker run --rm --volumes-from "`$c" -v ${remoteDir}:/backup alpine:3.22 sh -c '[ -f /var/lib/zenha/chat.db ] || { echo NO_DB; exit 0; }; apk add --no-cache -q sqlite >/dev/null && echo ".backup /backup/chat.db" | sqlite3 /var/lib/zenha/chat.db && echo BACKUP_OK'
[ -f $remoteDir/chat.db ] && chown azureuser:azureuser $remoteDir/chat.db && chmod 600 $remoteDir/chat.db
"@
        if ($made -notmatch 'BACKUP_OK') { throw "backup was not created on the VM: $made" }

        # The SSH port is opened only to this PC and only while copying.
        Invoke-Az network nsg rule create -g $ResourceGroup --nsg-name $nsg -n zenha-backup-ssh --priority 140 `
            --direction Inbound --access Allow --protocol Tcp --destination-port-ranges 22 `
            --source-address-prefixes "$myIp/32" -o none
        $copied = $false
        foreach ($attempt in 1..6) {
            & scp -q -i $key -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 `
                "azureuser@${vmIp}:zenha-backup/chat.db" $target
            if ($LASTEXITCODE -eq 0) { $copied = $true; break }
            Start-Sleep -Seconds 5
        }
        if (-not $copied) { throw 'scp failed' }
        "backup saved to $target ($((Get-Item $target).Length) bytes)"
    } finally {
        & $az network nsg rule delete -g $ResourceGroup --nsg-name $nsg -n zenha-backup-ssh -o none 2>$null
        Invoke-Remote "rm -rf $remoteDir" | Out-Null
        if ($startedHere) {
            Invoke-Az vm deallocate -g $ResourceGroup -n $VmName -o none
            'VM turned off again'
        }
    }
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
    'backup' { Backup-Chat }
    'schedule' {
        switch ($ScheduleAction) {
            'status' { Show-Schedule }
            'on' { Set-Schedule 'Enabled' }
            'off' { Set-Schedule 'Disabled' }
            default { throw 'usage: server.cmd schedule on|off|status' }
        }
    }
}

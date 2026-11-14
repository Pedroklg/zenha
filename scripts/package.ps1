$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$metaRaw = cargo metadata --no-deps --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata falhou com codigo $LASTEXITCODE"
}
$meta = ($metaRaw -join "`n") | ConvertFrom-Json
$pkg = $meta.packages | Where-Object { $_.name -eq 'rooms-client' } | Select-Object -First 1
if (-not $pkg -or [string]::IsNullOrWhiteSpace($pkg.version)) {
    throw 'Nao foi possivel obter a versao de rooms-client via cargo metadata'
}
$version = $pkg.version

Write-Host "==> Compilando release do Zenha ($version)"
cargo build --release -p rooms-client
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$exePath = Join-Path $root 'target\release\zenha.exe'
if (-not (Test-Path $exePath)) {
    throw "Executavel de release nao encontrado: $exePath"
}

$distDir = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $distDir | Out-Null

$zipName = "Zenha-$version-windows-x64.zip"
$zipPath = Join-Path $distDir $zipName
$stagingDir = Join-Path ([System.IO.Path]::GetTempPath()) ('zenha-pkg-' + [System.Guid]::NewGuid().ToString('N'))

try {
    New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null
    Copy-Item -Path $exePath -Destination (Join-Path $stagingDir 'zenha.exe')

    $readmeLines = @'
[
  "Zenha __VERSION__ (vers\u00e3o port\u00e1til para Windows x64)",
  "",
  "Como usar:",
  "1. Extraia os arquivos desta pasta e execute zenha.exe.",
  "2. Na primeira execu\u00e7\u00e3o, o Windows pode exibir \"O Windows protegeu o computador\"",
  "   (SmartScreen), pois o execut\u00e1vel ainda n\u00e3o possui assinatura digital.",
  "   Clique em \"Mais informa\u00e7\u00f5es\" e depois em \"Executar assim mesmo\".",
  "",
  "Configura\u00e7\u00f5es:",
  "Suas prefer\u00eancias ficam salvas em %APPDATA%\\zenha e s\u00e3o mantidas entre atualiza\u00e7\u00f5es."
]
'@ | ConvertFrom-Json

    $readmeContent = ($readmeLines -join "`r`n").Replace('__VERSION__', $version) + "`r`n"
    $readmePath = Join-Path $stagingDir 'LEIA-ME.txt'
    [System.IO.File]::WriteAllText($readmePath, $readmeContent, [System.Text.UTF8Encoding]::new($true))

    if (Test-Path $zipPath) {
        Remove-Item -Force $zipPath
    }
    Compress-Archive -Path (Join-Path $stagingDir '*') -DestinationPath $zipPath -CompressionLevel Optimal
    Write-Host "==> Zip gerado: $zipPath"
}
finally {
    if (Test-Path $stagingDir) {
        Remove-Item -Recurse -Force $stagingDir -ErrorAction SilentlyContinue
    }
}

$isccPath = $null
$isccCmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($isccCmd) {
    $isccPath = $isccCmd.Source
} else {
    $candidates = @()
    if (${env:ProgramFiles(x86)}) {
        $candidates += Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    }
    if ($env:ProgramFiles) {
        $candidates += Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'
    }
    if ($env:LOCALAPPDATA) {
        $candidates += Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'
    }
    $isccPath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

if (-not $isccPath) {
    Write-Warning 'ISCC.exe (Inno Setup 6) nao encontrado; gerando apenas o .zip portatil.'
    exit 0
}

$issPath = Join-Path $root 'installer\zenha.iss'
Write-Host "==> Compilando instalador com $isccPath"
& $isccPath "/DAppVersion=$version" $issPath
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$setupPath = Join-Path $distDir "Zenha-$version-windows-x64-setup.exe"
if (-not (Test-Path $setupPath)) {
    throw "Instalador esperado nao foi gerado: $setupPath"
}
Write-Host "==> Instalador gerado: $setupPath"

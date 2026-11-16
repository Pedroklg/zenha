param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Invoke-Step([string]$Name, [scriptblock]$Block) {
    Write-Host "==> $Name"
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "$Name falhou com codigo $LASTEXITCODE" }
}

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "Versao '$Version' deve ser no formato 1.2.3" }
$tag = "v$Version"

if ((git rev-parse --abbrev-ref HEAD) -ne 'main') { throw 'Rode a partir da branch main' }
if (git status --porcelain --untracked-files=no) { throw 'Ha mudancas nao commitadas' }

$meta = (cargo metadata --no-deps --format-version 1) -join "`n" | ConvertFrom-Json
$current = ($meta.packages | Where-Object { $_.name -eq 'rooms-client' }).version
if ($current -ne $Version) {
    $branch = "release/$tag"
    Invoke-Step "branch $branch" { git switch -q -c $branch }
    $toml = [IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'))
    $toml = [regex]::Replace($toml, '(?m)(\[workspace\.package\]\s*\r?\nversion = ")[^"]+(")', "`${1}$Version`${2}")
    [IO.File]::WriteAllText((Join-Path $root 'Cargo.toml'), $toml)
    $issPath = Join-Path $root 'installer\zenha.iss'
    $iss = [IO.File]::ReadAllText($issPath)
    $iss = [regex]::Replace($iss, '(#define AppVersion ")[^"]+(")', "`${1}$Version`${2}")
    [IO.File]::WriteAllText($issPath, $iss)
    Invoke-Step 'cargo update -w' { cargo update -w -q }
    Invoke-Step 'add bump' { git add Cargo.toml Cargo.lock installer/zenha.iss }
    Invoke-Step 'commit bump' { git commit -q -m "chore(release): bump version to $Version" }
    Invoke-Step 'switch main' { git switch -q main }
    Invoke-Step 'merge bump' { git merge --no-ff -q $branch -m "Merge branch '$branch'" }
    Invoke-Step 'delete branch' { git branch -q -d $branch }
    $meta = (cargo metadata --no-deps --format-version 1) -join "`n" | ConvertFrom-Json
    $current = ($meta.packages | Where-Object { $_.name -eq 'rooms-client' }).version
    if ($current -ne $Version) { throw "Falha ao subir a versao: Cargo.toml esta em $current" }
}

git ls-remote --exit-code --tags origin "refs/tags/$tag" | Out-Null
if ($LASTEXITCODE -eq 0) { throw "A tag $tag ja existe" }
$global:LASTEXITCODE = 0

Invoke-Step 'cargo ci' { cargo ci }

$env:ZENHA_DEFAULT_SERVER = (gh variable get ZENHA_DEFAULT_SERVER).Trim()
$env:ZENHA_RELEASES_REPO = (gh repo view --json nameWithOwner --jq .nameWithOwner).Trim()
Invoke-Step 'package' { cmd /c scripts\package.cmd }

$zip = "dist\Zenha-$Version-windows-x64.zip"
$setup = "dist\Zenha-$Version-windows-x64-setup.exe"
foreach ($file in $zip, $setup) { if (-not (Test-Path $file)) { throw "Artefato ausente: $file" } }

Invoke-Step 'push' { git push origin main }
$sha = (git rev-parse HEAD).Trim()
# The tag pushed by gh with the user's token starts server-image, and deploy runs after it.
Invoke-Step 'release' { gh release create $tag $setup $zip --target $sha --title "Zenha $tag" --generate-notes --latest }

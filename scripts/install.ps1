# Installs Konnector from the latest GitHub Windows release.
# Run in an elevated PowerShell session:
#   irm https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.ps1 | iex
# Or pin a version:
#   $env:KONNECTOR_VERSION='v0.1.0'; irm ... | iex

$ErrorActionPreference = 'Stop'

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-IsAdmin)) {
    Write-Error 'Run this script in an elevated PowerShell (Run as Administrator).'
}

$Repo = if ($env:KONNECTOR_GITHUB_REPO) { $env:KONNECTOR_GITHUB_REPO } else { 'veliuysal/konnector' }
$Repo = $Repo -replace '^https://github.com/', '' -replace '\.git$', ''

if ($env:KONNECTOR_VERSION) {
    $tag = $env:KONNECTOR_VERSION
    if ($tag -notmatch '^v') { $tag = "v$tag" }
    $ReleaseUrl = "https://api.github.com/repos/$Repo/releases/tags/$tag"
} else {
    $ReleaseUrl = "https://api.github.com/repos/$Repo/releases/latest"
}

Write-Host "Fetching $ReleaseUrl"
$Release = Invoke-RestMethod -Headers @{ Accept = 'application/vnd.github+json'; 'User-Agent' = 'konnector-install' } -Uri $ReleaseUrl
$TagName = $Release.tag_name
$Asset = $Release.assets | Where-Object { $_.name -eq "konnector-$TagName-windows-x86_64.zip" } | Select-Object -First 1
if (-not $Asset) {
    $Asset = $Release.assets | Where-Object { $_.name -like '*windows*.zip' -and $_.name -like '*konnector*' } | Select-Object -First 1
}
if (-not $Asset) {
    Write-Error "No Windows zip found in release $TagName"
}

$Tmp = Join-Path $env:TEMP ("konnector-" + [guid]::NewGuid().ToString() + '.zip')
Write-Host "Downloading $($Asset.browser_download_url)"
Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $Tmp

$Extract = Join-Path $env:TEMP ("konnector-extract-" + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $Extract | Out-Null
Expand-Archive -Path $Tmp -DestinationPath $Extract -Force

$Exe = Get-ChildItem -Path $Extract -Recurse -Filter konnector.exe | Select-Object -First 1
if (-not $Exe) {
    Write-Error 'Downloaded package is missing konnector.exe'
}

Push-Location $Exe.DirectoryName
try {
    & .\konnector.exe install $Tmp
    if ($LASTEXITCODE -ne 0) {
        Write-Error "konnector install failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

Remove-Item -Force $Tmp -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $Extract -ErrorAction SilentlyContinue

Write-Host 'Konnector installed.'
& 'C:\Program Files\Konnector\konnector.exe' status

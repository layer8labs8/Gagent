# Prepares a Windows 10 / 11 machine to run Gagent.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1            check only
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 -Install   install what is missing (Git, and Python if you want it)
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 -Build     also build gagent.exe from source
#
# Without options it only looks and tells you what is missing; nothing is installed.
#   -Install        install Git for Windows (bash for the agent) with winget; with -Python also Python 3
#   -Python         together with -Install
#   -Build          install Rust (rustup + Visual Studio Build Tools) with winget and build gagent.exe from source
#   -InstallDocker  install Docker Desktop (optional, only for real isolation; heavy)
param(
    [switch]$Install,
    [switch]$Python,
    [switch]$Build,
    [switch]$InstallDocker
)

$ErrorActionPreference = 'Stop'

function Test-Command($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

function Find-GitBash {
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)}, "$env:LOCALAPPDATA\Programs")) {
        if (-not $base) { continue }
        foreach ($rel in @('Git\bin\bash.exe', 'Git\usr\bin\bash.exe')) {
            $p = Join-Path $base $rel
            if (Test-Path $p) { return $p }
        }
    }
    return $null
}

function Install-WithWinget($id, $what) {
    if (-not (Test-Command winget)) {
        throw "winget is not available. Install $what by hand."
    }
    Write-Host "Installing $what ..."
    winget install --id $id -e --accept-package-agreements --accept-source-agreements
}

Write-Host "==> Git Bash (the agent runs its commands with it)"
$bash = Find-GitBash
if ($bash) {
    Write-Host "found: $bash"
} elseif ($Install) {
    Install-WithWinget 'Git.Git' 'Git for Windows'
} else {
    Write-Host "Git for Windows is missing. Install it with: winget install Git.Git   (or run this script with -Install)"
}

Write-Host "==> Python (optional, only if your projects use it)"
if (Test-Command python) {
    Write-Host ("found: " + (python --version))
} elseif ($Install -and $Python) {
    Install-WithWinget 'Python.Python.3.12' 'Python 3'
} else {
    Write-Host "Python is not installed. Projects in other languages do not need it. To install: winget install Python.Python.3.12"
}

Write-Host "==> Docker (optional, for real isolation)"
$dockerExe = $null
if (Test-Command docker) {
    $dockerExe = 'docker'
} else {
    foreach ($base in @($env:ProgramFiles, $env:LOCALAPPDATA)) {
        if (-not $base) { continue }
        $candidate = Join-Path $base 'Docker\Docker\resources\bin\docker.exe'
        if (Test-Path $candidate) { $dockerExe = $candidate; break }
    }
}
if ($dockerExe) {
    Write-Host "found: $dockerExe"
} elseif ($InstallDocker) {
    Install-WithWinget 'Docker.DockerDesktop' 'Docker Desktop'
    Write-Host "Restart Windows if asked, then start Docker Desktop once."
} else {
    Write-Host "Not installed. That is fine: Gagent runs code on this computer without it."
}

if ($Build) {
    Write-Host "==> Building Gagent"
    if (-not (Test-Command cargo)) {
        Install-WithWinget 'Rustlang.Rustup' 'Rust'
        Install-WithWinget 'Microsoft.VisualStudio.2022.BuildTools' 'Visual Studio Build Tools'
        $env:Path += ";$env:USERPROFILE\.cargo\bin"
    }
    $root = Split-Path -Parent $PSScriptRoot
    Push-Location $root
    try {
        cargo build --release
        Write-Host "Built: $root\target\release\gagent.exe"
    } finally {
        Pop-Location
    }
}

Write-Host "==> Done. Check the setup with: gagent-cli.exe doctor"

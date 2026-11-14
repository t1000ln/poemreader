# Builds the release executable and packages it into dist/*.zip.
param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Push-Location $Root

try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed"
    }

    $exe = Join-Path $Root "target\release\poemreader.exe"
    if (-not (Test-Path $exe)) {
        throw "missing $exe"
    }

    # The packaged app must not open a console window: PE subsystem 2 = GUI.
    $bytes = [System.IO.File]::ReadAllBytes($exe)
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
    $subsystem = [BitConverter]::ToUInt16($bytes, $peOffset + 24 + 68)
    if ($subsystem -ne 2) {
        throw "executable subsystem is $subsystem, expected 2 (Windows GUI)"
    }

    $dist = Join-Path $Root "dist"
    $name = "PoemReader-$Version-win64"
    $outDir = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"

    if (Test-Path $outDir) { Remove-Item -Recurse -Force $outDir }
    if (Test-Path $zip) { Remove-Item -Force $zip }
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null

    Copy-Item $exe (Join-Path $outDir "PoemReader.exe")
    Copy-Item (Join-Path $Root "README.md") $outDir
    Copy-Item (Join-Path $Root "LICENSE-MIT") $outDir
    Copy-Item (Join-Path $Root "LICENSE-APACHE") $outDir
    Copy-Item (Join-Path $Root "THIRD-PARTY-NOTICES.md") $outDir
    Copy-Item -Recurse (Join-Path $Root "samples") $outDir

    # Compress-Archive can fail on large inputs, so use the .NET zip API.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $outDir,
        $zip,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false
    )

    $exeSize = [math]::Round((Get-Item (Join-Path $outDir "PoemReader.exe")).Length / 1MB, 1)
    $zipSize = [math]::Round((Get-Item $zip).Length / 1MB, 1)

    Write-Host "Release: $outDir"
    Write-Host "Archive: $zip"
    Write-Host "Executable: $exeSize MB, archive: $zipSize MB"
}
finally {
    Pop-Location
}

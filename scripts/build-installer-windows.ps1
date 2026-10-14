# Builds dist/Strigov-Local-MCP-<version>-windows-x64.zip on native Windows x64.
$ErrorActionPreference = 'Stop'
$project = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $project

function Invoke-Native([string]$label, [scriptblock]$command) {
    & $command
    if ($LASTEXITCODE -ne 0) { throw "$label failed with exit code $LASTEXITCODE" }
}

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') { throw 'Run this script on Windows' }
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') { throw 'Windows x64 is required' }
$hostTriple = (rustc -vV | Select-String '^host: ').ToString().Substring(6)
if ($hostTriple -ne 'x86_64-pc-windows-msvc') { throw "Unexpected Rust host $hostTriple" }

Invoke-Native 'Runtime fetch' { python -B scripts/fetch-tunnel-runtime.py --platform windows-amd64 }

# Keep developer paths out of the shipped binaries; only this process is affected.
$homeDir = [Environment]::GetFolderPath('UserProfile')
# The last matching prefix wins in rustc, so the project (inside home) goes last.
# CARGO_ENCODED_RUSTFLAGS keeps paths with spaces intact.
if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) { throw 'Unset RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS for a reproducible release build' }
$env:CARGO_ENCODED_RUSTFLAGS = @("--remap-path-prefix=$homeDir=~", "--remap-path-prefix=$project=.") -join [char]0x1f
$targetDir = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $project 'target' }

Invoke-Native 'Release build' { cargo build --release --locked --bins }
Invoke-Native 'Packaging' { python -B scripts/package-windows.py --target-dir $targetDir }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$zip = Join-Path $project "dist/Strigov-Local-MCP-$version-windows-x64.zip"
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
$recorded = (Get-Content -LiteralPath "$zip.sha256" -Raw).Split(' ')[0]
if ($hash -ne $recorded) { throw 'SHA-256 sidecar mismatch' }
Write-Output "Installer: $zip"
Write-Output "SHA-256:   $hash"

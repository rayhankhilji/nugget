# Builds the WinForms fixture for crates/nugget-a11y/tests/windows_uia.rs with the C# compiler
# that ships with the .NET Framework (present on every Windows install and GitHub runner).
#
# Usage: build.ps1 [-OutDir <dir>]   Prints the path of the built NuggetA11yFixture.exe.
param([string]$OutDir = (Join-Path $PSScriptRoot 'out'))
$ErrorActionPreference = 'Stop'

$csc = @(
    "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe",
    "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $csc) { throw 'csc.exe (.NET Framework 4) not found' }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$exe = Join-Path $OutDir 'NuggetA11yFixture.exe'
& $csc /nologo /target:winexe /optimize+ "/out:$exe" `
    /reference:System.Windows.Forms.dll /reference:System.Drawing.dll `
    (Join-Path $PSScriptRoot 'A11yFixture.cs') | Out-Host
if ($LASTEXITCODE -ne 0) { throw "csc failed with exit code $LASTEXITCODE" }
Write-Output $exe

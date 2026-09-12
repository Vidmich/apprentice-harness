# Local equivalent of the CI checks. Usage: pwsh scripts/check.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
just check

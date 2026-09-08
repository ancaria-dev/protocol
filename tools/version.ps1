<#
.SYNOPSIS
    Prints or sets the version of protocol.exe, everywhere it is written.

.DESCRIPTION
    Cargo.toml is the source of truth. Cargo.lock carries the same number in
    this crate's own [[package]] entry, which a plain cargo build would
    rewrite anyway on the next run, but leaving it stale between now and then
    makes every diff confusing until somebody builds.

.EXAMPLE
    pwsh tools/version.ps1
    pwsh tools/version.ps1 0.99.1
#>
param(
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$tomlPath = Join-Path $root 'Cargo.toml'

$match = Select-String -Path $tomlPath -Pattern '^version = "(.+)"$' | Select-Object -First 1
if (-not $match) { throw "No version = line in $tomlPath" }
$current = $match.Matches[0].Groups[1].Value

if (-not $Version) {
    Write-Host $current
    return
}

$escaped = [regex]::Escape($current)
$touched = 0

$tomlText = Get-Content -Path $tomlPath -Raw
$tomlNew = [regex]::Replace($tomlText, "(?m)^version = `"$escaped`"$", "version = `"$Version`"", 1)
if ($tomlNew -eq $tomlText) { throw "version = `"$current`" not found in $tomlPath" }
Set-Content -Path $tomlPath -Value $tomlNew -NoNewline
$touched++

$lockPath = Join-Path $root 'Cargo.lock'
$lockText = Get-Content -Path $lockPath -Raw
$lockPattern = "(name = `"protocol`"\r?\nversion = `")$escaped(`")"
$lockNew = [regex]::Replace($lockText, $lockPattern, "`${1}$Version`${2}")
if ($lockNew -eq $lockText) {
    Write-Warning "the protocol package entry in Cargo.lock is already $Version, or cargo has not generated it yet"
} else {
    Set-Content -Path $lockPath -Value $lockNew -NoNewline
    $touched++
}

Write-Host "$current -> $Version in $touched file(s)"

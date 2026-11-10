# Execute the real host's installation/application code with isolated paths.
param(
    [string]$HostPath = 'target/tabularis-host',
    [string]$OldVersion = '0.2.0-beta.3'
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'This harness currently isolates Windows profiles only' }
$repoRoot = Split-Path $PSScriptRoot -Parent
$hostRoot = (Resolve-Path -LiteralPath $HostPath).Path
$hostCargo = Join-Path $hostRoot 'src-tauri/Cargo.toml'
$example = Join-Path $hostRoot 'src-tauri/examples/firebird_registry_lifecycle.rs'
if (Test-Path -LiteralPath $example) { throw 'Refusing to overwrite an existing host example' }
$cargoBytes = [IO.File]::ReadAllBytes($hostCargo)
$cargoText = [IO.File]::ReadAllText($hostCargo)
$standaloneCargo = $cargoText
if ($cargoText -notmatch '(?m)^\[workspace\]') { $standaloneCargo += "`n[workspace]`n" }
$testRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot ('target/registry-lifecycle-' + [Guid]::NewGuid().ToString('N'))))
$targetRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target')) + [IO.Path]::DirectorySeparatorChar
if (!$testRoot.StartsWith($targetRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe test root' }
New-Item -ItemType Directory -Path $testRoot | Out-Null
$savedEnvironment = @{}
foreach ($key in @('APPDATA','LOCALAPPDATA','FIREBIRD_REGISTRY_TEST_ROOT','FIREBIRD_REGISTRY_OLD_VERSION','FIREBIRD_REGISTRY_NEW_VERSION','PERL','LC_ALL','LANG')) {
    $savedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
$exampleCreated = $false
try {
    foreach ($key in @('APPDATA','LOCALAPPDATA')) {
        $dir = Join-Path $testRoot $key
        New-Item -ItemType Directory -Path $dir | Out-Null
        [Environment]::SetEnvironmentVariable($key, $dir, 'Process')
    }
    $env:FIREBIRD_REGISTRY_TEST_ROOT = $testRoot
    $env:FIREBIRD_REGISTRY_OLD_VERSION = $OldVersion
    $env:FIREBIRD_REGISTRY_NEW_VERSION = (Get-Content -Raw (Join-Path $repoRoot '.tabularium') | ConvertFrom-Json).version
    $portablePerl = Join-Path $repoRoot 'target/tooling/strawberry/perl/bin/perl.exe'
    if (!$env:PERL -and (Test-Path -LiteralPath $portablePerl)) { $env:PERL = $portablePerl }
    $env:LC_ALL = 'C'
    $env:LANG = 'C'
    New-Item -ItemType Directory -Force -Path (Split-Path $example -Parent) | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'host/registry_lifecycle.rs') -Destination $example
    $exampleCreated = $true
    [IO.File]::WriteAllText($hostCargo, $standaloneCargo, [Text.UTF8Encoding]::new($false))
    $logPath = Join-Path $repoRoot 'target/host-registry-lifecycle.log'
    # Windows PowerShell 5 turns redirected native stderr into ErrorRecords.
    $ErrorActionPreference = 'Continue'
    & cargo run --manifest-path $hostCargo --no-default-features --locked --example firebird_registry_lifecycle *> $logPath
    $testExit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Get-Content -LiteralPath $logPath -Tail 16
    if ($testExit -ne 0) { throw "Host registry lifecycle failed; see $logPath" }
} finally {
    try {
        if ([IO.File]::ReadAllText($hostCargo) -eq $standaloneCargo) {
            [IO.File]::WriteAllBytes($hostCargo, $cargoBytes)
        } else { Write-Warning 'Host Cargo.toml changed during the test; refusing to overwrite concurrent changes.' }
        if ($exampleCreated) { Remove-Item -LiteralPath $example -Force }
        if (!$testRoot.StartsWith($targetRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe cleanup path' }
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    } finally {
        foreach ($key in $savedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($key, $savedEnvironment[$key], 'Process') }
    }
}

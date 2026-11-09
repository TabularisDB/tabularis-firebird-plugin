# Build archives explicitly: Compress-Archive can omit dotfiles such as .tabularium.
param(
    [Parameter(Mandatory)][ValidateSet('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-msvc')][string]$Target,
    [Parameter(Mandatory)][ValidateSet('linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win-x64')][string]$Platform
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$manifest = Get-Content -Raw -LiteralPath (Join-Path $repoRoot '.tabularium') | ConvertFrom-Json
$cargo = Get-Content -Raw -LiteralPath (Join-Path $repoRoot 'Cargo.toml')
if ($cargo -notmatch ('(?m)^version = "' + [regex]::Escape($manifest.version) + '"\r?$')) {
    throw 'Cargo and manifest versions differ'
}
if ($env:GITHUB_REF -like 'refs/tags/v*' -and $env:GITHUB_REF -ne "refs/tags/v$($manifest.version)") {
    throw 'Release tag and manifest version differ'
}
$binary = $manifest.executable + $(if ($Platform -eq 'win-x64') { '.exe' } else { '' })
$archivePath = Join-Path $repoRoot "target/firebird-plugin-$Platform.zip"
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression
$zip = [IO.Compression.ZipFile]::Open($archivePath, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($name in @($binary, '.tabularium', 'LICENSE', 'THIRD_PARTY_NOTICES.md')) {
        $source = if ($name -eq $binary) { "target/$Target/release/$binary" } else { $name }
        $entry = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $repoRoot $source), $name)
        $mode = if ($name -eq $binary) { '100755' } else { '100644' }
        $entry.ExternalAttributes = [Convert]::ToInt32($mode, 8) -shl 16
    }
} finally { $zip.Dispose() }

# Exercise the extracted executable, as installed by the host, on its native runner.
$testDir = Join-Path $repoRoot ('target/package-test-' + [Guid]::NewGuid().ToString('N'))
try {
    [IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $testDir)
    $installedManifest = Get-Content -Raw -LiteralPath (Join-Path $testDir '.tabularium') | ConvertFrom-Json
    if ($installedManifest.name -ne 'firebird' -or $installedManifest.version -ne $manifest.version) {
        throw 'Archive manifest mismatch'
    }
    $executable = Join-Path $testDir $binary
    if ([Environment]::OSVersion.Platform -ne 'Win32NT') {
        & chmod +x $executable
        if ($LASTEXITCODE -ne 0) { throw 'Could not set executable permissions' }
    }
    $requests = @(
        '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}',
        '{"jsonrpc":"2.0","id":2,"method":"test_connection","params":{"params":{}}}'
    )
    $responses = @($requests | & $executable | ForEach-Object { $_ | ConvertFrom-Json })
    if ($LASTEXITCODE -ne 0 -or $responses.Count -ne 2 -or
        $responses[0].id -ne 1 -or $responses[0].error -or
        $responses[1].id -ne 2 -or $responses[1].error.code -ne -32602) {
        throw 'Packaged executable RPC smoke test failed'
    }
    Write-Host "Verified $Platform archive: manifest, licenses, executable discovery and JSON-RPC."
} finally {
    $absolute = [IO.Path]::GetFullPath($testDir)
    $targetDir = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target')) + [IO.Path]::DirectorySeparatorChar
    if (!$absolute.StartsWith($targetDir)) { throw 'Unsafe package cleanup path' }
    if (Test-Path -LiteralPath $absolute) { Remove-Item -LiteralPath $absolute -Recurse -Force }
}

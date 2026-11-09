# Portable Firebird integration test. No service or system installation.
param(
    [ValidateSet('3', '4', '5')][string]$Version = '5',
    [ValidateRange(1024, 65535)][int]$Port = 13050,
    [ValidateSet('Default','ChaCha64','ChaCha','Arc4','Disabled')][string]$ServerWireCrypt = 'Default',
    [scriptblock]$AfterTests
)
$ErrorActionPreference = 'Stop'
$savedProgressPreference = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
$package = switch ($Version) {
    '3' { @{ Version = '3.0.14'; File = 'Firebird-3.0.14.33856-0-x64.zip'; Hash = 'A070BC5633F9FB5AD261D87D19276324679E8EC77AFB03A2BEC056BA7E2D2DAD' } }
    '4' { @{ Version = '4.0.7'; File = 'Firebird-4.0.7.3271-0-x64.zip'; Hash = 'D1CCBFF156750EA712635DC49B3A1405FD1C9F23A952B018C885E7BA6AF59D06' } }
    '5' { @{ Version = '5.0.4'; File = 'Firebird-5.0.4.1812-0-windows-x64.zip'; Hash = '01E844FCE4D5F53272A76205DBC3A1BA4B782AB8E8EADCD808CDBCCD9CE13B72' } }
}
$repoRoot = Split-Path $PSScriptRoot -Parent
Push-Location $repoRoot
$serverProcess = $null
$ownsTestDir = $false
$testDir = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target/firebird-test'))
$savedEnvironment = @{}
foreach ($key in @('FIREBIRD_LOCK', 'FIREBIRD_TEST_CONFIG', 'FIREBIRD_TEST_CIPHER', 'ISC_USER', 'ISC_PASSWORD')) {
    $savedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    # Refuse to interfere with another server already using this port.
    $probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $Port)
    try { $probe.Start() } finally { $probe.Stop() }

    if (Test-Path -LiteralPath $testDir) { throw "Test directory already exists: $testDir" }
    $cacheDir = (New-Item -ItemType Directory -Path $testDir).FullName
    $ownsTestDir = $true
    $archive = Join-Path $cacheDir 'firebird.zip'
    Invoke-WebRequest -Uri "https://github.com/FirebirdSQL/firebird/releases/download/v$($package.Version)/$($package.File)" -OutFile $archive
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash -ne $package.Hash) {
        throw 'Firebird archive checksum mismatch'
    }
    $runDir = (New-Item -ItemType Directory -Path (Join-Path $cacheDir ([Guid]::NewGuid().ToString('N')))).FullName
    $serverDir = Join-Path $runDir 'server'
    Expand-Archive -LiteralPath $archive -DestinationPath $serverDir
    # All processes in this test must use the same directory for shared memory mappings.
    $env:FIREBIRD_LOCK = (New-Item -ItemType Directory -Force -Path (Join-Path $cacheDir 'lock')).FullName
    $env:ISC_USER = $null
    $env:ISC_PASSWORD = $null
    @"
RemoteBindAddress = 127.0.0.1
RemoteServicePort = $Port
IpcName = tabularis_test_$Port
"@ | Add-Content -LiteralPath (Join-Path $serverDir 'firebird.conf')
    $env:FIREBIRD_TEST_CIPHER = if ($ServerWireCrypt -eq 'Default') { $null } else { $ServerWireCrypt }
    if ($ServerWireCrypt -eq 'Disabled') {
        'WireCrypt = Disabled' | Add-Content -LiteralPath (Join-Path $serverDir 'firebird.conf')
    } else {
        'WireCrypt = Required' | Add-Content -LiteralPath (Join-Path $serverDir 'firebird.conf')
        if ($ServerWireCrypt -ne 'Default') { "WireCryptPlugin = $ServerWireCrypt" | Add-Content -LiteralPath (Join-Path $serverDir 'firebird.conf') }
    }
    $databasePath = Join-Path $runDir 'plugin-test.fdb'
    $testPassword = [Guid]::NewGuid().ToString('N')
    $sql = "CREATE DATABASE '$databasePath' USER 'SYSDBA' DEFAULT CHARACTER SET UTF8;`nCREATE OR ALTER USER SYSDBA PASSWORD '$testPassword';`nCOMMIT;`nQUIT;"
    $sql | & (Join-Path $serverDir 'isql.exe') -bail -quiet
    if ($LASTEXITCODE -ne 0) { throw 'Firebird database bootstrap failed' }
    $env:FIREBIRD_TEST_CONFIG = Join-Path $runDir 'connection.json'
    $connection = @{ host = '127.0.0.1'; port = $Port; database = $databasePath; username = 'SYSDBA'; password = $testPassword }
    if ($ServerWireCrypt -eq 'Disabled') { $connection.wire_crypt = 'disabled' }
    $connection |
        ConvertTo-Json | Set-Content -Encoding ascii -LiteralPath $env:FIREBIRD_TEST_CONFIG
    $serverProcess = Start-Process -FilePath (Join-Path $serverDir 'firebird.exe') -ArgumentList '-a' -WorkingDirectory $serverDir -WindowStyle Hidden -PassThru
    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ([DateTime]::UtcNow -lt $deadline -and !$serverProcess.HasExited) {
        $socket = [Net.Sockets.TcpClient]::new()
        try {
            $ready = $socket.ConnectAsync('127.0.0.1', $Port).Wait(200)
            if ($ready) { break }
        } catch { } finally { $socket.Dispose() }
        Start-Sleep -Milliseconds 100
    }
    if (!$ready) { throw 'Firebird test server did not start' }
    $env:ISC_USER = 'SYSDBA'
    $env:ISC_PASSWORD = $testPassword
    & (Join-Path $serverDir 'isql.exe') -bail -quiet -input tests/fixture.sql "127.0.0.1/${Port}:$databasePath"
    if ($LASTEXITCODE -ne 0) { throw 'Firebird fixture creation failed' }
    $testArguments = @('--ignored', '--test-threads=1')
    if ($Version -eq '3') {
        Write-Host 'Skipping Firebird 4+ types (INT128 decimals, time zones, DECFLOAT) on Firebird 3.'
        $testArguments += @('--skip', '_firebird4')
    }
    cargo test --locked live_ -- @testArguments
    if ($LASTEXITCODE -ne 0) { throw 'Firebird integration tests failed' }
    cargo build --locked --bin firebird-plugin
    if ($LASTEXITCODE -ne 0) { throw 'Plugin build failed' }
    $request = @{
        jsonrpc = '2.0'; id = 9; method = 'execute_query'
        params = @{
            params = (Get-Content -Raw -LiteralPath $env:FIREBIRD_TEST_CONFIG | ConvertFrom-Json)
            query = 'SELECT LABEL FROM FB_CHILD ORDER BY ID'; page = 2; limit = 1
        }
    } | ConvertTo-Json -Depth 5 -Compress
    $response = $request | & ./target/debug/firebird-plugin.exe | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $response.id -ne 9 -or $response.error -or
        $response.result.pagination.total_rows -ne 2 -or $response.result.affected_rows -ne 0 -or
        $response.result.rows[0][0] -ne 'beta') {
        throw 'Plugin stdio integration test failed'
    }
    Write-Host "Firebird $($package.Version) integration tests passed (server wire encryption: $ServerWireCrypt)."
    # Optional host verification shares the fixture lifetime and still gets finally cleanup.
    if ($AfterTests) { & $AfterTests }
} finally {
    try {
        if ($null -ne $serverProcess -and !$serverProcess.HasExited) {
            Stop-Process -Id $serverProcess.Id
            $serverProcess.WaitForExit()
        }
        if ($ownsTestDir) {
            $expectedPath = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target/firebird-test'))
            if ($testDir -ne $expectedPath -or !$testDir.StartsWith($repoRoot + [IO.Path]::DirectorySeparatorChar)) {
                throw 'Unsafe Firebird cleanup path'
            }
            Remove-Item -LiteralPath $testDir -Recurse -Force
        }
    } finally {
        foreach ($key in $savedEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $savedEnvironment[$key], 'Process')
        }
        Pop-Location
        $ProgressPreference = $savedProgressPreference
    }
}

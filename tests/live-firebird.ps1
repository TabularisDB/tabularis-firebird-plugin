# Portable Firebird 5 integration test. No service or system installation.
param([ValidateRange(1024, 65535)][int]$Port = 13050)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
Push-Location $repoRoot
$serverProcess = $null
$ownsTestDir = $false
$testDir = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target/firebird-test'))
$savedEnvironment = @{}
foreach ($key in @('FIREBIRD_LOCK', 'FIREBIRD_TEST_CONFIG', 'ISC_USER', 'ISC_PASSWORD')) {
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
    Invoke-WebRequest -Uri 'https://github.com/FirebirdSQL/firebird/releases/download/v5.0.4/Firebird-5.0.4.1812-0-windows-x64.zip' -OutFile $archive
    $expectedHash = '01E844FCE4D5F53272A76205DBC3A1BA4B782AB8E8EADCD808CDBCCD9CE13B72'
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash -ne $expectedHash) {
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
    $databasePath = Join-Path $runDir 'plugin-test.fdb'
    $testPassword = [Guid]::NewGuid().ToString('N')
    $sql = "CREATE DATABASE '$databasePath' USER 'SYSDBA' DEFAULT CHARACTER SET UTF8;`nCREATE OR ALTER USER SYSDBA PASSWORD '$testPassword';`nCOMMIT;`nQUIT;"
    $sql | & (Join-Path $serverDir 'isql.exe') -bail -quiet
    if ($LASTEXITCODE -ne 0) { throw 'Firebird database bootstrap failed' }
    $env:FIREBIRD_TEST_CONFIG = Join-Path $runDir 'connection.json'
    @{ host = '127.0.0.1'; port = $Port; database = $databasePath; username = 'SYSDBA'; password = $testPassword } |
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
    cargo test --locked live_ -- --ignored
    if ($LASTEXITCODE -ne 0) { throw 'Firebird integration tests failed' }
    cargo build --locked --bin firebird-plugin
    if ($LASTEXITCODE -ne 0) { throw 'Plugin build failed' }
    $request = @{
        jsonrpc = '2.0'; id = 9; method = 'execute_query'
        params = @{
            params = (Get-Content -Raw -LiteralPath $env:FIREBIRD_TEST_CONFIG | ConvertFrom-Json)
            query = 'SELECT LABEL FROM FB_CHILD ORDER BY ID'; page = 2; page_size = 1
        }
    } | ConvertTo-Json -Depth 5 -Compress
    $response = $request | & ./target/debug/firebird-plugin.exe | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $response.id -ne 9 -or $response.error -or
        $response.result.total_count -ne 2 -or $response.result.rows[0][0] -ne 'beta') {
        throw 'Plugin stdio integration test failed'
    }
    Write-Host 'Firebird 5.0.4 integration tests passed.'
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
    }
}

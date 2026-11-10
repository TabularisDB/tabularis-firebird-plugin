# Only called inside the disposable live-firebird.ps1 fixture lifetime.
param(
    [Parameter(Mandatory)][ref]$ServerProcess,
    [Parameter(Mandatory)][string]$ServerDir,
    [ValidateRange(5, 7200)][int]$DurationSeconds = 30
)
$ErrorActionPreference = 'Stop'
$config = Get-Content -Raw -LiteralPath $env:FIREBIRD_TEST_CONFIG | ConvertFrom-Json
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = [IO.Path]::GetFullPath('target/debug/firebird-plugin.exe')
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardInput = $true
$start.RedirectStandardOutput = $true
$plugin = [Diagnostics.Process]::Start($start)
$nextId = 0
function Invoke-Query([string]$Sql, [string]$Session = '') {
    $script:nextId++
    $request = @{ jsonrpc = '2.0'; id = $script:nextId; method = 'execute_query'; params = @{ params = $config; query = $Sql } }
    if ($Session) { $request.params.session_id = $Session }
    $plugin.StandardInput.WriteLine(($request | ConvertTo-Json -Depth 12 -Compress))
    $line = $plugin.StandardOutput.ReadLineAsync()
    if (!$line.Wait(15000)) { throw 'Operational test RPC exceeded 15 seconds' }
    if ($null -eq $line.Result) { throw 'Plugin exited during operational test' }
    $response = $line.Result | ConvertFrom-Json
    if ($response.id -ne $script:nextId) { throw 'Unexpected operational RPC response ID' }
    if ($response.error) { throw 'Operational test returned a transport/RPC error' }
    return $response.result
}
function Invoke-Success([string]$Sql, [string]$Session = '') {
    $result = Invoke-Query $Sql $Session
    if ($result.error) { throw ('Operational SQL failed: ' + $result.error) }
    if ($result.PSObject.Properties['result']) { return $result.result }
    return $result
}
try {
    $null = Invoke-Success 'CREATE TABLE FB_RESTART_TEST (ID INTEGER PRIMARY KEY)'
    $null = Invoke-Success 'BEGIN' 'restart'
    $null = Invoke-Success 'INSERT INTO FB_RESTART_TEST VALUES (1)' 'restart'
    Stop-Process -Id $ServerProcess.Value.Id -Force
    if (!$ServerProcess.Value.WaitForExit(5000)) { throw 'Test server did not stop' }
    $offline = Invoke-Query 'SELECT COUNT(*) FROM FB_RESTART_TEST' 'restart'
    if (!$offline.error -or $offline.in_transaction -ne $false) {
        throw 'A stopped server must fail the existing session and clear its lost transaction'
    }
    $ServerProcess.Value = Start-Process -FilePath (Join-Path $ServerDir 'firebird.exe') -ArgumentList '-a' -WorkingDirectory $ServerDir -WindowStyle Hidden -PassThru
    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ([DateTime]::UtcNow -lt $deadline -and !$ServerProcess.Value.HasExited) {
        $probe = [Net.Sockets.TcpClient]::new()
        try { $ready = $probe.ConnectAsync('127.0.0.1', $config.port).Wait(200) }
        catch { $ready = $false }
        finally { $probe.Dispose() }
        if ($ready) { break }
        Start-Sleep -Milliseconds 100
    }
    if (!$ready) { throw 'Test server did not restart' }
    $count = Invoke-Success 'SELECT COUNT(*) FROM FB_RESTART_TEST' 'restart'
    if ($count.rows[0][0] -ne 0) { throw 'Uncommitted data survived a complete server stop' }
    $null = Invoke-Success 'INSERT INTO FB_RESTART_TEST VALUES (2)' 'restart'
    $count = Invoke-Success 'SELECT COUNT(*) FROM FB_RESTART_TEST' 'restart'
    if ($count.rows[0][0] -ne 1) { throw 'Fresh work after server restart failed' }
    $null = Invoke-Success 'DROP TABLE FB_RESTART_TEST'

    $sql = "WITH RECURSIVE N(I) AS (SELECT 1 FROM RDB`$DATABASE UNION ALL SELECT I+1 FROM N WHERE I<1000) SELECT I, RPAD('x',512,'x') AS PAYLOAD FROM N"
    for ($warm = 0; $warm -lt 5; $warm++) { $null = Invoke-Success $sql }
    $samples = [Collections.Generic.List[long]]::new()
    $latencies = [Collections.Generic.List[double]]::new()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $requestClock = [Diagnostics.Stopwatch]::StartNew()
        $result = Invoke-Success $sql
        if ($result.rows.Count -ne 1000 -or $result.rows[999][0] -ne 1000 -or $result.rows[0][1].Length -ne 512) {
            throw 'Load test returned truncated or corrupted data'
        }
        $latencies.Add($requestClock.Elapsed.TotalMilliseconds)
        $plugin.Refresh()
        $samples.Add($plugin.WorkingSet64)
    } while ($clock.Elapsed.TotalSeconds -lt $DurationSeconds)
    $clock.Stop()
    $sorted = @($latencies | Sort-Object)
    $report = [ordered]@{
        recorded_utc = [DateTime]::UtcNow.ToString('o')
        firebird_version = (Invoke-Success "SELECT RDB`$GET_CONTEXT('SYSTEM','ENGINE_VERSION') FROM RDB`$DATABASE").rows[0][0]
        os = [Environment]::OSVersion.VersionString
        logical_processors = [Environment]::ProcessorCount
        server_restart_rollback_and_recovery = $true
        duration_seconds = [Math]::Round($clock.Elapsed.TotalSeconds, 3)
        requests = $samples.Count
        rows_per_request = 1000
        payload_characters_per_row = 512
        requests_per_second = [Math]::Round($samples.Count / $clock.Elapsed.TotalSeconds, 3)
        latency_p50_ms = [Math]::Round($sorted[[int][Math]::Floor(($sorted.Count - 1) * 0.50)], 3)
        latency_p95_ms = [Math]::Round($sorted[[int][Math]::Floor(($sorted.Count - 1) * 0.95)], 3)
        rss_first_bytes = $samples[0]
        rss_last_bytes = $samples[$samples.Count - 1]
        rss_peak_bytes = ($samples | Measure-Object -Maximum).Maximum
    }
    $reportDir = Join-Path $PSScriptRoot '../target/operations'
    New-Item -ItemType Directory -Force -Path $reportDir | Out-Null
    $reportPath = Join-Path $reportDir ("firebird-" + $report.firebird_version + '.json')
    $report | ConvertTo-Json | Set-Content -Encoding ascii -LiteralPath $reportPath
    Write-Host "Verified full server restart and $($samples.Count) load requests; report: $reportPath"
} finally {
    try {
        $plugin.StandardInput.Close()
        if (!$plugin.WaitForExit(5000)) { $plugin.Kill(); $plugin.WaitForExit() }
    } finally { $plugin.Dispose() }
}

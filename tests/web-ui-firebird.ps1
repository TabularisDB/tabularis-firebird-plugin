# Interactive browser validation with an isolated real host and disposable Firebird fixture.
# Build the frontend first; the explicitly isolated host example is built here.
param(
    [string]$HostPath = 'target/tabularis-host',
    [ValidateRange(1024,65535)][int]$WebPort = 13080,
    [ValidateRange(60,1800)][int]$LifetimeSeconds = 1200
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'This launcher currently isolates Windows profiles only' }
$repoRoot = Split-Path $PSScriptRoot -Parent
$infoPath = Join-Path $repoRoot 'target/web-ui-firebird-info.json'
if (Test-Path -LiteralPath $infoPath) { throw 'Another UI test may be active; refusing to overwrite its runtime information' }
$hostSourceRoot = (Resolve-Path -LiteralPath $HostPath).Path
$hostExe = Join-Path $hostSourceRoot 'src-tauri/target/debug/examples/firebird_web_ui.exe'
$webAssets = Join-Path $hostSourceRoot 'packages/web-ui/dist'
if (!(Test-Path -LiteralPath (Join-Path $webAssets 'index.html'))) { throw 'Build frontend first' }
$hostManifest = Join-Path $hostSourceRoot 'src-tauri/Cargo.toml'
$examplePath = Join-Path $hostSourceRoot 'src-tauri/examples/firebird_web_ui.rs'
if (Test-Path -LiteralPath $examplePath) { throw 'Refusing to overwrite a host example' }
$manifestBytes = [IO.File]::ReadAllBytes($hostManifest)
$manifestText = [IO.File]::ReadAllText($hostManifest)
$standaloneManifest = $manifestText
if ($manifestText -notmatch '(?m)^\[workspace\]') { $standaloneManifest += "`n[workspace]`n" }
$savedBuildPerl = $env:PERL
try {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'host/web_ui.rs') -Destination $examplePath
    [IO.File]::WriteAllText($hostManifest, $standaloneManifest, [Text.UTF8Encoding]::new($false))
    $portablePerl = Join-Path $repoRoot 'target/tooling/strawberry/perl/bin/perl.exe'
    if (!$env:PERL -and (Test-Path -LiteralPath $portablePerl)) { $env:PERL = $portablePerl }
    $ErrorActionPreference = 'Continue'
    cargo build --manifest-path $hostManifest --no-default-features --locked --example firebird_web_ui *> (Join-Path $repoRoot 'target/web-ui-host-build.log')
    $buildExit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($buildExit -ne 0) { throw 'Isolated host example build failed; see target/web-ui-host-build.log' }
} finally {
    if ([IO.File]::ReadAllText($hostManifest) -eq $standaloneManifest) { [IO.File]::WriteAllBytes($hostManifest, $manifestBytes) }
    Remove-Item -LiteralPath $examplePath -Force -ErrorAction SilentlyContinue
    $env:PERL = $savedBuildPerl
}
$uiProfileRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot ('target/web-ui-firebird-' + [Guid]::NewGuid().ToString('N'))))
$uiTargetRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target')) + [IO.Path]::DirectorySeparatorChar
if (!$uiProfileRoot.StartsWith($uiTargetRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe UI test root' }
$probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $WebPort)
try { $probe.Start() } finally { $probe.Stop() }
New-Item -ItemType Directory -Path $uiProfileRoot | Out-Null
$webState = @{ Process = $null }
$savedWebEnvironment = @{}
foreach ($name in @('APPDATA','LOCALAPPDATA','TABULARIS_WEB_PASSWORD','TABULARIS_DATA_DIR','FIREBIRD_UI_PORT','FIREBIRD_UI_WEB_ROOT')) { $savedWebEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
function Stop-TestWebHost {
    if ($null -eq $webState.Process) { return }
    $children = @(Get-CimInstance Win32_Process | Where-Object {
        $_.ParentProcessId -eq $webState.Process.Id -and $_.ExecutablePath -and
        $_.ExecutablePath.StartsWith($uiProfileRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)
    })
    if (!$webState.Process.HasExited) { $webState.Process.Kill(); $webState.Process.WaitForExit() }
    foreach ($child in $children) {
        Stop-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
        Wait-Process -Id $child.ProcessId -Timeout 5 -ErrorAction SilentlyContinue
    }
}
try {
    foreach ($name in @('APPDATA','LOCALAPPDATA')) {
        $dir = Join-Path $uiProfileRoot $name
        New-Item -ItemType Directory -Path (Join-Path $dir 'tabularis') -Force | Out-Null
        [Environment]::SetEnvironmentVariable($name, $dir, 'Process')
    }
    $env:TABULARIS_DATA_DIR = Join-Path $uiProfileRoot 'storage'
    New-Item -ItemType Directory -Path $env:TABULARIS_DATA_DIR | Out-Null
    $env:FIREBIRD_UI_PORT = [string]$WebPort
    $env:FIREBIRD_UI_WEB_ROOT = $webAssets
    @{
        activeExternalDrivers = @(); showWelcome = $false; checkForUpdates = $false
        autoCheckUpdatesOnStartup = $false; pingInterval = 0
    } | ConvertTo-Json | Set-Content -Encoding ascii -LiteralPath (Join-Path $env:TABULARIS_DATA_DIR 'config.json')
    $env:TABULARIS_WEB_PASSWORD = [Guid]::NewGuid().ToString('N')
    $passwordFile = Join-Path $uiProfileRoot 'web-password'
    [IO.File]::WriteAllText($passwordFile, $env:TABULARIS_WEB_PASSWORD)
    $stopFile = Join-Path $uiProfileRoot 'stop.request'
    $base = "http://127.0.0.1:$WebPort"
    & (Join-Path $PSScriptRoot 'live-firebird.ps1') -Version 5 -AfterTests {
        try {
            $startOptions = @{
                FilePath = $hostExe; WindowStyle = 'Hidden'; PassThru = $true
                RedirectStandardOutput = (Join-Path $uiProfileRoot 'host.stdout.log')
                RedirectStandardError = (Join-Path $uiProfileRoot 'host.stderr.log')
            }
            $webState.Process = Start-Process @startOptions
            $ready = $false
            $deadline = [DateTime]::UtcNow.AddSeconds(30)
            while ([DateTime]::UtcNow -lt $deadline -and !$webState.Process.HasExited) {
                try { $response = Invoke-WebRequest -UseBasicParsing -Uri "$base/healthz"; $ready = $response.StatusCode -eq 200 }
                catch { $ready = $false }
                if ($ready) { break }
                Start-Sleep -Milliseconds 200
            }
            if (!$ready) { throw 'Isolated web host did not become ready' }
            @{
                base_url = $base; stop_file = $stopFile; profile_root = $uiProfileRoot
                web_password_file = $passwordFile; firebird_config_file = $env:FIREBIRD_TEST_CONFIG
            } | ConvertTo-Json | Set-Content -Encoding ascii -LiteralPath $infoPath
            Write-Host "Isolated UI fixture ready; runtime information: $infoPath"
            $deadline = [DateTime]::UtcNow.AddSeconds($LifetimeSeconds)
            while (!(Test-Path -LiteralPath $stopFile) -and [DateTime]::UtcNow -lt $deadline -and !$webState.Process.HasExited) { Start-Sleep -Milliseconds 500 }
            if ($webState.Process.HasExited) { throw 'Web host exited during UI validation' }
            if (!(Test-Path -LiteralPath $stopFile)) { throw 'UI test lifetime expired; fixture cleaned automatically' }
        } finally { Stop-TestWebHost }
    }
} finally {
    try {
        Stop-TestWebHost
        if (Test-Path -LiteralPath $infoPath) { Remove-Item -LiteralPath $infoPath -Force }
        if (!$uiProfileRoot.StartsWith($uiTargetRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe UI cleanup path' }
        Remove-Item -LiteralPath $uiProfileRoot -Recurse -Force
    } finally {
        foreach ($name in $savedWebEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $savedWebEnvironment[$name], 'Process') }
    }
}

# Verify that a whole-database export also restores through Firebird's own parser.
# The caller owns the isolated server, directory, credentials and final cleanup.
param([Parameter(Mandatory=$true)][string]$IsqlPath)
$ErrorActionPreference='Stop'
$config=Get-Content -Raw -LiteralPath $env:FIREBIRD_TEST_CONFIG | ConvertFrom-Json
$folder=Split-Path $env:FIREBIRD_TEST_CONFIG -Parent
$dump=Join-Path $folder 'isql-roundtrip.sql'
$plugin=Join-Path (Split-Path $PSScriptRoot -Parent) 'target/debug/firebird-plugin.exe'
$request=@{jsonrpc='2.0';id=1;method='export_database_sql';params=@{params=$config;file_path=$dump}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if($LASTEXITCODE -ne 0 -or $response.error -or !$response.result.tables){throw 'Independent isql export failed'}
$target="$($config.host)/$($config.port):$($config.database).isql.fdb"
"CREATE DATABASE '$target' DEFAULT CHARACTER SET UTF8;`nQUIT;" | & $IsqlPath -bail -quiet
if($LASTEXITCODE -ne 0){throw 'isql target creation failed'}
& $IsqlPath -bail -quiet -nodbtriggers -ch UTF8 -input $dump $target
if($LASTEXITCODE -ne 0){throw 'isql could not restore the SQL export'}
$config.database += '.isql.fdb'
$request=@{jsonrpc='2.0';id=2;method='execute_query';params=@{params=$config;query='SELECT COUNT(*) FROM FB_CHILD';page=1;limit=10}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if($LASTEXITCODE -ne 0 -or $response.error -or $response.result.rows[0][0] -ne 2){throw 'isql restored data did not match'}
Write-Host 'Independent isql whole-database restore passed.'

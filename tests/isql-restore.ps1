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
$legacy="$($config.host)/$($config.port):$($config.database).dialect1.fdb"
"SET SQL DIALECT 1;`nCREATE DATABASE '$legacy' DEFAULT CHARACTER SET UTF8;`nQUIT;" | & $IsqlPath -bail -quiet
if($LASTEXITCODE -ne 0){throw 'Dialect 1 guard fixture creation failed'}
$config.database += '.dialect1.fdb'
$before=(Get-FileHash -Algorithm SHA256 -LiteralPath $dump).Hash
$request=@{jsonrpc='2.0';id=3;method='export_database_sql';params=@{params=$config;file_path=$dump}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if(!$response.error -or $response.error.message -notlike '*Dialect 3*' -or (Get-FileHash -Algorithm SHA256 -LiteralPath $dump).Hash -ne $before){throw 'Dialect 1 export did not protect the previous dump'}
$request=@{jsonrpc='2.0';id=4;method='import_database_sql';params=@{params=$config;file_path=$dump;create_database=$false}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if(!$response.error -or $response.error.message -notlike '*Dialect 3*'){throw 'Dialect 1 import target was not rejected'}
Write-Host 'Dialect 1 transfer guards preserve the previous export and reject an incompatible target.'
$nativeDump=Join-Path $folder 'dialect1-native.fbk'
$request=@{jsonrpc='2.0';id=5;method='backup_database';params=@{params=$config;file_path=$nativeDump}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if($LASTEXITCODE -ne 0 -or $response.error -or $response.result.bytes -le 0){throw 'Native backup of a Dialect 1 database failed'}
$nativeConfig=$config | ConvertTo-Json | ConvertFrom-Json
$nativeConfig.database += '.native-restored.fdb'
$request=@{jsonrpc='2.0';id=6;method='restore_database';params=@{params=$nativeConfig;file_path=$nativeDump;create_database=$true}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if($LASTEXITCODE -ne 0 -or $response.error -or !$response.result.created_database){throw 'Native Dialect 1 restore failed'}
$request=@{jsonrpc='2.0';id=7;method='execute_query';params=@{params=$nativeConfig;query='SELECT MON$SQL_DIALECT FROM MON$DATABASE'}}|ConvertTo-Json -Depth 8 -Compress
$response=$request | & $plugin | ConvertFrom-Json
if($LASTEXITCODE -ne 0 -or $response.error -or $response.result.rows[0][0] -ne 1){throw 'Native restore did not preserve SQL Dialect 1'}
$gbak=Join-Path (Split-Path $IsqlPath -Parent) 'gbak.exe'
$toolTarget="$($config.host)/$($config.port):$($config.database).gbak.fdb"
& $gbak -create -kill $nativeDump $toolTarget
if($LASTEXITCODE -ne 0){throw 'Independent gbak could not restore the native backup'}
Write-Host 'Native backup preserves Dialect 1 and restores through independent gbak.'

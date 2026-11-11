param([Parameter(Mandatory=$true)][string]$ConfigurationFile)
$ErrorActionPreference='Stop'
$config=Get-Content -Raw -LiteralPath $ConfigurationFile | ConvertFrom-Json
Add-Type -Path (Join-Path $PSScriptRoot 'native/ArrayFixture.cs')
$database="$($config.host)/$($config.port):$($config.database)"
[FirebirdArrayFixture]::Populate($env:FIREBIRD_TEST_LIBRARY,$database,$config.username,$config.password)

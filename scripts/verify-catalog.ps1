param([switch]$WriteReport, [string]$ProjectRoot)
$ErrorActionPreference = 'Stop'
$root = if ($ProjectRoot) { (Resolve-Path -LiteralPath $ProjectRoot).Path } else { Split-Path $PSScriptRoot -Parent }
$binary = Join-Path $root 'target/release/application-pre-installer.exe'
if (!(Test-Path -LiteralPath $binary)) { throw '先に cargo build --release --locked を実行してください。' }
$output = & $binary --scan-apps --accept-source-agreements | Out-String
if ($LASTEXITCODE -ne 0) { throw "一覧の取得・検証に失敗しました: $output" }
$report = $output | ConvertFrom-Json
if ($report.presets -ne $false -or $report.count -le 0) { throw '取得結果が不正です。' }
if ($WriteReport) {
    $target = Join-Path $root 'target'
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $target 'catalog-validation.json') -Encoding UTF8
}
Write-Host "WinGet catalog: $($report.count) entries; no bundled presets; no installation performed"

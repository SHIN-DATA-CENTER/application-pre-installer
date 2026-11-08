$apps = @()
$warnings = @()
$roots = @(
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKCU:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
)
foreach ($root in $roots) {
    if (!(Test-Path -LiteralPath $root)) { continue }
    try {
        foreach ($key in Get-ChildItem -LiteralPath $root -ErrorAction Stop) {
            try {
                $app = Get-ItemProperty -LiteralPath $key.PSPath -ErrorAction Stop
                if (!$app.DisplayName -or $app.SystemComponent -eq 1 -or $app.ParentKeyName) { continue }
                $apps += [ordered]@{ name = [string]$app.DisplayName; version = [string]$app.DisplayVersion; publisher = [string]$app.Publisher }
            } catch { $warnings += "アプリ登録の一部を読み取れません: $($_.Exception.Message)" }
        }
    } catch { $warnings += "アプリ一覧の一部を読み取れません: $($_.Exception.Message)" }
}
[ordered]@{ apps = @($apps); warnings = @($warnings) } | ConvertTo-Json -Depth 5 -Compress

# Embedded fixed-function worker. No command strings, wildcards, elevation, all-user or provisioning removal.
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$all = @(Get-AppxPackage -PackageTypeFilter Main,Bundle,Framework,Resource -ErrorAction Stop)
function Get-BlockedReason($p) {
    if ($p.NonRemovable -ne $false) { return 'Windowsで削除不可に設定されています' }
    if ($p.IsFramework -ne $false -or $p.IsResourcePackage -ne $false -or $p.IsDevelopmentMode -ne $false) { return 'システム部品・開発用パッケージは対象外です' }
    if ($p.Status.ToString() -ne 'Ok') { return 'パッケージの正常な登録状態を確認できません' }
    if ($p.SignatureKind.ToString() -notin @('Store','System')) { return 'Microsoft Store / Windowsの署名を確認できません' }
    if (@($all | Where-Object { $_.PackageFamilyName -ne $p.PackageFamilyName -and @($_.Dependencies.PackageFamilyName) -contains $p.PackageFamilyName }).Count -gt 0) { return 'ほかのアプリから必要とされています' }
    return ''
}
if ($payload.action -eq 'scan') {
    $result = @(
        foreach ($known in $payload.known) {
            $p = $all | Where-Object { $_.Name -eq $known.name -and $_.PackageFamilyName -eq $known.family -and $_.IsFramework -eq $false -and $_.IsResourcePackage -eq $false } | Sort-Object -Property @{Expression='IsBundle';Descending=$true},@{Expression='Version';Descending=$true} | Select-Object -First 1
            if ($null -eq $p) { continue }
            $reason = Get-BlockedReason $p
            @{ name=[string]$p.Name; family=[string]$p.PackageFamilyName; full_name=[string]$p.PackageFullName; owner_sid=$sid; version=[string]$p.Version; removable=($reason -eq ''); blocked_reason=[string]$reason }
        }
    )
    ConvertTo-Json -InputObject $result -Depth 4 -Compress
    return
}
$target = $payload.app
if ($null -eq $target -or $sid -ne $target.owner_sid) { throw '検出時と利用者が異なります。この利用者で再検出してください。' }
if (@($payload.known | Where-Object { $_.name -eq $target.name -and $_.family -eq $target.family }).Count -ne 1) { throw '対応していないWindowsアプリです' }
$family = @($all | Where-Object { $_.PackageFamilyName -eq $target.family })
if ($payload.action -eq 'verify') {
    if ($family.Count -eq 0) { @{status='success';message='再検査で現在の利用者からの削除を確認しました'} | ConvertTo-Json -Compress }
    else { @{status='unknown';message='アプリがまだ登録されています。自動再削除はしません。状態を確認して選び直してください。'} | ConvertTo-Json -Compress }
    return
}
if ($payload.action -ne 'remove') { throw '未対応の操作です' }
if ($family.Count -eq 0) { @{status='skipped';message='現在の利用者には登録されていないためスキップしました'} | ConvertTo-Json -Compress; return }
$p = $family | Where-Object { $_.Name -eq $target.name -and $_.IsFramework -eq $false -and $_.IsResourcePackage -eq $false } | Sort-Object -Property @{Expression='IsBundle';Descending=$true},@{Expression='Version';Descending=$true} | Select-Object -First 1
if ($null -eq $p -or $p.PackageFullName -ne $target.full_name) { @{status='awaiting_user';message='検出後にパッケージが更新・変更されました。再検出して選び直してください。'} | ConvertTo-Json -Compress; return }
$reason = Get-BlockedReason $p
if ($reason -ne '') { @{status='awaiting_user';message="削除を保留しました: $reason"} | ConvertTo-Json -Compress; return }
try {
    Remove-AppxPackage -Package $p.PackageFullName -Confirm:$false -ErrorAction Stop
    $remaining = @(Get-AppxPackage -PackageTypeFilter Main,Bundle -ErrorAction Stop | Where-Object { $_.PackageFamilyName -eq $target.family })
    if ($remaining.Count -eq 0) { @{status='success';message='現在の利用者からの削除を確認しました'} | ConvertTo-Json -Compress }
    else { @{status='unknown';message='削除処理後も登録が残っています。状態を確認してください。'} | ConvertTo-Json -Compress }
} catch {
    @{status='failed';message="削除に失敗しました。対象アプリを閉じ、設定・組織のポリシーを確認してください。`n$($_.Exception.Message)"} | ConvertTo-Json -Compress
}

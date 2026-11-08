# Called only by the elevated, constrained driver worker. The payload contains identity, never code.
if ($payload.update_id -notmatch '^[0-9a-fA-F-]{36}$' -or [int]$payload.revision -lt 1) { throw 'ドライバーIDが不正です' }
$devices = @(Get-PresentDevices)
$session = New-DriverSearcher
$criteria = "IsInstalled=0 and IsHidden=0 and Type='Driver' and UpdateID='$($payload.update_id)' and RevisionNumber=$([int]$payload.revision)"
$search = $session.CreateUpdateSearcher().Search($criteria)
if ([int]$search.ResultCode -ne 2) { throw "適合性の再確認に失敗しました: $($search.ResultCode)" }
if ($search.Updates.Count -eq 0) {
    # Distinguish installed from withdrawn/not-applicable; absence alone never means success.
    $old = $session.CreateUpdateSearcher().Search("IsInstalled=1 and Type='Driver' and UpdateID='$($payload.update_id)'")
    if ([int]$old.ResultCode -eq 2 -and $old.Updates.Count -gt 0) {
        @{ status = 'skipped'; message = '対象更新は既にインストールされています' } | ConvertTo-Json -Compress
    } else {
        @{ status = 'unknown'; message = '対象更新が見つかりません。撤回・適合条件変更の可能性があるため再検索してください' } | ConvertTo-Json -Compress
    }
    exit
}
$u = $search.Updates.Item(0)
if ($null -eq (Get-EligibleDriver $u $devices)) { throw '対象外・ファームウェア・ハードウェア不一致の更新を拒否しました' }
$updates = New-Object -ComObject Microsoft.Update.UpdateColl
$null = $updates.Add($u)
if (!$u.EulaAccepted) { $u.AcceptEula() }
$downloader = $session.CreateUpdateDownloader()
$downloader.Updates = $updates
$download = $downloader.Download()
if ([int]$download.ResultCode -ne 2 -or !$u.IsDownloaded) { throw "ドライバーダウンロード失敗: $($download.HResult)" }
$installer = $session.CreateUpdateInstaller()
$installer.Updates = $updates
$installer.AllowSourcePrompts = $false
$installer.ForceQuiet = $true
if ($installer.RebootRequiredBeforeInstallation) {
    @{ status = 'reboot_required'; message = 'インストール前に再起動が必要です。再起動後に状態を確認して再開してください' } | ConvertTo-Json -Compress
    exit
}
$result = $installer.Install()
$item = $result.GetUpdateResult(0)
if ([int]$item.ResultCode -eq 2) {
    if ($result.RebootRequired -or $item.RebootRequired) {
        @{ status = 'reboot_required'; message = 'ドライバーを適用しました。完了には利用者による再起動が必要です' } | ConvertTo-Json -Compress
    } else {
        @{ status = 'success'; message = 'Windows Updateがドライバーのインストール成功を確認しました' } | ConvertTo-Json -Compress
    }
} else {
    @{ status = 'failed'; message = "ドライバー導入失敗。ResultCode=$($item.ResultCode) HRESULT=$($item.HResult)" } | ConvertTo-Json -Compress
}

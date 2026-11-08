$devices = @(Get-PresentDevices)
$session = New-DriverSearcher
$search = $session.CreateUpdateSearcher().Search("IsInstalled=0 and IsHidden=0 and Type='Driver'")
if ([int]$search.ResultCode -ne 2) { throw "Windows Update検索が完了しませんでした。ResultCode=$($search.ResultCode)。組織のポリシー・通信状態・Windows Updateサービスを確認してください。" }
$candidates = @()
$excluded = 0
for ($i=0; $i -lt $search.Updates.Count; $i++) {
    $candidate = Get-EligibleDriver $search.Updates.Item($i) $devices
    if ($null -ne $candidate) { $candidates += $candidate } else { $excluded++ }
}
@{ candidates = @($candidates); excluded = $excluded } | ConvertTo-Json -Depth 8 -Compress

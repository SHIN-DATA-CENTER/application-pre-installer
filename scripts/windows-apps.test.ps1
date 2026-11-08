# All package operations in this test are mocks. Never call the machine's Appx cmdlets.
$worker = [scriptblock]::Create($payload.script)
$script:fakePackages = @()
$script:removeCalls = @()
function Get-AppxPackage { param($PackageTypeFilter, $ErrorAction) $script:fakePackages }
function Remove-AppxPackage {
    param($Package, $Confirm, $ErrorAction)
    $script:removeCalls += $Package
    if ($script:simulateFailure) { throw 'mock access denied' }
    $script:fakePackages = @()
}
$payload = [pscustomobject]@{
    action='remove'
    known=@(@{name='Microsoft.BingWeather';family='Microsoft.BingWeather_8wekyb3d8bbwe'})
    app=[pscustomobject]@{name='Microsoft.BingWeather';family='Microsoft.BingWeather_8wekyb3d8bbwe';full_name='Microsoft.BingWeather_1.0.0.0_x64__8wekyb3d8bbwe';owner_sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value}
}
function New-FakePackage {
    [pscustomobject]@{Name='Microsoft.BingWeather';PackageFamilyName=$payload.app.family;PackageFullName=$payload.app.full_name;Version='1.0.0.0';IsBundle=$false;NonRemovable=$false;IsFramework=$false;IsResourcePackage=$false;IsDevelopmentMode=$false;Status='Ok';SignatureKind='Store';Dependencies=@()}
}
$results = @{}
foreach ($case in @('success','absent','changed','protected','framework','dependent','badstatus','unknownsignature','failure','verify_present','verify_absent')) {
    $script:fakePackages = @(New-FakePackage)
    $script:removeCalls = @()
    $script:simulateFailure = ($case -eq 'failure')
    $payload.action = if ($case.StartsWith('verify')) { 'verify' } else { 'remove' }
    switch ($case) {
        absent { $script:fakePackages=@() }
        changed { $script:fakePackages[0].PackageFullName='Microsoft.BingWeather_2.0.0.0_x64__8wekyb3d8bbwe' }
        protected { $script:fakePackages[0].NonRemovable=$true }
        framework { $script:fakePackages[0].IsFramework=$true }
        dependent { $script:fakePackages += [pscustomobject]@{PackageFamilyName='Other.App_test';Dependencies=@(@{PackageFamilyName=$payload.app.family})} }
        badstatus { $script:fakePackages[0].Status='Modified' }
        unknownsignature { $script:fakePackages[0].SignatureKind='Developer' }
        verify_absent { $script:fakePackages=@() }
    }
    $reply = (& $worker) | ConvertFrom-Json
    $results[$case] = @{status=$reply.status;calls=@($script:removeCalls)}
}
$payload.action='remove'
$payload.app.owner_sid='S-1-5-21-0-0-0-9999'
$script:removeCalls=@()
try { $null = & $worker; $results.wrong_user=@{blocked=$false} }
catch { $results.wrong_user=@{blocked=$true;calls=@($script:removeCalls)} }
$payload.app.owner_sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$payload.action='scan'
$bundle=New-FakePackage
$bundle.IsBundle=$true
$bundle.PackageFullName='Microsoft.BingWeather_1.0.0.0_neutral_~_8wekyb3d8bbwe'
$script:fakePackages=@((New-FakePackage),$bundle)
$scanReply = (& $worker) | ConvertFrom-Json
$results.scan = [object[]]$scanReply
$results | ConvertTo-Json -Depth 8 -Compress

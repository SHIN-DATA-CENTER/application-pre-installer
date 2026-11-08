$os = Get-CimInstance Win32_OperatingSystem
$pc = Get-CimInstance Win32_ComputerSystem
$board = Get-CimInstance Win32_BaseBoard | Select-Object -First 1
$signed = @{}
$warnings = @()
try { Get-CimInstance Win32_PnPSignedDriver | ForEach-Object { $signed[[string]$_.DeviceID] = $_ } }
catch { $warnings += "ドライバー情報の一部を取得できません: $($_.Exception.Message)" }
$devices = @(Get-CimInstance Win32_PnPEntity | Where-Object { $_.Present -ne $false } | ForEach-Object {
    $d = $signed[[string]$_.DeviceID]
    [ordered]@{
        name = [string]$_.Name; instance_id = [string]$_.DeviceID; class = [string]$_.PNPClass
        hardware_ids = @(@($_.HardwareID) + @($_.CompatibleID) | Where-Object { $_ } | ForEach-Object { [string]$_ })
        problem_code = [int]$_.ConfigManagerErrorCode; version = [string]$d.DriverVersion; provider = [string]$d.DriverProviderName
    }
})
$installed = @()
foreach ($key in @('HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*')) {
    $installed += @(Get-ItemProperty $key -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName } | ForEach-Object {
        @{ name = [string]$_.DisplayName; version = [string]$_.DisplayVersion }
    })
}
$wg = ''
if (Get-Command winget.exe -ErrorAction SilentlyContinue) { $wg = [string](& winget.exe --version 2>$null | Select-Object -First 1) }
$arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -eq 'X64') { 'x64' } else { [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() }
[ordered]@{
    os = [string]$os.Caption; build = [int]$os.BuildNumber; architecture = $arch
    windows_client = ([int]$os.ProductType -eq 1); network_available = [Net.NetworkInformation.NetworkInterface]::GetIsNetworkAvailable()
    manufacturer = [string]$pc.Manufacturer; model = [string]$pc.Model
    board_manufacturer = [string]$board.Manufacturer; board_product = [string]$board.Product; board_revision = [string]$board.Version
    winget = $wg.Trim(); devices = $devices; installed = $installed; warnings = $warnings
} | ConvertTo-Json -Depth 8 -Compress

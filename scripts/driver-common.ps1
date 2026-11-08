function Get-PresentDevices {
    $versions = @{}
    Get-CimInstance Win32_PnPSignedDriver | ForEach-Object { $versions[[string]$_.DeviceID] = [string]$_.DriverVersion }
    @(Get-CimInstance Win32_PnPEntity | Where-Object { $_.Present -ne $false } | ForEach-Object {
        @{ Name = [string]$_.Name; Class = [string]$_.PNPClass; IDs = @(@($_.HardwareID) + @($_.CompatibleID) | Where-Object { $_ }); Version = [string]$versions[[string]$_.DeviceID] }
    })
}
function Get-EligibleDriver($u, $devices) {
    $allowed = @('AudioEndpoint','AudioProcessingObject','Bluetooth','Camera','Display','Extension','HDC','HIDClass','Image','Keyboard','MEDIA','Modem','Monitor','Mouse','Net','Printer','SCSIAdapter','SecurityDevices','Sensor','SmartCardReader','SoftwareComponent','Sound','System','USB','USBDevice')
    try {
        $hw = [string]$u.DriverHardwareID
        if (!$hw -or ![string]$u.DriverProvider -or $u.IsBeta) { return $null }
        if (([string]$u.Title) -match '(?i)firmware|bios|uefi|ファームウェア' -or $hw -match '(?i)uefi|firmware') { return $null }
        if (([string]$u.DriverClass) -match '(?i)firmware|bios|uefi') { return $null }
        $matching = @($devices | Where-Object { $_.IDs -contains $hw })
        if ($matching.Count -eq 0) { return $null }
        # WUA DriverClass is a SUS category (e.g. OtherHardware/Video), NOT an INF/PnP class.
        # Determine the device setup class from the exact matching local PnP devices instead.
        $classes = @($matching | ForEach-Object { [string]$_.Class } | Select-Object -Unique)
        if ($classes.Count -ne 1 -or $allowed -notcontains $classes[0]) { return $null }
        $class = [string]$classes[0]
        # WUA has already performed OS/OEM applicability and rank targeting. Never force a driver.
        # Installation uses IUpdateInstaller2.ForceQuiet; failure to run quietly is reported, never bypassed.
        [ordered]@{
            update_id = [string]$u.Identity.UpdateID; revision = [int]$u.Identity.RevisionNumber
            title = [string]$u.Title; class = $class; hardware_id = $hw
            provider = [string]$u.DriverProvider; model = [string]$u.DriverModel
            current_version = [string]$matching[0].Version
            optional = [bool]($u.BrowseOnly -or !$u.IsAssigned); hardware_match = $true
            eula = [string]$u.EulaText
        }
    } catch { return $null }
}
function New-DriverSearcher {
    $session = New-Object -ComObject Microsoft.Update.Session
    $session.ClientApplicationID = 'Application Pre-Installer'
    # Respect the configured update service and organizational policy.
    $session
}

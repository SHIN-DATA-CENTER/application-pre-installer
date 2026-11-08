try { Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe -ErrorAction Stop } catch { }
if (Get-Command winget.exe -ErrorAction SilentlyContinue) {
    & winget.exe --version
    if ($LASTEXITCODE -eq 0) { exit 0 }
}
# Official Microsoft bootstrap route. Install into the original user's profile, never AllUsers.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
if (!(Get-PackageProvider NuGet -ListAvailable -ErrorAction SilentlyContinue)) { $null = Install-PackageProvider NuGet -Scope CurrentUser -Force }
if (!(Get-Module -ListAvailable Microsoft.WinGet.Client)) {
    Install-Module Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser -Force -AllowClobber -Confirm:$false
}
Import-Module Microsoft.WinGet.Client -ErrorAction Stop
Repair-WinGetPackageManager -Latest -ErrorAction Stop
if (!(Get-Command winget.exe -ErrorAction SilentlyContinue)) { throw 'WinGetを準備できませんでした。Microsoft Storeの「アプリ インストーラー」を更新し、再ログインしてください。' }
& winget.exe --version
if ($LASTEXITCODE -ne 0) { throw 'WinGetの起動確認に失敗しました' }

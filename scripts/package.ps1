param([switch]$SkipBuild, [string]$ProjectRoot)
$ErrorActionPreference = 'Stop'
$root = if ($ProjectRoot) { (Resolve-Path -LiteralPath $ProjectRoot).Path } else { (Resolve-Path (Split-Path $PSScriptRoot -Parent)).Path }
Push-Location $root
try {
    if (!$SkipBuild) {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $binary = Join-Path $dist 'application-pre-installer.exe'
    Copy-Item -LiteralPath (Join-Path $root 'target/release/application-pre-installer.exe') -Destination $binary -Force
    $hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  application-pre-installer.exe" | Set-Content -LiteralPath (Join-Path $dist 'SHA256SUMS.txt') -Encoding ASCII
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $dist -Force
    Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $dist -Force
    Copy-Item -LiteralPath (Join-Path $root 'CHANGELOG.md') -Destination $dist -Force
    Copy-Item -LiteralPath (Join-Path $root 'docs') -Destination $dist -Recurse -Force
    $metadataText = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $metadata = $metadataText | ConvertFrom-Json
    $notice = New-Object System.Text.StringBuilder
    $null = $notice.AppendLine("# Third-party dependencies`n`nSlint runtime: LicenseRef-Slint-Royalty-free-2.0, with AboutSlint attribution in the application.`nBuild tools are listed for transparency. Each dependency retains its own copyright and license.`n")
    $licenseDir = Join-Path $dist 'licenses'
    New-Item -ItemType Directory -Path $licenseDir -Force | Out-Null
    $index = @()
    foreach ($package in $metadata.packages | Sort-Object name, version) {
        if (!$package.source) { continue }
        $null = $notice.AppendLine("## $($package.name) $($package.version)`n`nDeclared: $($package.license)`nRepository: $($package.repository)`n")
        $dir = Split-Path $package.manifest_path -Parent
        $files = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)' })
        foreach ($folder in @('LICENSES','licenses')) {
            $candidate = Join-Path $dir $folder
            if (Test-Path -LiteralPath $candidate -PathType Container) { $files += @(Get-ChildItem -LiteralPath $candidate -Recurse -File) ; break }
        }
        if ($package.license_file) {
            $specific = Join-Path $dir $package.license_file
            if (Test-Path -LiteralPath $specific -PathType Leaf) { $files += Get-Item -LiteralPath $specific }
        }
        $files = @($files | Sort-Object FullName -Unique)
        $destination = Join-Path $licenseDir "$($package.name)-$($package.version)"
        New-Item -ItemType Directory -Path $destination -Force | Out-Null
        foreach ($file in $files) {
            $relative = $file.FullName.Substring($dir.Length).TrimStart('\','/')
            $out = Join-Path $destination $relative
            New-Item -ItemType Directory -Path (Split-Path $out -Parent) -Force | Out-Null
            Copy-Item -LiteralPath $file.FullName -Destination $out -Force
        }
        if (!$files.Count) {
            # Retain the distributed crate source when its notices live in source headers.
            Copy-Item -LiteralPath $dir -Destination (Join-Path $destination 'crate-source') -Recurse -Force
            $null = $notice.AppendLine('No standalone license file in the crate archive; the original crate source is included for notices.')
        }
        $index += [pscustomobject]@{ name=$package.name; version=$package.version; license=$package.license; repository=$package.repository; notice_files=$files.Count }
    }
    $notice.ToString() | Set-Content -LiteralPath (Join-Path $dist 'THIRD_PARTY_NOTICES.md') -Encoding UTF8
    $index | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $dist 'dependency-licenses.json') -Encoding UTF8
    $items = @('application-pre-installer.exe','SHA256SUMS.txt','LICENSE','README.md','CHANGELOG.md','THIRD_PARTY_NOTICES.md','dependency-licenses.json','licenses','docs') | ForEach-Object { Join-Path $dist $_ }
    # Some published crate archives use Unix epoch timestamps, outside ZIP's DOS date range.
    # Normalize copies in dist only; never alter the registry source files.
    $zipEpoch = [DateTime]::SpecifyKind([DateTime]'1980-01-01', [DateTimeKind]::Utc)
    foreach ($artifactFile in Get-ChildItem -LiteralPath $dist -Recurse -File) {
        if ($artifactFile.LastWriteTimeUtc -lt $zipEpoch) { $artifactFile.LastWriteTimeUtc = $zipEpoch }
    }
    Compress-Archive -LiteralPath $items -DestinationPath (Join-Path $dist 'application-pre-installer-windows-x64.zip') -Force
    Write-Host "Packaged: $dist"
} finally { Pop-Location }

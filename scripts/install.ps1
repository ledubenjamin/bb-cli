# Install or update the Windows x86_64 release in the current user's bin directory.
[CmdletBinding()]
param(
    [string]$Version = $env:BB_VERSION,
    [string]$InstallDir = $env:BB_INSTALL_DIR,
    [switch]$NoPathUpdate
)

$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
    $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'This installer requires x86_64 Windows and 64-bit PowerShell.'
}
if (-not $InstallDir) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'bb\bin'
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$releases = 'https://github.com/ledubenjamin/bb-cli/releases'
if (-not $Version) {
    $Version = (Invoke-RestMethod "$releases/latest/download/VERSION").Trim()
}
if ($Version -cnotmatch '^v[0-9][0-9A-Za-z.+-]*$') {
    throw 'Expected a version tag such as v0.1.0.'
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$staged = $null
try {
    New-Item -ItemType Directory -Path $temporary | Out-Null
    $name = "bb-$($Version.Substring(1))-x86_64-pc-windows-msvc"
    $archive = "$name.zip"
    $archivePath = Join-Path $temporary $archive
    $checksumPath = Join-Path $temporary 'checksum'
    Invoke-WebRequest "$releases/download/$Version/$archive" -UseBasicParsing -OutFile $archivePath
    Invoke-WebRequest "$releases/download/$Version/$archive.sha256" -UseBasicParsing -OutFile $checksumPath
    $checksum = (Get-Content -LiteralPath $checksumPath -Raw).Trim() -split '\s+'
    if ($checksum.Count -ne 2 -or $checksum[1] -cne $archive -or
        (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash -ne $checksum[0]) {
        throw 'SHA-256 verification failed.'
    }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $temporary
    $binary = Join-Path $temporary "$name\bb.exe"
    $actualVersion = & $binary --version
    if ($LASTEXITCODE -ne 0 -or $actualVersion -cne "bb $($Version.Substring(1))") {
        throw 'The downloaded binary cannot run or has the wrong version.'
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $destination = Join-Path $InstallDir 'bb.exe'
    $staged = Join-Path $InstallDir ('.bb-' + [Guid]::NewGuid().ToString() + '.exe')
    Copy-Item -LiteralPath $binary -Destination $staged
    # Stage on the same filesystem; a locked executable leaves the old version intact.
    if (Test-Path -LiteralPath $destination) {
        # A typed null avoids PowerShell converting the backup path to an empty string.
        [IO.File]::Replace($staged, $destination, [NullString]::Value)
    } else {
        [IO.File]::Move($staged, $destination)
    }
    $staged = $null

    if (-not $NoPathUpdate) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ($InstallDir -notin ($userPath -split ';')) {
            [Environment]::SetEnvironmentVariable('Path', "$InstallDir;$userPath", 'User')
        }
        if ($InstallDir -notin ($env:Path -split ';')) {
            $env:Path = "$InstallDir;$env:Path"
        }
    }
    Write-Host "Installed $actualVersion to $destination"
} finally {
    if ($staged -and (Test-Path -LiteralPath $staged)) {
        Remove-Item -LiteralPath $staged -Force
    }
    Remove-Item -LiteralPath $temporary -Recurse -Force
}

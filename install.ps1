[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('install', 'update', 'uninstall', 'help')]
    [string] $Action = 'install'
)

$ErrorActionPreference = 'Stop'
$Repository = 'MinitorMHS/rustovan'
$ApiRoot = "https://api.github.com/repos/$Repository"
$InstallRoot = Join-Path $env:LOCALAPPDATA 'Programs\Rustovan'
$BinDirectory = Join-Path $InstallRoot 'bin'
$Executable = Join-Path $BinDirectory 'rustovan.exe'
$CommandShim = Join-Path $BinDirectory 'rustovan.cmd'
$MetadataFile = Join-Path $BinDirectory 'install.json'
$MarkerFile = Join-Path $BinDirectory '.rustovan-managed'

function Show-Help {
    @'
Rustovan per-user installer

Usage from PowerShell:
  .\install.ps1 [install|update|uninstall|help]

Install checks the latest stable GitHub release and installs or updates the
matching binary. Uninstall removes only Rustovan-managed files and the PATH
entry this installer added. Open a new terminal after installation, then run
`rustovan --help`.

For a private repository, authenticate with `gh auth login` or set
RUSTOVAN_GITHUB_TOKEN (or GH_TOKEN/GITHUB_TOKEN) to a token with release read
access. Do not put a token in this script or commit one to source control.
'@ | Write-Output
}

function Get-GitHubToken {
    $token = $env:RUSTOVAN_GITHUB_TOKEN
    if ([string]::IsNullOrWhiteSpace($token)) { $token = $env:GH_TOKEN }
    if ([string]::IsNullOrWhiteSpace($token)) { $token = $env:GITHUB_TOKEN }
    if ([string]::IsNullOrWhiteSpace($token) -and (Get-Command gh -ErrorAction SilentlyContinue)) {
        try { $token = (& gh auth token 2>$null | Out-String).Trim() } catch { $token = '' }
    }
    if (-not [string]::IsNullOrWhiteSpace($token) -and $token -notmatch '^[A-Za-z0-9_]+$') {
        throw 'GitHub token has unsupported characters; refusing to use it in an HTTP header.'
    }
    return $token
}

function Get-ApiHeaders([string] $Token, [string] $Accept = 'application/vnd.github+json') {
    $headers = @{
        Accept = $Accept
        'X-GitHub-Api-Version' = '2022-11-28'
        'User-Agent' = 'rustovan-release-installer'
    }
    if (-not [string]::IsNullOrWhiteSpace($Token)) { $headers.Authorization = "Bearer $Token" }
    return $headers
}

function Test-ReparsePoint([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    $item = Get-Item -LiteralPath $Path -Force
    return (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Get-Asset([object] $Release, [string] $Name) {
    return $Release.assets | Where-Object { $_.name -ceq $Name } | Select-Object -First 1
}

function Save-ReleaseAsset([long] $AssetId, [string] $Destination, [string] $Token, [string] $TempRoot) {
    $headers = Get-ApiHeaders $Token 'application/octet-stream'
    $uri = "$ApiRoot/releases/assets/$AssetId"
    $response = $null
    try {
        Invoke-WebRequest -Uri $uri -Headers $headers -OutFile $Destination -UseBasicParsing -MaximumRedirection 0 | Out-Null
        $response = $null
    }
    catch {
        $response = $_.Exception.Response
        if ($null -eq $response) { throw }
        $status = [int]$response.StatusCode
        if ($status -lt 300 -or $status -ge 400) { throw }
        $location = [string]$response.Headers['Location']
        if ([string]::IsNullOrWhiteSpace($location)) { throw 'GitHub asset redirect did not include a Location header.' }
        $redirectUri = [Uri]$location
        if ($redirectUri.Scheme -ne 'https') { throw 'GitHub returned a non-HTTPS asset redirect.' }
        # Deliberately omit GitHub authorization headers on the signed CDN URL.
        Invoke-WebRequest -Uri $redirectUri.AbsoluteUri -OutFile $Destination -UseBasicParsing -MaximumRedirection 5 | Out-Null
    }
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf) -or (Get-Item -LiteralPath $Destination).Length -le 0) {
        throw 'Downloaded release asset is empty.'
    }
}

function Ensure-UserPathEntry([string] $Directory) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $userPath) { $userPath = '' }
    $parts = @($userPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    foreach ($part in $parts) {
        if ($part.TrimEnd('\') -ieq $Directory.TrimEnd('\')) { return $false }
    }
    $newPath = (@($parts) + $Directory) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    return $true
}

function Remove-UserPathEntry([string] $Directory) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($userPath)) { return }
    $kept = @($userPath -split ';' | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_) -and $_.TrimEnd('\') -ine $Directory.TrimEnd('\')
    })
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}

if ($Action -eq 'help') { Show-Help; exit 0 }

if ($Action -eq 'uninstall') {
    $pathAddedByInstaller = $false
    $managed = (Test-Path -LiteralPath $MarkerFile -PathType Leaf) -and -not (Test-ReparsePoint $MarkerFile) -and
        ((Get-Content -LiteralPath $MarkerFile -Raw -ErrorAction SilentlyContinue).Trim() -eq 'rustovan-managed-v1')
    if ($managed -and (Test-Path -LiteralPath $MetadataFile -PathType Leaf)) {
        try { $oldMetadata = Get-Content -LiteralPath $MetadataFile -Raw | ConvertFrom-Json; $pathAddedByInstaller = [bool]$oldMetadata.pathAdded } catch { }
    }
    if ($pathAddedByInstaller) { Remove-UserPathEntry $BinDirectory }
    if ($managed) {
        foreach ($file in @($Executable, $CommandShim, $MetadataFile, $MarkerFile)) {
            if (Test-ReparsePoint $file) {
                Write-Warning "Leaving reparse point untouched: $file"
            } elseif (Test-Path -LiteralPath $file -PathType Leaf) {
                Remove-Item -LiteralPath $file -Force
            }
        }
        try { Remove-Item -LiteralPath $BinDirectory -Force } catch { Write-Host "Preserved non-empty directory: $BinDirectory" }
        try { Remove-Item -LiteralPath $InstallRoot -Force } catch { }
    } elseif (Test-Path -LiteralPath $BinDirectory) {
        Write-Warning "Install directory is not marked as Rustovan-managed; leaving it untouched: $BinDirectory"
    }
    Write-Host 'Rustovan uninstall finished. Open a new terminal to refresh PATH.'
    exit 0
}

if ($PSVersionTable.PSVersion.Major -lt 5) { throw 'PowerShell 5.1 or newer is required.' }
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set; cannot choose a per-user install location.' }
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$architecture = ''
try { $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() } catch { }
if ([string]::IsNullOrWhiteSpace($architecture)) {
    if ($env:PROCESSOR_ARCHITEW6432) { $architecture = $env:PROCESSOR_ARCHITEW6432 }
    else { $architecture = $env:PROCESSOR_ARCHITECTURE }
}
$assetId = switch -Regex ($architecture.ToUpperInvariant()) {
    '^(ARM64|AARCH64)$' { 'windows-aarch64'; break }
    '^(AMD64|X64|X86_64)$' { 'windows-x86_64'; break }
    default { throw "No Rustovan Windows release supports detected OS architecture '$architecture'." }
}

$token = Get-GitHubToken
$apiHeaders = Get-ApiHeaders $token
if ((Test-ReparsePoint $InstallRoot) -or (Test-ReparsePoint $BinDirectory)) {
    throw "Refusing to install through a reparse point: $BinDirectory"
}
if ((Test-Path -LiteralPath $CommandShim) -and -not (Test-Path -LiteralPath $CommandShim -PathType Leaf)) {
    throw "Refusing to replace a non-file command shim path: $CommandShim"
}
if (Test-ReparsePoint $CommandShim) { throw "Refusing to replace reparse point: $CommandShim" }
if ((Test-Path -LiteralPath $CommandShim -PathType Leaf) -and
    ((Get-Content -LiteralPath $CommandShim -TotalCount 2)[1] -ne 'rem managed by rustovan installer')) {
    throw "Refusing to replace unmanaged command shim: $CommandShim"
}
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("rustovan-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
try {
    try {
        $release = Invoke-RestMethod -Uri "$ApiRoot/releases/latest" -Headers $apiHeaders -Method Get
    } catch {
        throw "Could not read the latest GitHub release. The repository may be private or rate-limited. Authenticate with 'gh auth login' or set RUSTOVAN_GITHUB_TOKEN/GH_TOKEN/GITHUB_TOKEN. Details: $($_.Exception.Message)"
    }
    $tag = [string]$release.tag_name
    if ($tag -notmatch '^[A-Za-z0-9._-]+$') { throw "Unexpected GitHub release tag format: $tag" }

    $archiveName = "rustovan-$assetId.zip"
    $archiveAsset = Get-Asset $release $archiveName
    $archiveEntryName = 'rustovan.exe'
    if ($null -eq $archiveAsset) {
        $archiveName = "ton-vanity-rust-$assetId.zip"
        $archiveAsset = Get-Asset $release $archiveName
        $archiveEntryName = 'ton-vanity-rust.exe'
    }
    $checksumAsset = Get-Asset $release 'SHA256SUMS.txt'
    if ($null -eq $archiveAsset) { throw "Latest release $tag does not contain $archiveName." }
    if ($null -eq $checksumAsset) { throw "Latest release $tag has no SHA256SUMS.txt; refusing an unverifiable download." }

    $checksumPath = Join-Path $tempRoot 'SHA256SUMS.txt'
    Save-ReleaseAsset ([long]$checksumAsset.id) $checksumPath $token $tempRoot
    $expectedHash = $null
    foreach ($line in Get-Content -LiteralPath $checksumPath) {
        if ($line -match '^(?<hash>[0-9a-fA-F]{64})\s+\*?(?<name>.+)$' -and $Matches.name -ceq $archiveName) {
            $expectedHash = $Matches.hash.ToLowerInvariant()
            break
        }
    }
    if ($expectedHash -notmatch '^[0-9a-f]{64}$') { throw "No valid SHA-256 entry for $archiveName in the release manifest." }

    $oldMetadata = $null
    if ((Test-Path -LiteralPath $MarkerFile -PathType Leaf) -and (Test-Path -LiteralPath $MetadataFile -PathType Leaf)) {
        try { $oldMetadata = Get-Content -LiteralPath $MetadataFile -Raw | ConvertFrom-Json } catch { $oldMetadata = $null }
    }
    $isCurrent = $false
    $markerMatches = (Test-Path -LiteralPath $MarkerFile -PathType Leaf) -and -not (Test-ReparsePoint $MarkerFile) -and
        ((Get-Content -LiteralPath $MarkerFile -Raw -ErrorAction SilentlyContinue).Trim() -eq 'rustovan-managed-v1')
    if ($null -ne $oldMetadata -and $markerMatches -and $oldMetadata.marker -eq 'rustovan-managed-v1' -and
        $oldMetadata.tag -eq $tag -and $oldMetadata.asset -eq $assetId -and
        $oldMetadata.packageSha256 -eq $expectedHash -and
        (Test-Path -LiteralPath $Executable -PathType Leaf) -and -not (Test-ReparsePoint $Executable)) {
        $actualBinaryHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
        $isCurrent = ($oldMetadata.binarySha256 -eq $actualBinaryHash)
    }

    if ($isCurrent) {
        Write-Host "Rustovan $tag ($assetId) is already installed and verified; no binary download needed."
        $pathAdded = [bool]$oldMetadata.pathAdded
    } else {
        $archivePath = Join-Path $tempRoot $archiveName
        Save-ReleaseAsset ([long]$archiveAsset.id) $archivePath $token $tempRoot
        $actualPackageHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actualPackageHash -ne $expectedHash) { throw 'Release archive SHA-256 does not match SHA256SUMS.txt; refusing installation.' }

        if ((Test-ReparsePoint $InstallRoot) -or (Test-ReparsePoint $BinDirectory)) { throw "Refusing to install through a reparse point: $BinDirectory" }
        if ((Test-Path -LiteralPath $BinDirectory) -and -not (Test-Path -LiteralPath $MarkerFile -PathType Leaf)) {
            throw "Install directory exists but is not managed by Rustovan: $BinDirectory"
        }
        New-Item -ItemType Directory -Force -Path $BinDirectory | Out-Null
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -cne $archiveEntryName -or $zip.Entries[0].Length -le 0) {
                throw "Windows release archive must contain exactly one non-empty file named $archiveEntryName."
            }
            $stagingExe = Join-Path $tempRoot 'rustovan.exe'
            $source = $zip.Entries[0].Open()
            $destination = [IO.File]::Open($stagingExe, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $source.CopyTo($destination) } finally { $destination.Dispose(); $source.Dispose() }
        } finally { $zip.Dispose() }

        foreach ($file in @($Executable, $CommandShim, $MetadataFile, $MarkerFile)) {
            if (Test-ReparsePoint $file) { throw "Refusing to replace reparse point: $file" }
            if ((Test-Path -LiteralPath $file) -and -not (Test-Path -LiteralPath $file -PathType Leaf)) {
                throw "Refusing to replace a non-file install path: $file"
            }
        }
        Move-Item -LiteralPath $stagingExe -Destination $Executable -Force
        $binaryHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
        Set-Content -LiteralPath $MarkerFile -Value 'rustovan-managed-v1' -NoNewline -Encoding ASCII

        $pathAddedBefore = $false
        if ($null -ne $oldMetadata -and $oldMetadata.marker -eq 'rustovan-managed-v1') { $pathAddedBefore = [bool]$oldMetadata.pathAdded }
        $pathAddedNow = Ensure-UserPathEntry $BinDirectory
        $pathAdded = $pathAddedBefore -or $pathAddedNow
        $newMetadata = [ordered]@{
            marker = 'rustovan-managed-v1'
            tag = $tag
            asset = $assetId
            packageSha256 = $expectedHash
            binarySha256 = $binaryHash
            pathAdded = [bool]$pathAdded
        }
        $metadataTemp = Join-Path $BinDirectory ("install.json." + [Guid]::NewGuid().ToString('N') + '.tmp')
        if (Test-ReparsePoint $metadataTemp) { throw "Refusing to write through reparse point: $metadataTemp" }
        $newMetadata | ConvertTo-Json | Set-Content -LiteralPath $metadataTemp -Encoding ASCII
        Move-Item -LiteralPath $metadataTemp -Destination $MetadataFile -Force

        $shimContent = "@echo off`r`nrem managed by rustovan installer`r`n`"%~dp0rustovan.exe`" %*`r`nexit /b %ERRORLEVEL%`r`n"
        Set-Content -LiteralPath $CommandShim -Value $shimContent -Encoding ASCII
        Write-Host "Installed Rustovan $tag for $assetId."
    }

    if ($isCurrent) {
        $pathAddedNow = Ensure-UserPathEntry $BinDirectory
        if ($pathAddedNow) {
            $pathAdded = $true
            $oldMetadata.pathAdded = $true
            $metadataTemp = Join-Path $BinDirectory ("install.json." + [Guid]::NewGuid().ToString('N') + '.tmp')
            $oldMetadata | ConvertTo-Json | Set-Content -LiteralPath $metadataTemp -Encoding ASCII
            Move-Item -LiteralPath $metadataTemp -Destination $MetadataFile -Force
        }
    }
    if (Test-ReparsePoint $CommandShim) { throw "Refusing to write through reparse point: $CommandShim" }
    if ((Test-Path -LiteralPath $CommandShim) -and -not (Test-Path -LiteralPath $CommandShim -PathType Leaf)) {
        throw "Refusing to replace a non-file command shim path: $CommandShim"
    }
    if ((Test-Path -LiteralPath $CommandShim -PathType Leaf) -and
        ((Get-Content -LiteralPath $CommandShim -TotalCount 2)[1] -ne 'rem managed by rustovan installer')) {
        throw "Refusing to replace unmanaged command shim: $CommandShim"
    }
    $shimContent = "@echo off`r`nrem managed by rustovan installer`r`n`"%~dp0rustovan.exe`" %*`r`nexit /b %ERRORLEVEL%`r`n"
    Set-Content -LiteralPath $CommandShim -Value $shimContent -Encoding ASCII
    Write-Host "Global command installed: $CommandShim"
    Write-Host 'Open a new terminal, then run: rustovan --help'
    Write-Host 'To update later: .\install.ps1 update'
    Write-Host 'To uninstall: .\install.ps1 uninstall'
} finally {
    if (Test-Path -LiteralPath $tempRoot) { Remove-Item -LiteralPath $tempRoot -Recurse -Force }
}

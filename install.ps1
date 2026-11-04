# Install prk on Windows (prebuilt binary, no Rust needed).
#   PowerShell:  irm https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.ps1 | iex
#   cmd:         powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.ps1 | iex"
# Change the install dir with $env:PRK_INSTALL_DIR, pin a version with $env:PRK_VERSION = 'v0.1.2'

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # the progress bar makes downloads much slower in PowerShell 5
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

& {
    $repository = 'pankajyadav-blitz/prk'
    $installDir = if ($env:PRK_INSTALL_DIR) { $env:PRK_INSTALL_DIR } else { Join-Path $HOME '.local\bin' }
    $baseUrl = if ($env:PRK_BASE_URL) { $env:PRK_BASE_URL.TrimEnd('/') }
               elseif ($env:PRK_VERSION) { "https://github.com/$repository/releases/download/$($env:PRK_VERSION)" }
               else { "https://github.com/$repository/releases/latest/download" }

    # ARM Windows runs the x64 build.
    $archiveName = 'prk-x86_64-pc-windows-msvc.zip'
    $temporaryDir = Join-Path ([IO.Path]::GetTempPath()) ("prk-install-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $temporaryDir | Out-Null
    try {
        $version = try { (Invoke-RestMethod -UseBasicParsing "$baseUrl/VERSION").ToString().Trim() } catch { 'unknown' }
        Write-Host "prk: downloading $archiveName ($version)"
        $archive = Join-Path $temporaryDir $archiveName
        Invoke-WebRequest -UseBasicParsing "$baseUrl/$archiveName" -OutFile $archive
        Invoke-WebRequest -UseBasicParsing "$baseUrl/$archiveName.sha256" -OutFile "$archive.sha256"

        $expected = ((Get-Content -Raw "$archive.sha256").Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLower()
        if ($expected -ne $actual) { throw "prk: checksum mismatch, aborting" }

        Expand-Archive -Force -LiteralPath $archive -DestinationPath $temporaryDir
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        Copy-Item -Force (Join-Path $temporaryDir 'prk.exe') (Join-Path $installDir 'prk.exe')
    } finally {
        Remove-Item -Recurse -Force $temporaryDir -ErrorAction SilentlyContinue
    }
    Write-Host "prk: installed to $(Join-Path $installDir 'prk.exe')"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $installDir)) {
        [Environment]::SetEnvironmentVariable('Path', ((@($userPath, $installDir) | Where-Object { $_ }) -join ';'), 'User')
        Write-Host "prk: added $installDir to your PATH (new windows pick it up)"
    }
    if (-not (($env:Path -split ';') -contains $installDir)) { $env:Path = "$env:Path;$installDir" }
    Write-Host "prk: next, run 'prk init' to set it up; update later with 'prk update'"
}

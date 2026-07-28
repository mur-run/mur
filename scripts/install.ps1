# install.ps1 — one-liner Windows installer for mur CLI.
# Usage:
#   irm https://mur.run/install.ps1 | iex
#   $env:MUR_VERSION = "2.16.1"; irm https://mur.run/install.ps1 | iex
# Optional env: MUR_INSTALL_DIR (defaults to $HOME\.local\bin)

$ErrorActionPreference = 'Stop'

$GhOwner = 'mur-run'
$GhRepo  = 'mur'

# --- Detect arch ---
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
    throw "Windows on $arch is not yet supported. Install from source: cargo install mur"
}
$asset = 'mur-x86_64-pc-windows-msvc.zip'

# --- Resolve version ---
$version = $env:MUR_VERSION
if (-not $version) {
    Write-Host 'Resolving latest mur release...'
    $latest = Invoke-RestMethod -Headers @{ 'User-Agent' = 'mur-install' } `
        -Uri "https://api.github.com/repos/$GhOwner/$GhRepo/releases/latest"
    $version = $latest.tag_name -replace '^v', ''
}

$relUrl       = "https://github.com/$GhOwner/$GhRepo/releases/download/v$version"
$assetUrl     = "$relUrl/$asset"
$checksumsUrl = "$relUrl/checksums.txt"

# --- Pick install dir ---
$installDir = $env:MUR_INSTALL_DIR
if (-not $installDir) {
    $installDir = Join-Path $HOME '.local\bin'
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

# --- Download + verify ---
$tmp = New-Item -ItemType Directory -Force `
    -Path (Join-Path ([IO.Path]::GetTempPath()) ("mur-install-" + [Guid]::NewGuid()))
try {
    Write-Host "Downloading $asset (v$version)..."
    Invoke-WebRequest -Uri $assetUrl     -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -Uri $checksumsUrl -OutFile (Join-Path $tmp 'checksums.txt')

    $expectedLine = (Get-Content (Join-Path $tmp 'checksums.txt')) |
        Where-Object { $_ -match "[ *]$([regex]::Escape($asset))$" }
    if (-not $expectedLine) { throw "No checksum entry for $asset" }
    $expected = ($expectedLine -split '\s+')[0].ToLower()

    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($expected -ne $actual) { throw 'Checksum verification FAILED. Aborting.' }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter 'mur.exe' | Select-Object -First 1
    if (-not $exe) { throw 'Archive did not contain mur.exe' }
    # Keep this list in sync with the Homebrew formula's `install` block.
    # Optional entries are skipped when an older release did not ship them.
    foreach ($name in 'mur', 'mur-mcp-server', 'murmurd', 'mur-agent-runtime') {
        $found = Get-ChildItem -Path $tmp -Recurse -Filter "$name.exe" | Select-Object -First 1
        if ($found) { Move-Item -Force -Path $found.FullName -Destination (Join-Path $installDir "$name.exe") }
    }
    # `murmur` is argv[0] shorthand for `mur agent cli` (BusyBox convention).
    # ponytail: copy, not a symlink — those need Developer Mode or admin on Windows.
    Copy-Item -Force -Path (Join-Path $installDir 'mur.exe') -Destination (Join-Path $installDir 'murmur.exe')
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# --- PATH check ---
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($userPath -split ';' | Where-Object { $_ -eq $installDir })) {
    Write-Host "Adding $installDir to your User PATH..."
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
    Write-Host 'Open a new terminal for PATH changes to take effect.'
}

Write-Host ''
Write-Host "mur v$version installed to $installDir\mur.exe"
Write-Host "Run 'mur init' to get started."

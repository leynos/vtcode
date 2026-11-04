# Diapason Native Installer (Windows PowerShell)
# Downloads and installs the latest Diapason binary from GitHub Releases

param(
    [string]$InstallDir = "$env:USERPROFILE\.local\bin",
    [string]$BinaryPath = "",
    [switch]$SourceBuild,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

# Configuration
$Repo = "leynos/vtcode"
$BinName = "diapason.exe"
$GitHubAPI = "https://api.github.com/repos/$Repo/releases?per_page=10"
$GitHubReleases = "https://github.com/$Repo/releases/download"

# Logging functions
function Write-Info { Write-Host "INFO: $args" -ForegroundColor Cyan }
function Write-Success { Write-Host "✓ $args" -ForegroundColor Green }
function Write-Error { Write-Host "✗ $args" -ForegroundColor Red }
function Write-Warning { Write-Host "⚠ $args" -ForegroundColor Yellow }

# Show help
function Show-Help {
    Write-Host @"
Diapason Native Installer (Windows PowerShell)

Usage: .\install.ps1 [options]

Options:
  -InstallDir <path>   Installation directory (default: $env:USERPROFILE\.local\bin)
  -SourceBuild         Build from the Diapason source repository with Cargo
  -BinaryPath <path>   Install an explicitly selected local Diapason binary
  -Help                Show this help message

Examples:
  .\install.ps1                                         # Install to user directory
  .\install.ps1 -InstallDir "C:\Program Files\diapason" # Install to Program Files

Note: Installing to Program Files may require administrator privileges.
"@
    exit 0
}

if ($Help) {
    Show-Help
}

# Ensure installation directory exists
if (-not (Test-Path $InstallDir)) {
    Write-Info "Creating installation directory: $InstallDir"
    New-Item -ItemType Directory -Path $InstallDir -Force > $null
}

# Fetch recent releases from GitHub API
function Get-RecentReleases {
    Write-Info "Fetching recent Diapason releases from GitHub..."
    
    try {
        $response = Invoke-RestMethod -Uri $GitHubAPI -ErrorAction Stop
        return $response
    }
    catch {
        Write-Error "Failed to fetch release info from GitHub API"
        Write-Error "Error: $_"
        exit 1
    }
}

function Get-DownloadFilename {
    param([string]$ReleaseTag)

    $VersionTag = $ReleaseTag
    if ($VersionTag.StartsWith("v")) {
        $VersionTag = $VersionTag.Substring(1)
    }
    return "diapason-$VersionTag-x86_64-pc-windows-msvc.zip"
}

function Test-AssetAvailable {
    param([string]$Url)

    try {
        Invoke-WebRequest -Uri $Url -Method Head -ErrorAction Stop > $null
        return $true
    }
    catch {
        return $false
    }
}

# Select latest release that actually has Windows asset
function Get-DownloadUrl {
    param([array]$Releases)

    foreach ($Release in $Releases) {
        $ReleaseTag = $Release.tag_name
        if ([string]::IsNullOrEmpty($ReleaseTag)) {
            continue
        }

        $Filename = Get-DownloadFilename -ReleaseTag $ReleaseTag
        $DownloadUrl = "$GitHubReleases/$ReleaseTag/$Filename"
        if (Test-AssetAvailable -Url $DownloadUrl) {
            Write-Info "Found compatible version: $ReleaseTag"
            return @{
                Url = $DownloadUrl
                Tag = $ReleaseTag
                Filename = $Filename
            }
        }
    }

    Write-Error "No recent releases include a Windows native binary asset"
    Write-Info "Use -SourceBuild to build from leynos/vtcode or -BinaryPath for a local binary"
    exit 1
}

# Download binary with progress
function Download-Binary {
    param(
        [string]$Url,
        [string]$OutputPath
    )
    
    Write-Info "Downloading binary from GitHub..."
    Write-Info "URL: $Url"
    
    try {
        # Use ProgressPreference to show download progress
        $ProgressPreference = 'Continue'
        Invoke-WebRequest -Uri $Url -OutFile $OutputPath -ErrorAction Stop
        Write-Success "Downloaded successfully"
    }
    catch {
        Write-Error "Failed to download binary"
        Write-Error "Error: $_"
        exit 1
    }
}

# Verify checksum if available
function Verify-Checksum {
    param(
        [string]$BinaryPath,
        [string]$ReleaseTag,
        [string]$ReleaseFilename
    )
    
    Write-Info "Verifying binary integrity..."
    
    $Filename = if ([string]::IsNullOrEmpty($ReleaseFilename)) {
        (Get-Item $BinaryPath).Name
    } else {
        $ReleaseFilename
    }
    $TempChecksumFile = Join-Path $env:TEMP "diapason-checksums.txt"
    $ExpectedChecksum = ""

    # Try to download checksums.txt first
    $ChecksumsUrl = "$GitHubReleases/$ReleaseTag/checksums.txt"
    try {
        Invoke-WebRequest -Uri $ChecksumsUrl -OutFile $TempChecksumFile -ErrorAction SilentlyContinue
        if (Test-Path $TempChecksumFile) {
            $ExpectedChecksum = (Get-Content $TempChecksumFile | Select-String $Filename | ForEach-Object { $_ -split '\s+' } | Select-Object -First 1).Trim()
        }
    }
    catch {
        # Continue to try individual file
    }

    # If not found in checksums.txt, try individual .sha256 file
    if ([string]::IsNullOrEmpty($ExpectedChecksum)) {
        $ShaUrl = "$GitHubReleases/$ReleaseTag/$Filename.sha256"
        try {
            Invoke-WebRequest -Uri $ShaUrl -OutFile $TempChecksumFile -ErrorAction SilentlyContinue
            if (Test-Path $TempChecksumFile) {
                $ExpectedChecksum = (Get-Content $TempChecksumFile | Select-Object -First 1).Trim()
            }
        }
        catch {
            # Both failed
        }
    }
    
    if ([string]::IsNullOrEmpty($ExpectedChecksum)) {
        Write-Warning "Checksum not found for $Filename, skipping verification"
        if (Test-Path $TempChecksumFile) { Remove-Item -Path $TempChecksumFile -Force }
        return
    }
    
    # Compute actual checksum
    $ActualChecksum = (Get-FileHash -Path $BinaryPath -Algorithm SHA256).Hash
    
    # PowerShell Get-FileHash returns uppercase, while shasum usually returns lowercase
    if ($ActualChecksum.ToLower() -ne $ExpectedChecksum.ToLower()) {
        Write-Error "Checksum mismatch for $Filename!"
        Write-Error "Expected: $ExpectedChecksum"
        Write-Error "Got:      $ActualChecksum"
        if (Test-Path $TempChecksumFile) { Remove-Item -Path $TempChecksumFile -Force }
        exit 1
    }
    
    Write-Success "Checksum verified: $ExpectedChecksum"
    
    # Clean up temporary file
    if (Test-Path $TempChecksumFile) { Remove-Item -Path $TempChecksumFile -Force }
}

# Extract binary from zip
function Extract-Binary {
    param(
        [string]$ZipPath,
        [string]$OutputDir
    )
    
    Write-Info "Extracting binary..."
    
    try {
        Expand-Archive -Path $ZipPath -DestinationPath $OutputDir -Force -ErrorAction Stop
    }
    catch {
        Write-Error "Failed to extract binary"
        Write-Error "Error: $_"
        exit 1
    }
    
    # Find the binary
    $BinaryPath = Get-ChildItem -Path $OutputDir -Name "$BinName" -Recurse | Select-Object -First 1
    
    if ($null -eq $BinaryPath) {
        Write-Error "Binary not found in archive"
        exit 1
    }
    
    return Join-Path $OutputDir $BinaryPath
}

# Install binary
function Install-Binary {
    param(
        [string]$Source,
        [string]$Target
    )
    
    Write-Info "Installing to $Target..."
    
    try {
        Copy-Item -LiteralPath $Source -Destination $Target -Force -ErrorAction Stop
        Write-Success "Binary installed to $Target"
    }
    catch {
        Write-Error "Failed to install binary to $Target"
        Write-Error "Error: $_"
        Write-Info "You may need to run PowerShell as Administrator"
        exit 1
    }
}

function Install-SourceBinary {
    param(
        [string]$TargetPath,
        [string]$CargoRoot
    )

    $Cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $Cargo) {
        throw "Cargo is required for a source build"
    }

    $SourceRef = if ([string]::IsNullOrWhiteSpace($env:DIAPASON_SOURCE_REF)) { "main" } else { $env:DIAPASON_SOURCE_REF }
    Write-Info "Building Diapason from leynos/vtcode ($SourceRef)..."
    & cargo install --git "https://github.com/$Repo.git" --branch $SourceRef --locked --root $CargoRoot diapason
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo source installation failed with exit code $LASTEXITCODE"
    }

    $BuiltBinary = Join-Path (Join-Path $CargoRoot "bin") $BinName
    if (-not (Test-Path -LiteralPath $BuiltBinary -PathType Leaf)) {
        throw "Cargo did not produce the expected binary at $BuiltBinary"
    }

    Install-Binary -Source $BuiltBinary -Target $TargetPath
}

# Check if installation directory is in PATH
function Check-Path {
    param([string]$InstallPath)
    
    $PathDirs = $env:PATH -split ';'
    return $PathDirs -contains $InstallPath
}

# Add installation directory to PATH
function Add-ToPath {
    param([string]$InstallPath)
    
    Write-Warning "Installation directory is not in PATH"
    Write-Info "Add the following to your user environment variables:"
    Write-Host ""
    Write-Host "  Path: $InstallPath"
    Write-Host ""
    Write-Info "To add it manually:"
    Write-Host '  1. Press Win+X, select "System"'
    Write-Host '  2. Click "Advanced system settings"'
    Write-Host '  3. Click "Environment Variables"'
    Write-Host '  4. Under "User variables", select "Path" and click "Edit"'
    Write-Host "  5. Click "New" and add: $InstallPath"
    Write-Host ""
}

# Main installation flow
function Main {
    Write-Host "Diapason Native Installer (Windows)" -ForegroundColor Magenta
    Write-Host ""
    
    # Create temporary directory for downloads
    $TempDir = Join-Path $env:TEMP ("diapason-install-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $TempDir > $null
    
    try {
        $TargetPath = Join-Path $InstallDir $BinName

        if (-not [string]::IsNullOrWhiteSpace($BinaryPath)) {
            if (-not (Test-Path -Path $BinaryPath -PathType Leaf)) {
                throw "Local Diapason binary not found: $BinaryPath"
            }
            Install-Binary -Source $BinaryPath -Target $TargetPath
        }
        elseif ($SourceBuild) {
            $CargoRoot = Join-Path $TempDir "cargo"
            Install-SourceBinary -TargetPath $TargetPath -CargoRoot $CargoRoot
        }
        else {
            # Release matching accepts only Diapason-named binaries. Existing
            # VTCode release assets cannot be selected by this installer.
            $Releases = Get-RecentReleases
            $DownloadInfo = Get-DownloadUrl $Releases
            $ArchivePath = Join-Path $TempDir "diapason-binary.zip"
            Download-Binary -Url $DownloadInfo.Url -OutputPath $ArchivePath
            Verify-Checksum -BinaryPath $ArchivePath -ReleaseTag $DownloadInfo.Tag -ReleaseFilename $DownloadInfo.Filename
            $ExtractDir = Join-Path $TempDir "extract"
            New-Item -ItemType Directory -Path $ExtractDir -Force > $null
            $DownloadedBinary = Extract-Binary -ZipPath $ArchivePath -OutputDir $ExtractDir
            Install-Binary -Source $DownloadedBinary -Target $TargetPath
        }

        # Check if in PATH
        if (-not (Check-Path $InstallDir)) {
            Add-ToPath $InstallDir
        }
        
        Write-Host ""
        Write-Success "Installation complete!"
        Write-Info "Diapason is ready to use"
        Write-Host ""
        
        # Test installation
        try {
            $Version = & $TargetPath --version
            Write-Success "Version check passed: $Version"
        }
        catch {
            Write-Warning "Could not verify installation, but binary appears to be installed"
        }
        
        Write-Host ""
        Write-Info "To get started, run: diapason ask 'hello world'"
    }
    finally {
        # Cleanup
        Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main

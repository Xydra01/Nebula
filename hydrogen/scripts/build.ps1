# hydrogen/scripts/build.ps1
# Automates MSVC environment initialization and CMake/Ninja compilation for Project Hydrogen.

param(
    [string] $Config = 'Release',
    [switch] $Run,
    [switch] $Clean
)

$ErrorActionPreference = 'Stop'

# Determine directory paths
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$hydrogenDir = Split-Path -Parent $scriptDir
$buildDir = Join-Path $hydrogenDir "build"

# Clean if requested
if ($Clean -and (Test-Path $buildDir)) {
    Write-Host "Cleaning build directory: $buildDir" -ForegroundColor Cyan
    Remove-Item -Path $buildDir -Recurse -Force
}

# 1. Discover MSVC Build Tools via vswhere.exe
$vswherePaths = @(
    "F:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe",
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe",
    "${env:ProgramFiles}\Microsoft Visual Studio\Installer\vswhere.exe"
)

$vswhereExe = $null
foreach ($path in $vswherePaths) {
    if (Test-Path $path) {
        $vswhereExe = $path
        break
    }
}

if (-not $vswhereExe) {
    throw "vswhere.exe not found in standard locations."
}

$vsInstallPath = & $vswhereExe -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsInstallPath) {
    throw "Visual Studio C++ Build Tools not found via vswhere."
}

$vcvarsBat = Join-Path $vsInstallPath "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvarsBat)) {
    throw "vcvars64.bat not found at: $vcvarsBat"
}

# 2. Extract and import environment variables from vcvars64.bat
Write-Host "Initializing MSVC x64 environment from: $vcvarsBat" -ForegroundColor Cyan
$cmdOutput = & cmd /c "call `"$vcvarsBat`" >nul && set"
foreach ($line in $cmdOutput) {
    if ($line -match '^([^=]+)=(.*)$') {
        $varName = $matches[1]
        $varValue = $matches[2]
        [Environment]::SetEnvironmentVariable($varName, $varValue, 'Process')
    }
}

# 3. Verify Ninja and CMake
$cmakeCmd = Get-Command cmake -ErrorAction SilentlyContinue
if (-not $cmakeCmd) {
    throw "cmake executable not found in PATH."
}

$ninjaCmd = Get-Command ninja -ErrorAction SilentlyContinue
if (-not $ninjaCmd) {
    throw "ninja executable not found in PATH."
}

# 4. Configure CMake with Ninja
Write-Host "Configuring Project Hydrogen ($Config) with CMake and Ninja..." -ForegroundColor Cyan
& cmake -G Ninja -B $buildDir -S $hydrogenDir "-DCMAKE_BUILD_TYPE=$Config"
if ($LASTEXITCODE -ne 0) {
    throw "CMake configuration failed with exit code $LASTEXITCODE."
}

# 5. Build targets
Write-Host "Building targets in $Config mode..." -ForegroundColor Cyan
& cmake --build $buildDir --config $Config
if ($LASTEXITCODE -ne 0) {
    throw "CMake build failed with exit code $LASTEXITCODE."
}

Write-Host "Build completed successfully." -ForegroundColor Green

# 6. Execute sanity check if requested
if ($Run) {
    $sanityExe = Join-Path $buildDir "bin\hydrogen_sanity_test.exe"
    if (-not (Test-Path $sanityExe)) {
        throw "Sanity test binary not found at: $sanityExe"
    }

    Write-Host "`nExecuting Hydrogen Sanity Test: $sanityExe" -ForegroundColor Cyan
    & $sanityExe
    $testExitCode = $LASTEXITCODE
    if ($testExitCode -ne 0) {
        throw "hydrogen_sanity_test failed with exit code $testExitCode."
    }
}

[CmdletBinding()]
param(
    [string]$Version = "0.0.1",
    [switch]$Publish,
    [string]$RustToolchain = "1.95.0"
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $repoRoot

$registry = "https://registry.npmjs.org"
$target = "x86_64-pc-windows-msvc"
$platformTag = "win32-x64"
$rustRoot = Join-Path $repoRoot "codex-rs"
$buildScript = Join-Path $repoRoot "codex-cli/scripts/build_npm_package.py"

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host ("+ {0} {1}" -f $Command, ($Arguments -join " "))
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $Command"
    }
}

function Get-NpmUser {
    $output = & npm whoami --registry=$registry 2>&1
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "npm is not authenticated. Run 'npm login --registry=$registry' first."
    }

    $npmUser = ($output | Select-Object -First 1).ToString().Trim().ToLowerInvariant()
    if ($npmUser -notmatch "^[a-z0-9][a-z0-9._~-]*$") {
        throw "npm whoami returned an invalid scope name: $npmUser"
    }
    return $npmUser
}

function Read-TarPackageJson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Tarball
    )

    $manifestOutput = & tar -xOf $Tarball package/package.json 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to read package/package.json from $Tarball"
    }

    try {
        return (($manifestOutput -join [Environment]::NewLine) | ConvertFrom-Json)
    } catch {
        throw "Invalid package.json in $Tarball`: $($_.Exception.Message)"
    }
}

if ($Version -notmatch "^\d+\.\d+\.\d+$") {
    throw "This personal release script requires a stable semver such as 0.0.1."
}

$npmUser = Get-NpmUser
$packageName = "@$npmUser/codex"
$platformAlias = "$packageName-$platformTag"
$outputRoot = Join-Path $repoRoot "dist/personal-npm/$Version"

if (Test-Path -LiteralPath $outputRoot) {
    $existingFiles = Get-ChildItem -LiteralPath $outputRoot -Recurse -File -Force
    if ($existingFiles) {
        throw "Output directory contains files; choose a new version or remove it manually: $outputRoot"
    }
} else {
    New-Item -ItemType Directory -Path $outputRoot | Out-Null
}

$vendorRoot = Join-Path $outputRoot "vendor"
$vendorTargetRoot = Join-Path $vendorRoot $target
$vendorBinRoot = Join-Path $vendorTargetRoot "bin"
$platformStage = Join-Path $outputRoot "stage-win32-x64"
$rootStage = Join-Path $outputRoot "stage-root"
$platformTarball = Join-Path $outputRoot "codex-win32-x64-$Version.tgz"
$rootTarball = Join-Path $outputRoot "codex-$Version.tgz"
$releaseBinRoot = Join-Path $rustRoot "target/$target/release"

New-Item -ItemType Directory -Path $vendorBinRoot, $platformStage, $rootStage -Force | Out-Null

# Windows x64 的辅助程序和主程序必须一起放入 vendor，否则 CLI 能安装但沙箱和 code mode 会在运行时缺文件。
$rustupCargoArguments = @("run", $RustToolchain, "cargo")
Invoke-CheckedCommand "rustup" ($rustupCargoArguments + @("--version"))
$previousSqliteFlags = $env:LIBSQLITE3_FLAGS
$env:LIBSQLITE3_FLAGS = "SQLITE_DISABLE_INTRINSIC"
try {
    Invoke-CheckedCommand "rustup" ($rustupCargoArguments + @(
        "build",
        "--manifest-path",
        (Join-Path $rustRoot "Cargo.toml"),
        "--release",
        "--target",
        $target,
        "--bin",
        "codex",
        "--bin",
        "codex-code-mode-host",
        "--bin",
        "codex-command-runner",
        "--bin",
        "codex-windows-sandbox-setup"
    ))
} finally {
    if ($null -eq $previousSqliteFlags) {
        Remove-Item Env:LIBSQLITE3_FLAGS -ErrorAction SilentlyContinue
    } else {
        $env:LIBSQLITE3_FLAGS = $previousSqliteFlags
    }
}

$binaryNames = @(
    "codex.exe",
    "codex-code-mode-host.exe",
    "codex-command-runner.exe",
    "codex-windows-sandbox-setup.exe"
)
foreach ($binaryName in $binaryNames) {
    $sourceBinary = Join-Path $releaseBinRoot $binaryName
    if (-not (Test-Path -LiteralPath $sourceBinary)) {
        throw "Cargo build did not produce required binary: $sourceBinary"
    }
    Copy-Item -LiteralPath $sourceBinary -Destination (Join-Path $vendorBinRoot $binaryName)
}

Invoke-CheckedCommand "python" @(
    $buildScript,
    "--package",
    "codex-win32-x64",
    "--release-version",
    $Version,
    "--npm-package-name",
    $packageName,
    "--staging-dir",
    $platformStage,
    "--vendor-src",
    $vendorRoot,
    "--pack-output",
    $platformTarball
)

Invoke-CheckedCommand "python" @(
    $buildScript,
    "--package",
    "codex",
    "--release-version",
    $Version,
    "--npm-package-name",
    $packageName,
    "--platform",
    $platformTag,
    "--staging-dir",
    $rootStage,
    "--pack-output",
    $rootTarball
)

$platformManifest = Read-TarPackageJson $platformTarball
$rootManifest = Read-TarPackageJson $rootTarball
$platformVersion = "$Version-$platformTag"
$aliasProperty = $rootManifest.optionalDependencies.PSObject.Properties[$platformAlias]

if ($platformManifest.name -ne $packageName -or $platformManifest.version -ne $platformVersion) {
    throw "Platform package manifest does not match $packageName@$platformVersion"
}
if (($platformManifest.os -notcontains "win32") -or ($platformManifest.cpu -notcontains "x64")) {
    throw "Platform package is not restricted to Windows x64"
}
if ($rootManifest.name -ne $packageName -or $rootManifest.version -ne $Version) {
    throw "Root package manifest does not match $packageName@$Version"
}
if ($null -eq $aliasProperty -or $aliasProperty.Value -ne "npm:$packageName@$platformVersion") {
    throw "Root package does not alias $platformAlias to $packageName@$platformVersion"
}

Invoke-CheckedCommand "node" @("--check", (Join-Path $rootStage "bin/codex.js"))
Invoke-CheckedCommand "npm" @(
    "publish",
    $platformTarball,
    "--access",
    "public",
    "--tag",
    $platformTag,
    "--dry-run",
    "--registry",
    $registry
)
Invoke-CheckedCommand "npm" @(
    "publish",
    $rootTarball,
    "--access",
    "public",
    "--tag",
    "latest",
    "--dry-run",
    "--registry",
    $registry
)

if (-not $Publish) {
    Write-Host "Dry-run completed. Tarballs are in $outputRoot"
    Write-Host "Publish with: pwsh ./scripts/publish_personal_npm.ps1 -Version $Version -Publish"
    return
}

# 主包的 optional dependency 指向平台版本，因此平台包必须先发布，顺序不能反过来。
Invoke-CheckedCommand "npm" @(
    "publish",
    $platformTarball,
    "--access",
    "public",
    "--tag",
    $platformTag,
    "--registry",
    $registry
)
Invoke-CheckedCommand "npm" @(
    "publish",
    $rootTarball,
    "--access",
    "public",
    "--tag",
    "latest",
    "--registry",
    $registry
)

Invoke-CheckedCommand "npm" @("view", "$packageName@$Version", "version", "--registry", $registry)
Invoke-CheckedCommand "npm" @("view", "$packageName@$platformVersion", "version", "--registry", $registry)

$verifyPrefix = Join-Path $outputRoot "npm-install"
New-Item -ItemType Directory -Path $verifyPrefix | Out-Null
Invoke-CheckedCommand "npm" @(
    "install",
    "--prefix",
    $verifyPrefix,
    "--no-save",
    "--no-audit",
    "--no-fund",
    "$packageName@$Version",
    "--registry",
    $registry
)

$installedLauncher = Join-Path $verifyPrefix "node_modules/$npmUser/codex/bin/codex.js"
Invoke-CheckedCommand "node" @($installedLauncher, "--help")
Invoke-CheckedCommand "node" @($installedLauncher, "--version")
Write-Host "Published and installed $packageName@$Version for Windows x64."

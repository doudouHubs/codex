[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $RawArguments
)

$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$defaultTarget = if ([string]::IsNullOrWhiteSpace($env:CODEX_RELEASE_TARGET)) {
    "x86_64-pc-windows-msvc"
} else {
    $env:CODEX_RELEASE_TARGET
}
$target = $defaultTarget
$targetDirValue = if ([string]::IsNullOrWhiteSpace($env:CODEX_RELEASE_TARGET_DIR)) {
    Join-Path $repoRoot ".cache\codex-release-target"
} else {
    $env:CODEX_RELEASE_TARGET_DIR
}
$distDirValue = if ([string]::IsNullOrWhiteSpace($env:CODEX_RELEASE_DIST_DIR)) {
    Join-Path $repoRoot "dist\$target"
} else {
    $env:CODEX_RELEASE_DIST_DIR
}
$v8CacheDirValue = if ([string]::IsNullOrWhiteSpace($env:CODEX_RELEASE_V8_CACHE_DIR)) {
    Join-Path $repoRoot ".cache\rusty-v8-150.4.0-x86_64-pc-windows-msvc"
} else {
    $env:CODEX_RELEASE_V8_CACHE_DIR
}
$jobsValue = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_JOBS)) {
    "1"
} else {
    $env:CODEX_BUILD_JOBS
}
$releaseDebug = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_DEBUG)) {
    "0"
} else {
    $env:CODEX_BUILD_DEBUG
}
$releaseStrip = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_STRIP)) {
    "symbols"
} else {
    $env:CODEX_BUILD_STRIP
}
$lto = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_LTO)) {
    "false"
} else {
    $env:CODEX_BUILD_LTO
}
$releaseOptLevel = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_OPT_LEVEL)) {
    "2"
} else {
    $env:CODEX_BUILD_OPT_LEVEL
}
$releaseCodegenUnits = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_CODEGEN_UNITS)) {
    "16"
} else {
    $env:CODEX_BUILD_CODEGEN_UNITS
}
$proxy = if ([string]::IsNullOrWhiteSpace($env:CODEX_BUILD_PROXY)) {
    "http://127.0.0.1:7890"
} else {
    $env:CODEX_BUILD_PROXY
}

function Show-Usage {
    Write-Output @"
Usage: just build-i [--target <rust-target>] [--target-dir <dir>] [--jobs <count>]

Supported target: x86_64-pc-windows-msvc
"@
}

function Resolve-ReleasePath([string] $PathValue) {
    if ([IO.Path]::IsPathRooted($PathValue)) {
        return [IO.Path]::GetFullPath($PathValue)
    }
    return [IO.Path]::GetFullPath((Join-Path $repoRoot $PathValue))
}

function Initialize-MsvcEnvironment([string] $Target) {
    if ($Target -ne "x86_64-pc-windows-msvc") {
        throw "MSVC environment initialization currently supports only x86_64-pc-windows-msvc."
    }

    $vsWhereCandidates = @()
    if (-not [string]::IsNullOrWhiteSpace(${env:ProgramFiles(x86)})) {
        $vsWhereCandidates += Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    }
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $vsWhereCandidates += Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe"
    }
    $vsWhere = $vsWhereCandidates |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    if (-not $vsWhere) {
        throw "vswhere.exe was not found. Install the Visual Studio C++ build tools before running build-i."
    }

    $installPath = & $vsWhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath 2>$null | Select-Object -First 1
    if ($installPath) {
        $installPath = $installPath.ToString().Trim()
    }
    if ([string]::IsNullOrWhiteSpace($installPath)) {
        throw "Visual Studio with the x64 MSVC toolset was not found."
    }

    # 这台机器可能预置了 VS2019 的 VSINSTALLDIR；若不清掉，vcvars64.bat 会把旧安装
    # 当成当前上下文，导致 Cargo 看到的是混合工具链，最终表现为 mspdbcore.dll 缺失。
    $vcvars = Join-Path $installPath "VC\Auxiliary\Build\vcvars64.bat"
    if (-not (Test-Path -LiteralPath $vcvars -PathType Leaf)) {
        throw "x64 MSVC environment script was not found at $vcvars."
    }

    $variablesToClear = @(
        "VSINSTALLDIR",
        "VCINSTALLDIR",
        "VCToolsInstallDir",
        "VCToolsVersion",
        "VCToolsRedistDir",
        "VisualStudioVersion",
        "DevEnvDir",
        "VCIDEInstallDir",
        "INCLUDE",
        "EXTERNAL_INCLUDE",
        "LIB",
        "LIBPATH",
        "WindowsSdkDir",
        "WindowsSdkVersion",
        "WindowsSDKVersion",
        "WindowsSdkVerBinPath",
        "WindowsSdkBinPath",
        "WindowsLibPath",
        "UniversalCRTSdkDir",
        "UCRTVersion",
        "ExtensionSdkDir",
        "NETFXSDKDir"
    )
    $clearCommands = ($variablesToClear | ForEach-Object { 'set "{0}="' -f $_ }) -join " && "
    $command = '{0} && call "{1}" >nul 2>&1 && set' -f $clearCommands, $vcvars
    $environmentLines = & cmd.exe /d /s /c $command
    $msvcExitCode = $LASTEXITCODE
    if ($msvcExitCode -ne 0) {
        throw "MSVC environment initialization failed with exit code $msvcExitCode using $vcvars."
    }

    $variablesToImport = @(
        "INCLUDE",
        "LIB",
        "LIBPATH",
        "PATH",
        "UCRTVersion",
        "UniversalCRTSdkDir",
        "VCINSTALLDIR",
        "VCToolsInstallDir",
        "WindowsLibPath",
        "WindowsSdkBinPath",
        "WindowsSdkDir",
        "WindowsSDKLibVersion",
        "WindowsSDKVersion"
    )
    foreach ($line in $environmentLines) {
        if ($line -notmatch "^(.*?)=(.*)$") {
            continue
        }

        $name = $Matches[1]
        $value = $Matches[2]
        if ($variablesToImport -contains $name) {
            if ($name -ieq "Path") {
                $name = "PATH"
            }
            Set-Item -Path "Env:$name" -Value $value
        }
    }

    $missingTools = @()
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        $missingTools += "cl.exe"
    }
    if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        $missingTools += "link.exe"
    }

    $mspdbCore = $null
    foreach ($pathEntry in ($env:PATH -split ";")) {
        if ([string]::IsNullOrWhiteSpace($pathEntry)) {
            continue
        }
        $candidate = Join-Path $pathEntry "mspdbcore.dll"
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $mspdbCore = $candidate
            break
        }
    }
    if (-not $mspdbCore) {
        $missingTools += "mspdbcore.dll"
    }
    if ($missingTools.Count -gt 0) {
        throw "MSVC environment is incomplete; missing $($missingTools -join ', ')."
    }

    $rustc = Get-Command rustc.exe -ErrorAction SilentlyContinue
    $rustLld = $null
    if ($rustc) {
        $sysroot = (& $rustc.Source --print sysroot 2>$null).Trim()
        $rustHost = & $rustc.Source -vV 2>$null |
            Select-String "^host: " |
            ForEach-Object { $_.Line.Substring(6).Trim() }
        if ($sysroot -and $rustHost) {
            $rustLld = Join-Path $sysroot "lib\rustlib\$rustHost\bin\rust-lld.exe"
        }
    }
    if ($rustLld -and (Test-Path -LiteralPath $rustLld -PathType Leaf)) {
        # rust-lld 不需要加载 MSVC 的 PDB 链接组件，能绕开低提交额度机器上的
        # LNK1171/错误代码 1455，同时仍使用刚导入的 MSVC SDK 库和头文件。
        $linkerVariable = "CARGO_TARGET_{0}_LINKER" -f $Target.ToUpperInvariant().Replace("-", "_")
        Set-Item -Path "Env:$linkerVariable" -Value $rustLld
        Write-Host "Using Rust linker: $rustLld"
    }

    Write-Host "Using MSVC toolchain: $env:VCToolsInstallDir"
}

# 先解析兼容旧习惯的 Cargo 风格参数，再统一校验，避免参数被静默忽略。
if ($null -eq $RawArguments) {
    $RawArguments = @()
}
for ($index = 0; $index -lt $RawArguments.Count; $index++) {
    $argument = $RawArguments[$index]
    if ($argument -in @("-h", "--help")) {
        Show-Usage
        exit 0
    }
    if ($argument -eq "--") {
        continue
    }
    if ($argument -eq "--target" -or $argument.StartsWith("--target=")) {
        if ($argument.StartsWith("--target=")) {
            $target = $argument.Substring("--target=".Length)
        } else {
            if ($index + 1 -ge $RawArguments.Count) {
                throw "--target requires a value."
            }
            $index++
            $target = $RawArguments[$index]
        }
        continue
    }
    if ($argument -eq "--target-dir" -or $argument.StartsWith("--target-dir=")) {
        if ($argument.StartsWith("--target-dir=")) {
            $targetDirValue = $argument.Substring("--target-dir=".Length)
        } else {
            if ($index + 1 -ge $RawArguments.Count) {
                throw "--target-dir requires a value."
            }
            $index++
            $targetDirValue = $RawArguments[$index]
        }
        continue
    }
    if ($argument -eq "--jobs" -or $argument.StartsWith("--jobs=")) {
        if ($argument.StartsWith("--jobs=")) {
            $jobsValue = $argument.Substring("--jobs=".Length)
        } else {
            if ($index + 1 -ge $RawArguments.Count) {
                throw "--jobs requires a value."
            }
            $index++
            $jobsValue = $RawArguments[$index]
        }
        continue
    }
    throw "Unsupported build-i argument '$argument'. Use --target, --target-dir, or --jobs."
}

if ($target -ne "x86_64-pc-windows-msvc") {
    throw "Windows build-i currently supports only x86_64-pc-windows-msvc."
}

$parsedJobs = 0
if (-not [int]::TryParse([string] $jobsValue, [ref] $parsedJobs) -or $parsedJobs -lt 1) {
    throw "--jobs must be a positive integer, got '$jobsValue'."
}
if ($releaseOptLevel -notmatch "^(0|1|2|3|s|z)$") {
    throw "CODEX_BUILD_OPT_LEVEL must be one of 0, 1, 2, 3, s, or z; got '$releaseOptLevel'."
}
$parsedCodegenUnits = 0
if (-not [int]::TryParse([string] $releaseCodegenUnits, [ref] $parsedCodegenUnits) -or $parsedCodegenUnits -lt 1) {
    throw "CODEX_BUILD_CODEGEN_UNITS must be a positive integer, got '$releaseCodegenUnits'."
}

$targetDir = Resolve-ReleasePath $targetDirValue
$distDir = Resolve-ReleasePath $distDirValue
$v8CacheDir = Resolve-ReleasePath $v8CacheDirValue
$releaseDir = Join-Path (Join-Path $targetDir $target) "release"
$packageDir = Join-Path $distDir "codex-package"
$archivePath = Join-Path $distDir "codex-package-$target.tar.gz"
$v8Archive = Join-Path $v8CacheDir "rusty_v8_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.lib.gz"
$v8Binding = Join-Path $v8CacheDir "src_binding_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.rs"

foreach ($requiredAsset in @($v8Archive, $v8Binding)) {
    if (-not (Test-Path -LiteralPath $requiredAsset -PathType Leaf)) {
        throw "Missing verified Codex V8 asset: $requiredAsset"
    }
}

$python = (Get-Command python.exe -ErrorAction Stop).Source
$cargoWrapper = Join-Path $repoRoot "scripts\run_cargo_with_codex_v8.py"
$packageBuilder = Join-Path $repoRoot "scripts\build_codex_package.py"
$codexBinary = Join-Path $releaseDir "codex.exe"
$codeModeHostBinary = Join-Path $releaseDir "codex-code-mode-host.exe"
$supervisorBinary = Join-Path $releaseDir "codex-supervisor.exe"
$commandRunnerBinary = Join-Path $releaseDir "codex-command-runner.exe"
$sandboxSetupBinary = Join-Path $releaseDir "codex-windows-sandbox-setup.exe"

$env:TARGET = $target
$env:CARGO_TARGET_DIR = $targetDir
$env:CARGO_BUILD_JOBS = [string] $parsedJobs
$env:CARGO_PROFILE_RELEASE_LTO = $lto
# codex-core 的 release 优化峰值很高；仍保持 release 产物和完整功能，只调整本地构建的
# 优化强度与代码生成分片，避免 LLVM 在 16 GB 机器上因提交内存不足直接退出。
$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = $releaseOptLevel
$env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = [string] $parsedCodegenUnits
# 本地正式包不需要调试符号；关闭 line tables/PDB 能显著降低 Windows rustc 的峰值内存。
$env:CARGO_PROFILE_RELEASE_DEBUG = $releaseDebug
$env:CARGO_PROFILE_RELEASE_STRIP = $releaseStrip
$env:CARGO_INCREMENTAL = "0"
$env:LIBSQLITE3_FLAGS = "SQLITE_DISABLE_INTRINSIC"
$env:HTTP_PROXY = $proxy
$env:HTTPS_PROXY = $proxy
$env:RUSTY_V8_ARCHIVE = $v8Archive
$env:RUSTY_V8_SRC_BINDING_PATH = $v8Binding
# aws-lc-sys 自带经过发布包校验的 x64 NASM 对象；本机没有 nasm.exe 时使用它，避免把
# TLS 功能变成额外的系统依赖。用户显式设置该变量时仍保留其选择。
if ([string]::IsNullOrWhiteSpace($env:AWS_LC_SYS_PREBUILT_NASM)) {
    $env:AWS_LC_SYS_PREBUILT_NASM = "1"
}

# 普通 PowerShell 不会自动继承 Visual Studio 开发者环境；aws-lc-sys 会直接调用
# cl.exe，必须先导入同一套 MSVC、Windows SDK 和 PDB 运行时路径。
Initialize-MsvcEnvironment -Target $target

Push-Location (Join-Path $repoRoot "codex-rs")
try {
    Write-Host "Building Codex release for $target with $parsedJobs jobs..."
    & $python $cargoWrapper cargo build --locked --target $target --release `
        --bin codex `
        --bin codex-code-mode-host `
        --bin codex-supervisor `
        --bin codex-command-runner `
        --bin codex-windows-sandbox-setup
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo release build failed with exit code $LASTEXITCODE."
    }

    Write-Host "Staging Codex package at $packageDir..."
    $packageArguments = @(
        $packageBuilder
        "--target"
        $target
        "--variant"
        "codex"
        "--cargo-profile"
        "release"
        "--entrypoint-bin"
        $codexBinary
        "--code-mode-host-bin"
        $codeModeHostBinary
        "--codex-supervisor-bin"
        $supervisorBinary
        "--codex-command-runner-bin"
        $commandRunnerBinary
        "--codex-windows-sandbox-setup-bin"
        $sandboxSetupBinary
        "--package-dir"
        $packageDir
        "--archive-output"
        $archivePath
        "--force"
    )
    & $python @packageArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Codex package staging failed with exit code $LASTEXITCODE."
    }
} finally {
    Pop-Location
}

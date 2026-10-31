# Build and run the C ABI smoke test (Block B4 acceptance criterion).
#
#   ./tests/run_c_smoke.ps1 [-Steps 5] [-SimId 0123456789abcdef]
#
# Needs `cargo` and a C compiler (`gcc` from MSYS2 / MinGW). The script builds
# the cdylib and the `mock_host` example, compiles `tests/c_smoke.c`, then runs
# both processes against the real shared-memory protocol.
[CmdletBinding()]
param(
    [ValidateRange(1, 1000)]
    [int]$Steps = 5,
    [string]$SimId = ""
)

$ErrorActionPreference = "Stop"

$crate = Split-Path -Parent $PSScriptRoot
$workspace = (Resolve-Path (Join-Path $crate "../..")).Path
$debug = Join-Path $workspace "target/debug"
$gcc = Get-Command gcc -ErrorAction SilentlyContinue
if (-not $gcc) {
    throw "gcc not found; install MSYS2/MinGW or use tests/run_c_smoke.sh on Linux"
}
# Some MSYS2 installs keep the compiler runtime DLLs in a `cpp` subdirectory.
$gccBin = Split-Path -Parent $gcc.Source
$gccRuntime = Join-Path $gccBin "cpp"
$env:PATH = if (Test-Path $gccRuntime) { "$gccRuntime;$gccBin;$env:PATH" } else { "$gccBin;$env:PATH" }

if ($SimId -eq "") {
    $bytes = New-Object byte[] 8
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $value = [BitConverter]::ToUInt64($bytes, 0) -band 0x7FFFFFFFFFFFFFFF -bor 1
    $SimId = $value.ToString("x16")
}
$hostArg = "0x$SimId"

Push-Location $crate
try {
    # Native stderr must not terminate the script under $ErrorActionPreference = Stop.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $build = cargo build -p coupling-client 2>&1
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        $build = cargo build -p coupling-client --example mock_host 2>&1
        $code = $LASTEXITCODE
    }
    $ErrorActionPreference = $previous
    if ($code -ne 0) { $build; throw "cargo build failed" }
}
finally {
    Pop-Location
}

$dll = Join-Path $debug "coupling_client.dll"
$hostExe = Join-Path $debug "examples/mock_host.exe"
$smokeExe = Join-Path $debug "c_smoke.exe"
if (-not (Test-Path $dll)) { throw "missing $dll" }
if (-not (Test-Path $hostExe)) { throw "missing $hostExe" }

& $gcc.Source -std=c11 -Wall -Wextra -Werror `
    -I (Join-Path $crate "include") `
    (Join-Path $crate "tests/c_smoke.c") `
    $dll -o $smokeExe
if ($LASTEXITCODE -ne 0) { throw "gcc failed to build tests/c_smoke.c" }

$hostOut = Join-Path $debug "c_smoke_host.out"
$hostErr = Join-Path $debug "c_smoke_host.err"
Remove-Item -ErrorAction SilentlyContinue $hostOut, $hostErr

# The host waits for the hello, so it must be started before the client connects.
# System.Diagnostics.Process is used directly: Start-Process does not expose ExitCode
# reliably when stdout/stderr are redirected on Windows PowerShell 5.1.
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $hostExe
$psi.Arguments = "`"$hostArg`" 1 $Steps 15000"
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true
$hostProc = New-Object System.Diagnostics.Process
$hostProc.StartInfo = $psi
if (-not $hostProc.Start()) { throw "failed to start $hostExe" }
$hostOutTask = $hostProc.StandardOutput.ReadToEndAsync()
$hostErrTask = $hostProc.StandardError.ReadToEndAsync()

$exit = 1
try {
    $env:PATH = "$debug;$env:PATH"
    & $smokeExe $hostArg "$Steps"
    $exit = $LASTEXITCODE

    if (-not $hostProc.WaitForExit(30000)) {
        Write-Host "ERROR: mock_host did not exit within 30 s"
        $exit = 1
    }
    else {
        $hostText = $hostOutTask.Result
        $hostErrorText = $hostErrTask.Result
        Set-Content -Path $hostOut -Value $hostText
        Set-Content -Path $hostErr -Value $hostErrorText
        if ($hostProc.ExitCode -ne 0) {
            Write-Host "ERROR: mock_host exited with $($hostProc.ExitCode)"
            $exit = 1
        }
        if ($hostText -notmatch "HOST_DONE") {
            Write-Host "ERROR: mock_host did not report HOST_DONE"
            $exit = 1
        }
        if ($hostErrorText.Trim().Length -gt 0) {
            Write-Host "mock_host stderr:"
            Write-Host $hostErrorText
            $exit = 1
        }
    }
}
finally {
    if (-not $hostProc.HasExited) {
        $hostProc.Kill()
        $hostProc.WaitForExit(5000) | Out-Null
    }
}

if ($exit -ne 0) {
    exit $exit
}
Write-Host "C ABI smoke test passed ($Steps steps, sim_id=0x$SimId)"

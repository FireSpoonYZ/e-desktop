# Run from Windows PowerShell 5.1: powershell -NoProfile -File scripts/windows-smoke.ps1
# Creates disposable windows; the Rust example restricts all actions to their PID.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$output = Join-Path $env:TEMP ('e-desktop-smoke-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $output | Out-Null
$exe = Join-Path $output 'e-desktop-smoke-fixture.exe'
Add-Type -TypeDefinition (Get-Content "$PSScriptRoot/windows-smoke-fixture.cs" -Raw) -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions,System.Core -OutputAssembly $exe -OutputType WindowsApplication
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class SmokeCleanup {
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
'@
$fixture = Start-Process $exe -PassThru
try {
    $fixture.WaitForInputIdle(10000) | Out-Null
    Push-Location $root
    try {
        cargo run -p e-desktop --example windows_smoke --no-default-features --locked -- $fixture.Id "$output/evidence.json"
        if ($LASTEXITCODE -ne 0) { throw "Native smoke failed; inspect $output/evidence.json" }
    } finally { Pop-Location }
    Write-Output "Native smoke passed. Evidence: $output/evidence.json"
} finally {
    # Gracefully close only the fixture's remaining windows, including a failed run.
    $log = Join-Path $env:TEMP 'e-desktop-smoke-state.json'
    if (Test-Path $log) {
        $state = Get-Content $log -Raw | ConvertFrom-Json
        if ($state.pid -eq $fixture.Id) {
            foreach ($window in $state.windows) {
                [uint32]$owner = 0
                [SmokeCleanup]::GetWindowThreadProcessId([IntPtr]$window.hwnd, [ref]$owner) | Out-Null
                if ($owner -eq $fixture.Id) {
                    [SmokeCleanup]::PostMessage([IntPtr]$window.hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
                }
            }
        }
    }
}

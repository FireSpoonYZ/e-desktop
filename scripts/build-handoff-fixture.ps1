param([string]$OutputPath = (Join-Path $PSScriptRoot "../target/handoff-fixture/handoff-fixture.exe"))
$ErrorActionPreference = "Stop"
$csc = Join-Path $env:WINDIR "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
if (!(Test-Path $csc)) { throw "Required .NET Framework C# compiler not found: $csc" }
$OutputPath = [IO.Path]::GetFullPath($OutputPath)
New-Item -ItemType Directory -Force -Path (Split-Path $OutputPath) | Out-Null
& $csc /nologo /target:winexe /optimize+ "/out:$OutputPath" /reference:System.dll /reference:System.Drawing.dll /reference:System.Windows.Forms.dll /reference:System.Web.Extensions.dll (Join-Path $PSScriptRoot "handoff-fixture.cs")
if ($LASTEXITCODE -ne 0) { throw "handoff fixture compilation failed ($LASTEXITCODE)" }
Write-Output "Compiled only (not started): $OutputPath"

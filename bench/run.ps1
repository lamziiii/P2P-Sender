# Runs one transfer between two headless nodes (examples/bench.rs, built
# with `cargo build --release --example bench`) and samples their memory and
# CPU from outside.
#
#   .\bench\run.ps1 -File C:\tmp\1G.bin
param(
  [Parameter(Mandatory)][string]$File,
  [string]$Work = (Join-Path $PSScriptRoot '..\bench-work'),
  # Seconds the receiver is online before the sender starts (0 = cold start).
  [int]$HeadStart = 0
)
$ErrorActionPreference = 'Stop'
# Where Cargo builds (see scripts/dist.mjs).
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $env:LOCALAPPDATA 'p2pshare-target' }
$bench = Join-Path $target 'release\examples\bench.exe'

if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force $Work | Out-Null
$Work = (Resolve-Path $Work).Path
$File = (Resolve-Path $File).Path

function Start-Node([string]$role) {
  $out = Join-Path $Work "$role.out"
  $argv = @($role, $Work)
  if ($role -eq 'send') { $argv += $File }
  $p = Start-Process -FilePath $bench -ArgumentList ($argv | ForEach-Object { "`"$_`"" }) -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError "$out.err"
  [pscustomobject]@{ Role = $role; Proc = $p; Out = $out; PeakMB = 0.0; CpuS = 0.0 }
}

$recv = Start-Node 'recv'
Start-Sleep -Seconds $HeadStart
$nodes = @($recv, (Start-Node 'send'))
while ($nodes | Where-Object { -not $_.Proc.HasExited }) {
  foreach ($n in $nodes) {
    try {
      $n.Proc.Refresh()
      if (-not $n.Proc.HasExited) {
        $n.PeakMB = [math]::Max($n.PeakMB, $n.Proc.WorkingSet64 / 1MB)
        $n.CpuS = $n.Proc.TotalProcessorTime.TotalSeconds
      }
    } catch {}
  }
  Start-Sleep -Milliseconds 200
}

foreach ($n in $nodes) {
  $result = Select-String -Path $n.Out -Pattern '^RESULT (.*)$' | Select-Object -Last 1
  $connected = Select-String -Path $n.Out -Pattern 'connect\S* en (\d+) ms' | Select-Object -Last 1
  [pscustomobject]@{
    role      = $n.Role
    result    = if ($result) { $result.Matches[0].Groups[1].Value } else { '(none)' }
    connectMs = if ($connected) { [int]$connected.Matches[0].Groups[1].Value } else { $null }
    peakRamMB = [math]::Round($n.PeakMB, 1)
    cpuS      = [math]::Round($n.CpuS, 2)
  } | ConvertTo-Json -Compress
}

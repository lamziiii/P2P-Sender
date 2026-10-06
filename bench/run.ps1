# Runs one transfer between two headless nodes and samples their memory and
# CPU from outside, the same way for both versions.
#
#   .\bench\run.ps1 -Sender v3 -Receiver v3 -File C:\tmp\1G.bin
#   .\bench\run.ps1 -Sender v2 -Receiver v3 -File C:\tmp\small.bin   # interop
param(
  [ValidateSet('v2', 'v3')][string]$Sender = 'v3',
  [ValidateSet('v2', 'v3')][string]$Receiver = 'v3',
  [Parameter(Mandatory)][string]$File,
  [string]$Work = (Join-Path $PSScriptRoot '..\bench-work'),
  [string]$ElectronDir = (Join-Path $PSScriptRoot '..\..\P2P_electron'),
  # Seconds the receiver is online before the sender starts (0 = cold start).
  [int]$HeadStart = 0
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$v3 = Join-Path $root 'src-tauri\target\release\examples\bench.exe'
$v2 = Join-Path $root 'bench\electron-bench.mjs'

if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force $Work | Out-Null
$Work = (Resolve-Path $Work).Path
$File = (Resolve-Path $File).Path

function Start-Node([string]$version, [string]$role) {
  $out = Join-Path $Work "$role.out"
  $argv = @($role, $Work)
  if ($role -eq 'send') { $argv += $File }
  if ($version -eq 'v3') {
    $p = Start-Process -FilePath $v3 -ArgumentList ($argv | ForEach-Object { "`"$_`"" }) -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError "$out.err"
  } else {
    $all = @($v2, $ElectronDir) + $argv
    $p = Start-Process -FilePath 'node' -ArgumentList ($all | ForEach-Object { "`"$_`"" }) -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError "$out.err"
  }
  [pscustomobject]@{ Role = $role; Version = $version; Proc = $p; Out = $out; PeakMB = 0.0; CpuS = 0.0 }
}

$recv = Start-Node $Receiver 'recv'
Start-Sleep -Seconds $HeadStart
$nodes = @($recv, (Start-Node $Sender 'send'))
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
    version   = $n.Version
    result    = if ($result) { $result.Matches[0].Groups[1].Value } else { '(none)' }
    connectMs = if ($connected) { [int]$connected.Matches[0].Groups[1].Value } else { $null }
    peakRamMB = [math]::Round($n.PeakMB, 1)
    cpuS      = [math]::Round($n.CpuS, 2)
  } | ConvertTo-Json -Compress
}

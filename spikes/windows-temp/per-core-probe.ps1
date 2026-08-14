# THROWAWAY. Spike code for LB-11, not part of the LoadBear build.
#
# Settles whether Core Temp's eight values are genuine per-core sensors or one
# die sensor presented eight times. Pins a busy loop to a single core and
# watches which values move.
#
# If only the loaded core rises, they are real per-core sensors.
# If all eight rise together, it is one die sensor.

$ErrorActionPreference = 'Stop'

function Read-CoreTemps {
  $mmf = [System.IO.MemoryMappedFiles.MemoryMappedFile]::OpenExisting('CoreTempMappingObjectEx')
  $v = $mmf.CreateViewAccessor(0, 0, [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::Read)
  $n = $v.ReadUInt32(1536)
  $t = @()
  for ($i = 0; $i -lt [int]$n; $i++) { $t += [math]::Round($v.ReadSingle(1544 + $i * 4), 1) }
  $v.Dispose(); $mmf.Dispose()
  return $t
}

Write-Output "Letting the machine settle for 20 seconds..."
Start-Sleep -Seconds 20
$before = Read-CoreTemps
Write-Output ("baseline: " + ($before -join ', '))

# Pin a spinner to logical processor 2 only. Affinity mask 0x4 is bit 2.
Write-Output ""
Write-Output "Loading logical processor 2 only, for 45 seconds..."
$job = Start-Process -PassThru -WindowStyle Hidden powershell -ArgumentList @(
  '-NoProfile', '-Command',
  '$end=(Get-Date).AddSeconds(45); while((Get-Date) -lt $end){ $x=1 }'
)
Start-Sleep -Milliseconds 500
$job.ProcessorAffinity = [IntPtr]0x4

Start-Sleep -Seconds 40
$after = Read-CoreTemps
Write-Output ("under load: " + ($after -join ', '))

try { $job.Kill() } catch { }

Write-Output ""
Write-Output "=== Delta per core ==="
$deltas = @()
for ($i = 0; $i -lt $before.Count; $i++) {
  $d = [math]::Round($after[$i] - $before[$i], 1)
  $deltas += $d
  Write-Output ("Core {0}: {1,6} -> {2,6}   delta {3,6}" -f $i, $before[$i], $after[$i], $d)
}

$max = ($deltas | Measure-Object -Maximum).Maximum
$min = ($deltas | Measure-Object -Minimum).Minimum
$spread = [math]::Round($max - $min, 1)

Write-Output ""
Write-Output "spread between the largest and smallest delta: $spread C"
if ($spread -ge 3.0) {
  Write-Output "VERDICT: genuine per-core sensors. One core moved independently of the others."
} elseif ($spread -le 1.0) {
  Write-Output "VERDICT: one die sensor. All values moved together."
} else {
  Write-Output "VERDICT: inconclusive. Rerun on an idle machine, the baseline was probably not settled."
}

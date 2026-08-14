# THROWAWAY. Spike code for LB-02, not part of the LoadBear build.
#
# Reads Core Temp's shared memory block to capture ground truth without
# needing the GUI or elevation. Offsets below are asserted, not trusted:
# the script prints plausibility checks so a wrong layout is obvious.

$ErrorActionPreference = 'Stop'

$names = @('CoreTempMappingObjectEx', 'Local\CoreTempMappingObjectEx',
           'CoreTempMappingObject',   'Local\CoreTempMappingObject')

$mmf = $null
$opened = $null
foreach ($n in $names) {
  try {
    $mmf = [System.IO.MemoryMappedFiles.MemoryMappedFile]::OpenExisting($n)
    $opened = $n
    break
  } catch { }
}

if (-not $mmf) {
  Write-Output "FAIL: no Core Temp shared memory object found. Tried: $($names -join ', ')"
  Write-Output "Is Core Temp running?"
  exit 1
}

Write-Output "Opened shared memory object: $opened"

$view = $mmf.CreateViewAccessor(0, 0, [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::Read)

# Asserted layout of CORE_TEMP_SHARED_DATA_EX
$OFF_TJMAX    = 1024   # uint[128], follows uiLoad[256]
$OFF_CORECNT  = 1536
$OFF_CPUCNT   = 1540
$OFF_TEMP     = 1544   # float[256]
$OFF_VID      = 2568
$OFF_CPUSPEED = 2572
$OFF_CPUNAME  = 2584   # char[100]
$OFF_FAHR     = 2684
$OFF_DELTA    = 2685

$coreCnt = $view.ReadUInt32($OFF_CORECNT)
$cpuCnt  = $view.ReadUInt32($OFF_CPUCNT)
$tjmax   = $view.ReadUInt32($OFF_TJMAX)
$vid     = $view.ReadSingle($OFF_VID)
$speed   = $view.ReadSingle($OFF_CPUSPEED)
$fahr    = $view.ReadByte($OFF_FAHR)
$delta   = $view.ReadByte($OFF_DELTA)

$nameBytes = New-Object byte[] 100
$view.ReadArray($OFF_CPUNAME, $nameBytes, 0, 100) | Out-Null
$cpuName = ([System.Text.Encoding]::ASCII.GetString($nameBytes) -split "`0")[0]

Write-Output ""
Write-Output "=== Raw values at asserted offsets ==="
Write-Output "CPU name      : $cpuName"
Write-Output "Core count    : $coreCnt"
Write-Output "CPU count     : $cpuCnt"
Write-Output "TjMax         : $tjmax"
Write-Output "VID           : $([math]::Round($vid,4))"
Write-Output "CPU speed MHz : $([math]::Round($speed,1))"
Write-Output "Fahrenheit    : $fahr (0 = Celsius)"
Write-Output "DeltaToTjMax  : $delta (0 = absolute temperature)"

Write-Output ""
Write-Output "=== Per-core temperatures ==="
$temps = @()
for ($i = 0; $i -lt [int]$coreCnt; $i++) {
  $t = $view.ReadSingle($OFF_TEMP + ($i * 4))
  $temps += $t
  Write-Output ("Core {0}: {1} C" -f $i, [math]::Round($t,1))
}

Write-Output ""
Write-Output "=== Plausibility checks ==="
$ok = $true

if ($cpuName -match '\S') { Write-Output "PASS  CPU name is a readable string" }
else { Write-Output "FAIL  CPU name is empty or garbage, offsets are wrong"; $ok = $false }

if ($coreCnt -ge 1 -and $coreCnt -le 128) { Write-Output "PASS  core count $coreCnt is in a sane range" }
else { Write-Output "FAIL  core count $coreCnt is implausible, offsets are wrong"; $ok = $false }

# TjMax is deliberately NOT a layout check. Core Temp publishes zero here on this
# AMD Renoir part, confirmed by the fields on either side of the array reading
# correctly. A zero is a real finding about the CPU, not evidence of bad offsets.
if ($tjmax -ge 60 -and $tjmax -le 130) { Write-Output "INFO  TjMax $tjmax is published" }
elseif ($tjmax -eq 0) { Write-Output "INFO  TjMax not published by Core Temp on this part. Must come from the spec database" }
else { Write-Output "WARN  TjMax $tjmax is neither zero nor plausible, worth a second look" }

$badTemps = @($temps | Where-Object { $_ -lt 5 -or $_ -gt 120 })
if ($temps.Count -gt 0 -and $badTemps.Count -eq 0) { Write-Output "PASS  all $($temps.Count) core temperatures are in a sane range" }
else { Write-Output "FAIL  $($badTemps.Count) of $($temps.Count) temperatures are implausible"; $ok = $false }

Write-Output ""
if ($ok) { Write-Output "RESULT: layout confirmed, values are usable as ground truth" }
else { Write-Output "RESULT: layout is wrong, do not trust these numbers" }

$view.Dispose()
$mmf.Dispose()

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [ValidateRange(1, 1000)]
    [int]$Cycles = 100,
    [ValidateRange(100, 10000)]
    [int]$MaximumStopMilliseconds = 1000,
    [string]$Output = ".\start-stop-gate.json"
)

$ErrorActionPreference = "Stop"
$exe = (Resolve-Path -LiteralPath $Executable).Path
$outputPath = [System.IO.Path]::GetFullPath($Output)
$records = [System.Collections.Generic.List[object]]::new()
$failed = $false

function Invoke-ProductCommand {
    param([string[]]$Arguments)
    $text = & $exe @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "command failed: $($Arguments[0])"
    }
    return ($text -join "`n")
}

function Read-ProductStatus {
    $json = Invoke-ProductCommand -Arguments @("status", "--json")
    return $json | ConvertFrom-Json
}

function Save-Results {
    $document = [ordered]@{
        format = 1
        requested_cycles = $Cycles
        maximum_stop_milliseconds = $MaximumStopMilliseconds
        completed_cycles = $records.Count
        passed = (-not $failed -and $records.Count -eq $Cycles)
        records = $records
    }
    $parent = Split-Path -Parent $outputPath
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    $temporary = "$outputPath.tmp"
    $document | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $temporary -Encoding utf8
    Move-Item -LiteralPath $temporary -Destination $outputPath -Force
}

for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
    $startedAt = [DateTimeOffset]::UtcNow
    $startWatch = [System.Diagnostics.Stopwatch]::StartNew()
    $stopMilliseconds = $null
    try {
        Invoke-ProductCommand -Arguments @("start") | Out-Null
        $startWatch.Stop()
        $connected = Read-ProductStatus
        if ($connected.host.lifecycle.state -ne "connected") {
            throw "host did not report connected"
        }
        if ($connected.android.vpn_fd_open -ne $true) {
            throw "Android did not report an open VPN descriptor"
        }

        $stopWatch = [System.Diagnostics.Stopwatch]::StartNew()
        Invoke-ProductCommand -Arguments @("stop") | Out-Null
        $stopWatch.Stop()
        $stopMilliseconds = $stopWatch.ElapsedMilliseconds
        if ($stopMilliseconds -gt $MaximumStopMilliseconds) {
            throw "explicit Stop exceeded the lifecycle deadline"
        }
        $stopped = Read-ProductStatus
        if ($stopped.host.lifecycle.state -ne "stopped") {
            throw "host did not report stopped"
        }
        if ($null -ne $stopped.android -and $stopped.android.vpn_fd_open -eq $true) {
            throw "Android still reports an open VPN descriptor"
        }

        $records.Add([ordered]@{
            cycle = $cycle
            started_at = $startedAt.ToString("o")
            start_ms = $startWatch.ElapsedMilliseconds
            stop_ms = $stopMilliseconds
            passed = $true
            failure_category = $null
        })
    }
    catch {
        $failed = $true
        try { Invoke-ProductCommand -Arguments @("stop") | Out-Null } catch { }
        $records.Add([ordered]@{
            cycle = $cycle
            started_at = $startedAt.ToString("o")
            start_ms = $startWatch.ElapsedMilliseconds
            stop_ms = $stopMilliseconds
            passed = $false
            failure_category = "lifecycle_transaction"
        })
        Save-Results
        throw "lifecycle gate failed at cycle $cycle; details were intentionally redacted"
    }
    Save-Results
}

Write-Host "Completed $Cycles start/stop cycles. Result: $outputPath"

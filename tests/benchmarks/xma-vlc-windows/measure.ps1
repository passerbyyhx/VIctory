param(
    [Parameter(Mandatory)][string]$Base,
    [Parameter(Mandatory)][string]$Head,
    [string]$Results = (Join-Path $PWD 'xma-364-pairs.csv'),
    [ValidateRange(2, 100)][int]$Pairs = 12,
    [ValidateRange(0, 31)][int]$Cpu = 4
)
$ErrorActionPreference = 'Stop'
$fixture = Join-Path $PSScriptRoot 'decode_stream.rs'
$revisions = @(
    @{ name = 'base'; path = (Resolve-Path $Base).Path; sha = 'd2a4bae1f49533ef5cb512fd1b1ba1513f2e1989' },
    @{ name = 'head'; path = (Resolve-Path $Head).Path; sha = 'e503756597cafb3ac6c259ec82458fa5c61215aa' }
)

foreach ($revision in $revisions) {
    Push-Location $revision.path
    try {
        if ((git rev-parse HEAD) -ne $revision.sha) { throw "Unexpected revision in $PWD" }
        cargo build --locked --release -p victory-formats --lib
        if ($LASTEXITCODE) { throw 'formats release build failed' }
        rustc --edition=2024 -C opt-level=3 -C debuginfo=line-tables-only --extern 'victory_formats=target/release/libvictory_formats.rlib' -L dependency=target/release/deps $fixture -o 'target/release/xma-stream.exe'
        if ($LASTEXITCODE) { throw 'benchmark build failed' }
        $revision.exe = Join-Path $PWD 'target/release/xma-stream.exe'
        $revision.pcm = Join-Path $PWD 'target/xma-bench-check.pcm'
        & $revision.exe $revision.pcm --verify-only
        if ($LASTEXITCODE) { throw 'stream check failed' }
        $revision.hash = (Get-FileHash -Algorithm SHA256 $revision.pcm).Hash
    } finally { Pop-Location }
}
if ($revisions[0].hash -ne $revisions[1].hash) { throw 'The decoded outputs differ' }

# Children inherit this mask. Keep both executables on one logical CPU even
# on hybrid x86 hardware. Restore the calling process after the run.
$process = [Diagnostics.Process]::GetCurrentProcess()
$previousMask = $process.ProcessorAffinity
$mask = [long]1 -shl $Cpu
if (($previousMask.ToInt64() -band $mask) -eq 0) { throw "CPU $Cpu unavailable" }
$rows = [Collections.Generic.List[object]]::new()
try {
    $process.ProcessorAffinity = [IntPtr]$mask
    for ($pair = 1; $pair -le $Pairs; $pair++) {
        $order = if ($pair % 2 -eq 1) { 0, 1 } else { 1, 0 }
        foreach ($i in $order) {
            $revision = $revisions[$i]
            $output = & $revision.exe $revision.pcm
            if ($LASTEXITCODE) { throw "Benchmark failed for $($revision.name)" }
            $line = $output | Select-Object -Last 1
            if ($line -notmatch '^streams=(\d+) frames=(\d+) elapsed_ms=([\d.]+)$') { throw "Unexpected result: $line" }
            $rows.Add([pscustomobject]@{
                pair = $pair; revision = $revision.name; sha = $revision.sha; logical_cpu = $Cpu
                streams = $Matches[1]; frames = $Matches[2]; elapsed_ms = $Matches[3]
                pcm_sha256 = $revision.hash
            })
        }
    }
} finally { $process.ProcessorAffinity = $previousMask }
$rows | Export-Csv -LiteralPath $Results -NoTypeInformation
Write-Output "Recorded $($rows.Count) measurements in $Results"

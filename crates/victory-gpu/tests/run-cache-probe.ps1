param(
    [Parameter(Mandatory)] [string] $ParentExe,
    [Parameter(Mandatory)] [string] $HeadExe,
    [Parameter(Mandatory)] [string] $Output
)
$ErrorActionPreference = 'Stop'
$ParentExe = (Resolve-Path -LiteralPath $ParentExe).Path
$HeadExe = (Resolve-Path -LiteralPath $HeadExe).Path
$Output = (New-Item -ItemType Directory -Path $Output).FullName

$env:VICTORY_D3D12_WARP = $null
foreach ($version in 'parent', 'head') {
    $exe = if ($version -eq 'parent') { $ParentExe } else { $HeadExe }
    Get-FileHash -LiteralPath $exe | Format-List | Out-File "$Output/$version-exe.txt"
}
$pairs = @(
    @{ Name = 'parent-to-head'; Source = $ParentExe; Dest = $HeadExe }
    @{ Name = 'head-to-parent'; Source = $HeadExe; Dest = $ParentExe }
)
foreach ($pair in $pairs) {
    $direction = $pair.Name
    $cache = (New-Item -ItemType Directory -Path "$Output/$direction-cache").FullName
    $env:TEN_CACHE_DIR = $cache
    foreach ($phase in 'first', 'second', 'verify') {
        $env:TEN_CACHE_PHASE = $phase
        $env:VICTORY_PIPELINE_LOG = "$Output/$direction-$phase-pipeline.tsv"
        $exe = if ($phase -eq 'verify') { $pair.Dest } else { $pair.Source }
        & $exe --exact pipeline::cross_version_cache_probe::synthetic_psos_share_the_early_list_across_executables --include-ignored --nocapture --test-threads=1 *> "$Output/$direction-$phase.log"
        if ($LASTEXITCODE -ne 0) {
            Get-Content -LiteralPath "$Output/$direction-$phase.log"
            throw "$direction $phase exited $LASTEXITCODE"
        }
        Copy-Item -LiteralPath "$cache/early-pipelines.txt" "$Output/$direction-$phase-early.txt"
    }
}

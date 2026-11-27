
param(
    [Parameter(Mandatory)] [string] $HeadExe,
    [Parameter(Mandatory)] [string] $ParentExe,
    [Parameter(Mandatory)] [string] $Output,
    [Parameter(Mandatory)] [ValidateSet('warp', 'nvidia')] [string] $Adapter
)
$ErrorActionPreference = 'Stop'
$HeadExe = (Resolve-Path -LiteralPath $HeadExe).Path
$ParentExe = (Resolve-Path -LiteralPath $ParentExe).Path
$Output = (New-Item -ItemType Directory -Path $Output).FullName
$env:VICTORY_D3D12_WARP = if ($Adapter -eq 'warp') { '1' } else { $null }
$env:VICTORY_D3D12_DEBUG = $null
Get-FileHash -LiteralPath $HeadExe,$ParentExe | Format-List | Out-File "$Output/binary-hashes.txt"

foreach ($version in 'head','parent') {
    $exe = if ($version -eq 'head') { $HeadExe } else { $ParentExe }
    $env:EIGHT_LOG_VERSION = $version
    $path = "$Output/$version-new-dir/pipeline.tsv"
    foreach ($mode in 'late','retry','empty') {
        $env:EIGHT_LOG_MODE = $mode
        $env:VICTORY_PIPELINE_LOG = if ($mode -eq 'empty') { '' } else { $path }
        $log = "$Output/$version-$mode.log"
        & $exe --exact pipeline::eight_cache_probe::pipeline_log_open_and_real_events --include-ignored --nocapture --test-threads=1 *> $log
        if ($LASTEXITCODE -ne 0) {
            Get-Content -LiteralPath $log
            throw "$version/$mode exited $LASTEXITCODE"
        }
        Get-Content $log | Select-String 'version=|before_directory|could not be opened'
        if ($mode -ne 'empty' -and (Test-Path -LiteralPath $path)) {
            Copy-Item -LiteralPath $path "$Output/$version-$mode.tsv"
        }
    }
}

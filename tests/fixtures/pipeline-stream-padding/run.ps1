
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Output,
    [string] $ShaderDir = $PSScriptRoot,
    [Parameter(Mandatory)] [ValidateSet('warp', 'nvidia')] [string] $Adapter
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$Output = (New-Item -ItemType Directory -Path $Output).FullName
$env:VICTORY_D3D12_WARP = if ($Adapter -eq 'warp') { '1' } else { $null }
$env:VICTORY_D3D12_DEBUG = $null
$env:EIGHT_CACHE_MS = (Resolve-Path -LiteralPath "$ShaderDir/mesh.dxil").Path
$env:EIGHT_CACHE_MS_PS = (Resolve-Path -LiteralPath "$ShaderDir/mesh-ps.dxil").Path
Get-FileHash -LiteralPath $Exe,$env:EIGHT_CACHE_MS,$env:EIGHT_CACHE_MS_PS | Format-List | Out-File "$Output/binary-hashes.txt"

function Invoke-Probe($Kind, $Series, $Phase, $Byte, $Raw, $Expect) {
    $env:EIGHT_CACHE_KIND = $Kind
    $env:EIGHT_CACHE_DIR = "$Output/$Kind-$Series-cache"
    $env:EIGHT_CACHE_PHASE = $Phase
    $env:EIGHT_CACHE_POISON = $Byte
    $env:EIGHT_CACHE_RAW_PADDING = if ($Raw) { '1' } else { $null }
    $env:EIGHT_CACHE_EXPECT = $Expect
    $log = "$Output/$Kind-$Series-$Phase.log"
    & $Exe --exact pipeline::eight_cache_probe::stream_padding_persistence --include-ignored --nocapture --test-threads=1 *> $log
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath $log
        throw "$Kind/$Series/$Phase exited $LASTEXITCODE"
    }
    Get-Content $log | Select-String 'UNSUPPORTED|library_name=|stream=|LoadPipeline=|CreatePipelineState='
}

foreach ($kind in 'vertex','mesh') {
    New-Item -ItemType Directory -Path "$Output/$kind-fixed-cache","$Output/$kind-raw-cache" | Out-Null
    Invoke-Probe $kind fixed cold aa $false make
    Invoke-Probe $kind fixed warm bb $false load
    Invoke-Probe $kind fixed changed-field bb $false make

    Invoke-Probe $kind raw cold aa $true make
    Invoke-Probe $kind raw same-poison aa $true load
    Invoke-Probe $kind raw changed-poison bb $true observe
    Invoke-Probe $kind raw warm-fixed bb $false observe
}

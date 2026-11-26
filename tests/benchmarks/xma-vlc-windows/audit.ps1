param([Parameter(Mandatory)][string]$Head)
$ErrorActionPreference = 'Stop'
Push-Location $Head
try {
    if ((git rev-parse HEAD) -ne 'e503756597cafb3ac6c259ec82458fa5c61215aa') { throw 'Unexpected PR head' }
    $audit = Join-Path $PWD 'target/xma-364-untimed-audit'
    $src = Join-Path $audit 'src'
    New-Item -ItemType Directory -Path $src -Force | Out-Null
    Get-ChildItem -LiteralPath 'crates/victory-formats/src' | Copy-Item -Destination $src -Recurse -Force
    $path = Join-Path $src 'xma.rs'
    $original = [IO.File]::ReadAllText($path)
    $marker = 'let (_, len, symbol) = self.codes[i];'
    if ([regex]::Matches($original, [regex]::Escape($marker)).Count -ne 1) { throw 'Unexpected Vlc::read source' }
    $line = 'eprintln!("VLC length={len} direct={}", end == first + 1);'
    [IO.File]::WriteAllText($path, $original.Replace($marker, "$marker`n        $line"), [Text.UTF8Encoding]::new($false))
    $rlib = Join-Path $audit 'libvictory_formats.rlib'
    rustc --edition=2024 --crate-type=rlib --crate-name victory_formats -C opt-level=3 (Join-Path $src 'lib.rs') -o $rlib
    if ($LASTEXITCODE) { throw 'Untimed audit library build failed' }
    $exe = Join-Path $audit 'check.exe'
    rustc --edition=2024 -C opt-level=3 --extern "victory_formats=$rlib" (Join-Path $PSScriptRoot 'decode_stream.rs') -o $exe
    if ($LASTEXITCODE) { throw 'Untimed audit fixture build failed' }
    $log = Join-Path $audit 'vlc-read.log'
    & $exe (Join-Path $audit 'audit.pcm') --verify-only 2> $log
    if ($LASTEXITCODE) { throw 'Stream verification failed' }
    $events = @(Get-Content -LiteralPath $log)
    $ones = @($events | Where-Object { $_ -eq 'VLC length=1 direct=true' }).Count
    $sixes = @($events | Where-Object { $_ -eq 'VLC length=6 direct=true' }).Count
    if ($ones -ne 368 -or $sixes -ne 2048 -or $events.Count -ne 2416) { throw 'Unexpected lookup distribution' }
    "Audited one stream: $ones one-bit scale symbols and $sixes six-bit vector symbols. All $($events.Count) used direct lookup."
} finally { Pop-Location }

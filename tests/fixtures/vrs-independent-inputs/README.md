# Independent D3D12 shading-rate inputs

This local probe extends the 4×4 position-color fixture in commit `40bcd0fbffbf19a7e0f3342c4e5f6638d83d6e31` on PR #3511. It supplies a 2×1 per-primitive rate through the vertex shader and a 1×2 value in a single host-bound R8_UINT shading-rate tile. The guest supplies the base rate and combiner pair. The image is intentionally bound through the host because the tested guest path has no image setter.

Use a disposable worktree of this fixture commit; preparation replaces the shading-rate test in that worktree. On a Windows machine with a Tier 2 adapter, the debug layer, Windows SDK DXC, and the repository's Rust toolchain, run the following from its root. Select the dxc.exe matching your SDK.

```powershell
python tests/fixtures/vrs-independent-inputs/prepare.py . "C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxc.exe"
$env:VICTORY_D3D12_DEBUG = '1'
Remove-Item Env:VICTORY_D3D12_WARP -ErrorAction SilentlyContinue
cargo +stable test --locked -p victory-gpu --features diagnostics --lib au_independent_vrs_inputs -- --nocapture --test-threads=1
```

The expected per-pixel group sizes are 1×1 for both passthrough, 2×1 for primitive override, 1×2 for image override, 2×2 for both MAX, and 1×1 after guest Reset with both nondefault sources supplied again. The check compares every pair of the 16 output pixels.

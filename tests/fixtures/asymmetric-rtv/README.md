# Asymmetric clear and readback on PR #3495

This 4×2 fixture clears the entire texture to A, then the two center pixels on its second row to B. It checks all eight raw pixels after a fence, ignoring row-pitch padding. The production guest RTV record and command-list bind/clear paths are used for supported formats. When the resolved view lacks native render-target support, the test logs that decision before binding and has no color result for that view.

| Guest format | A (RGBA) | B (RGBA) |
| --- | --- | --- |
| 115, B4G4R4A4_UNORM | (3/15, 6/15, 12/15, 1) | (12/15, 3/15, 6/15, 9/15) |
| 67, RGB9E5 and format 28 control | (2, 0.75, 0.25, 1) | (0.25, 2, 0.75, 1) |

Run in a checkout of the fixture commit on Windows with the D3D12 debug layer. For the second adapter set `VICTORY_D3D12_WARP=1` and `EIGHT_EXPECT_ADAPTER=Basic Render`. Preserve the console output as `4090.log` or `warp.log` for the decoder.

```powershell
$env:VICTORY_D3D12_DEBUG='1'
$env:EIGHT_EXPECT_ADAPTER='4090 Laptop'
Remove-Item Env:VICTORY_D3D12_WARP -ErrorAction SilentlyContinue
cargo +stable test --locked -p victory-gpu --features diagnostics --lib asymmetric_guest_rtv_readback -- --ignored --nocapture --test-threads=1
python tests/fixtures/asymmetric-rtv/decode.py <folder-containing-both-logs>
```

The Rust test compares literal storage bytes. The companion decoder also computes RGB from the RGB9E5 shared exponent, extracts R/G/B/A nibbles from the 16-bit format and checks the expected channel codes for the UNORM aliases. Format 49 (R8G8_UNORM) stores only R/G. Decoding follows the view actually used for the clear.

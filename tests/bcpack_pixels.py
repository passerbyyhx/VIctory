"""Compare Pillow 11.2.1 BC7 output against the synthetic fixture's intent.

Capture `cargo test -p victory-formats --lib bc7_pixel_fixture -- --nocapture`
as UTF-8 text, then pass its path to this script. The fixture runs the public
BCPack decode entry point. This oracle consumes its BC7 output as a block;
the expected pixels come from the declared endpoints and index arrays.
"""

import argparse
from pathlib import Path

from PIL import Image, __version__


# Microsoft BC7 specification (endpoint expansion and interpolation).
# https://learn.microsoft.com/en-us/windows/win32/direct3d11/bc7-format
# https://learn.microsoft.com/en-us/windows/win32/direct3d11/bc7-format-mode-reference
WEIGHTS = {
    4: (0, 21, 43, 64),
    8: (0, 9, 18, 27, 37, 46, 55, 64),
    16: (0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64),
}
PRECISIONS = {4: (5, 5, 5, 6), 5: (7, 7, 7, 8), 6: (8, 8, 8, 8)}


def interpolate(a, b, weight):
    return ((64 - weight) * a + weight * b + 32) >> 6


def expected_pixels(mode, rotation, selector, endpoints, primary, secondary):
    expanded = []
    for channel, bits in enumerate(PRECISIONS[mode]):
        pair = endpoints[2 * channel : 2 * channel + 2]
        expanded.append(tuple((v << (8 - bits)) | (v >> (2 * bits - 8)) for v in pair))

    weights0 = WEIGHTS[16 if mode == 6 else 4]
    weights1 = WEIGHTS[8 if mode == 4 else 4]
    for idx0, idx1 in zip(primary, secondary, strict=True):
        vector_weight, scalar_weight = weights0[idx0], weights1[idx1]
        if mode == 6:
            scalar_weight = vector_weight
        elif selector:
            vector_weight, scalar_weight = scalar_weight, vector_weight
        rgba = [interpolate(a, b, vector_weight) for a, b in expanded[:3]]
        rgba.append(interpolate(*expanded[3], scalar_weight))
        if rotation:
            rgba[rotation - 1], rgba[3] = rgba[3], rgba[rotation - 1]
        yield tuple(rgba)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture_log", type=Path)
    args = parser.parse_args()
    if __version__ != "11.2.1":
        parser.error(f"expected Pillow 11.2.1, found {__version__}")

    expected_cases = {
        (f"dual{variant}", mode, rotation, selector)
        for variant in range(2)
        for mode in (4, 5)
        for rotation in range(4)
        for selector in range(2 if mode == 4 else 1)
    }
    expected_cases.update((f"guard{bogus}", 6, 0, 0) for bogus in range(4))
    expected_cases.update((f"dict{entry}", 5, 0, 0) for entry in range(2))
    seen = set()
    bad_blocks = 0
    bad_pixels = 0
    print(f"Pillow {__version__}, bcn decoder, BC7")
    for line in args.fixture_log.read_text(encoding="utf-8-sig").splitlines():
        if not line.startswith("BC7PIXELS "):
            continue
        _, label, mode, rotation, selector, endpoints, primary, secondary, block = line.split()
        mode, rotation, selector = int(mode), int(rotation), int(selector)
        case = (label, mode, rotation, selector)
        if case not in expected_cases or case in seen:
            parser.error(f"unexpected or duplicate fixture case {case}")
        seen.add(case)
        endpoints, primary, secondary, block = map(
            bytes.fromhex, (endpoints, primary, secondary, block)
        )
        if (len(endpoints), len(primary), len(secondary), len(block)) != (8, 16, 16, 16):
            parser.error(f"wrong fixture dimensions for {case}")

        actual = list(Image.frombytes("RGBA", (4, 4), block, "bcn", 7).getdata())
        expected = list(expected_pixels(mode, rotation, selector, endpoints, primary, secondary))
        differences = [
            (i, got, want)
            for i, (got, want) in enumerate(zip(actual, expected, strict=True))
            if got != want
        ]
        bad_blocks += bool(differences)
        bad_pixels += len(differences)
        print(f"{case} block={block.hex()} mismatches={len(differences)}/16")
        print(f"  actual   {' '.join(bytes(pixel).hex() for pixel in actual)}")
        print(f"  expected {' '.join(bytes(pixel).hex() for pixel in expected)}")
        for i, got, want in differences:
            print(f"  (x={i % 4}, y={i // 4}) got {got}, want {want}")
    if seen != expected_cases:
        parser.error(f"missing fixture cases: {sorted(expected_cases - seen)}")
    print(f"{bad_blocks}/{len(seen)} blocks and {bad_pixels}/{len(seen) * 16} pixels differ")
    return int(bad_blocks != 0)


if __name__ == "__main__":
    raise SystemExit(main())

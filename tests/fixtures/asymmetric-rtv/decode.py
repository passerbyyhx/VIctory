"""Independently decode the packed pixels from the asymmetric RTV readback logs."""
import ast
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
for adapter, expected_reads in (("4090", 3), ("warp", 7)):
    log = (root / (adapter + ".log")).read_text(encoding="utf-8-sig")
    assert "test result: ok. 1 passed" in log, adapter
    cases = re.findall(r"^guest=(\d+) view=(\w+) rows=(\[.*\])", log, re.M)
    assert len(cases) == expected_reads, (adapter, cases)
    decoded = 0
    for guest, kind, raw in cases:
        guest = int(guest)
        view = {67: 28, 115: 49, 28: 28}[guest] if kind == "alias" else guest
        rows = ast.literal_eval(re.sub(r"\b[0-9a-f]{2}\b", lambda m: str(int(m[0], 16)), raw))
        if view == 67:
            def unpack(b):
                v = int.from_bytes(b, "little")
                return tuple(((v >> shift) & 511) * 2.**((v >> 27)-24) for shift in (0,9,18))
            size, a, b = 4, (2., .75, .25), (.25, 2., .75)
        elif view == 115:
            def unpack(b):
                v = int.from_bytes(b, "little")
                return tuple((v >> shift) & 15 for shift in (8,4,0,12))
            size, a, b = 2, (3,6,12,15), (12,3,6,9)
        elif view == 49:
            size, unpack, a, b = 2, tuple, (51,102), (204,51)
        else:
            size, unpack, a, b = 4, tuple, (255,191,64,255), (64,255,191,255)
        expected = [[a,a,a,a],[a,b,b,a]]
        assert len(rows) == 2 and all(len(row) == size*4 for row in rows), raw
        observed = [[unpack(row[i:i+size]) for i in range(0,len(row),size)] for row in rows]
        assert observed == expected, (adapter,guest,kind,observed)
        if view != 49:
            assert all(p[0] != p[2] for row in observed for p in row)
        decoded += 8
        print(f"{adapter} guest={guest} mode={kind} view={view} pixels=8/8 background={a} inner={b}")
    skipped = len(re.findall(r"^guest=\d+ view=\w+ no draw:",log,re.M))
    assert skipped == (4 if adapter == "4090" else 0)
    print(f"{adapter} total={decoded}/{decoded} original_views_without_RT_support={skipped}")

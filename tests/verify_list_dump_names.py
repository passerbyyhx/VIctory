"""Run the offline CLI against two synthetic vertex shaders with colliding dump labels.

With a Windows SDK installed, run in PowerShell from the directory containing both scripts:
    python verify_list_dump_names.py path/to/fixed/victory.exe "$env:TEMP/victory-check" --packed --jobs 4 --repeat 20

Choose a new output directory outside the repository. For the unmodified parent,
pass --expect-lost. Omit --packed to run the short punctuation case, or use
--long for two different directories with a long common filename.
"""
import argparse
import hashlib
import os
from pathlib import Path
import struct
import subprocess
import zlib

import verify_translate_dedup as fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cli", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--long", action="store_true")
    parser.add_argument("--reverse", action="store_true")
    parser.add_argument("--packed", action="store_true")
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--expect-lost", action="store_true")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True)
    dump = root / "out"
    dump.mkdir()
    base = fixture.native_vertex_program()
    uncompressed = zlib.decompress(base[72:])
    old, new = fixture.words(0x07060504), fixture.words(0x06070405)
    assert uncompressed.count(old) == 1
    changed = uncompressed.replace(old, new)  # second program swizzles the position channels
    stream = zlib.compress(changed)
    blob = b"XBCH" + struct.pack("<IQQ", 3, len(stream), len(changed)) + stream
    second = bytes(24) + b"XDAT" + fixture.words(1, 0x24, 0x0D, 48, len(blob)) + blob
    names = ["pass-a.bin", "pass_a.bin"]
    if args.long:
        names = [f"{prefix}/" + "p" * 100 + ".bin" for prefix in ["one", "two"]]
    sources = [fixture.container(base, "INPUT_A"), fixture.container(second, "INPUT_B")]
    if args.packed:
        # Two records in one valid SGD5 stage list are eligible for separate CLI workers.
        # The distinct offsets and initial letters are outside the old 100-character suffix.
        db = bytearray(b"SGD5" + bytes(16) + fixture.words(2))
        for i, container in enumerate(sources):
            name = bytes([65 + i]) + b"q" * 108
            compressor = zlib.compressobj(wbits=-15)
            data = compressor.compress(container) + compressor.flush()
            size = len(data) + 12
            record = fixture.words(0x000DDCE7, size, len(container)) + data
            db.extend(bytes([len(name) + 1]) + name + b"\0" + bytes([0x32]) + fixture.words(size) + record)
            db.extend(struct.pack("<Q", i + 1) + bytes(0x90) + fixture.words(0))
        db.extend(bytes(24))  # five other stage lists and an empty effect list
        (root / "packed.sgd5").write_bytes(db)
        names = ["packed.sgd5"]
    else:
        for name, container in zip(names, sources):
            path = root / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(container)
    expected = 1 if args.expect_lost else 2
    if args.reverse:
        names.reverse()
    env = dict(os.environ, VICTORY_EMIT_DUMP=str(dump))
    command = [str(args.cli.resolve()), "translate", "--list", "--validate", "--jobs", str(args.jobs), *names]
    first_files = None
    for _ in range(args.repeat):
        result = subprocess.run(command, env=env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=True)
        log = result.stdout.decode("utf-8", "replace")
        (root / "cli.log").write_text(log, encoding="utf-8")
        assert "2 (0 duplicates skipped)" in log, log
        assert "2 validated, 0 pipelines" in log, log
        files = {path.name: path.read_bytes() for path in dump.glob("*.dxil")}
        names_read = [fixture.input_semantic(data) for data in files.values()]
        assert len(files) == expected, files.keys()
        if not args.expect_lost:
            assert set(names_read) == {"INPUT_A", "INPUT_B"}, names_read
        elif not args.packed:
            assert names_read == ["INPUT_A" if args.reverse else "INPUT_B"], names_read
        if first_files is None:
            first_files = files
        elif not args.expect_lost:
            assert first_files == files
    print(f"2 emitted, 2 validated, 0 duplicates; --jobs {args.jobs}, {args.repeat} run(s)")
    for name, data in files.items():
        print(name, fixture.input_semantic(data), len(data), hashlib.sha256(data).hexdigest())


if __name__ == "__main__":
    main()

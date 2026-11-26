"""Verify PR #3003 through the Windows CLI using only Python 3 and the Windows SDK.

Build victory.exe at both revisions. For example, from a VIctory checkout:
    git fetch origin refs/pull/3003/head
    git worktree add --detach ../translate-base 63bcca43c6a00a758510398fa43096ce7e4f7df0
    git worktree add --detach ../translate-head c84b7eefb9d9ad39252303cf7b634686c627f25c
    cargo build --locked -p victory --manifest-path ../translate-base/Cargo.toml
    cargo build --locked -p victory --manifest-path ../translate-head/Cargo.toml
    python tests/verify_translate_dedup.py ../translate-base/target/debug/victory.exe ../translate-head/target/debug/victory.exe

Use independent Cargo target directories. The CLI finds the SDK's dxil.dll
or accepts VICTORY_DXIL. The script finds dxc.exe next to that DLL or on PATH.
Pass --out NEW_DIRECTORY outside the checkout to retain the generated files.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile
import zlib


def words(*values):
    return struct.pack("<" + "I" * len(values), *values)


def native_vertex_program():
    # exp pos0 v4,v5,v6,v7 done; s_endpgm. Input row 0 supplies v4 through v7.
    code = words(0xF80008CF, 0x07060504, 0xBF810000)
    stage = bytearray(0x18C)
    stage[0x24:0x29] = bytes([6, 0, 15, 4, 0])  # generic float4, register 0, v4
    struct.pack_into("<I", stage, 0xC4, 1)       # one vertex input

    # FSSM stage table and its single vertex record, as read by native.rs.
    table = bytearray(0x68)
    struct.pack_into("<Q", table, 0, 6)
    for slot in (0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x48, 0x60):
        struct.pack_into("<Q", table, slot, 0xDEADBEEFDEADBEEF)
    struct.pack_into("<Q", table, 0x28, len(table))
    record = bytearray(0x54)
    struct.pack_into("<QQ", record, 0, len(table) + len(record) + len(stage), len(code))
    struct.pack_into("<I", record, 0x14, 32)     # wave32
    struct.pack_into("<I", record, 0x50, 8)      # 8 VGPRs
    payload = table + record + stage + code
    fssm = b"FSSM" + struct.pack("<IQ", 0x101, len(payload)) + payload
    stream = zlib.compress(fssm)
    blob = b"XBCH" + struct.pack("<IQQ", 3, len(stream), len(fssm)) + stream
    return bytes(24) + b"XDAT" + words(1, 0x24, 0x0D, 48, len(blob)) + blob


def signature(name, system_value=0):
    # Single float4 at row zero. Read/write mask polarity differs for outputs.
    rw_mask = 15 if system_value == 0 else 0
    element = words(0, 40, 0, system_value, 3, 0) + bytes([15, rw_mask, 0, 0]) + words(0)
    return words(1, 8) + element + name.encode("ascii") + b"\0"


def container(payload, input_name):
    parts = [
        (b"DXIL", payload),
        (b"ISG1", signature(input_name)),
        (b"OSG1", signature("SV_Position", system_value=1)),
    ]
    header_size = 32 + 4 * len(parts)
    offsets = []
    body = bytearray()
    for tag, data in parts:
        body.extend(bytes(-len(body) % 4))
        offsets.append(header_size + len(body))
        body.extend(tag + words(len(data)) + data)
    return b"DXBC" + bytes(16) + words(1, header_size + len(body), len(parts), *offsets) + body


def input_semantic(dxil):
    count, = struct.unpack_from("<I", dxil, 28)
    for offset in struct.unpack_from(f"<{count}I", dxil, 32):
        if dxil[offset:offset + 4] == b"ISG1":
            part = offset + 8
            name_offset, = struct.unpack_from("<I", dxil, part + 12)
            return dxil[part + name_offset:].split(b"\0", 1)[0].decode("ascii")
    raise ValueError("Emitted DXIL has no ISG1 part")


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def verify_case(cli, corpus, output, order, jobs, revision, dxc_override):
    dump = output / "dump"
    dump.mkdir(parents=True)
    fixture = output / "fixtures.bin"
    first, second, repeat = (corpus[tag] for tag in order)
    fixture.write_bytes(first + b"gap1" + second + b"gap2" + repeat + b"tail")
    env = dict(os.environ, VICTORY_EMIT_DUMP=str(dump))
    env.pop("VICTORY_PAIRS", None)
    command = [str(cli), "translate", "--list", "--validate", "--jobs", str(jobs), fixture.name]
    run = subprocess.run(command, cwd=output, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    log = run.stdout.decode("utf-8", "replace")
    (output / "cli.log").write_text(log, encoding="utf-8")
    check(run.returncode == 0, log)
    count = 1 if revision == "base" else 2
    check(re.search(rf"containers\s+3 \({3 - count} duplicates? skipped\)", log), log)
    check(re.search(rf"DXIL\s+{count} emitted of {count} vertex and pixel programs; {count} validated, 0 pipelines", log), log)
    validator = re.search(r"^validator\s+([^\r\n]+)", log, re.MULTILINE)
    check(validator, "Validator location missing from CLI log")
    sdk_dxc = Path(validator[1]).with_name("dxc.exe")
    dxc = dxc_override or (sdk_dxc if sdk_dxc.is_file() else shutil.which("dxc.exe"))
    check(dxc, "Could not find dxc.exe. Pass --dxc with the path to the SDK executable.")

    emitted = {}
    for shader in dump.glob("*.dxil"):
        binary = shader.read_bytes()
        name = input_semantic(binary)
        check(name in {"INPUT_A", "INPUT_B"} and name not in emitted, f"Unexpected input {name}")
        emitted[name] = binary
        disasm = subprocess.check_output([str(dxc), "-dumpbin", shader.name], cwd=dump, text=True)
        check(name in disasm and "SV_Position" in disasm, f"Missing signature in {shader}")
        check(disasm.count("call float @dx.op.loadInput.f32") == 4, f"Expected four loads in {shader}")
        check(disasm.count("call void @dx.op.storeOutput.f32") == 4, f"Expected four stores in {shader}")
        (output / f"{name}.txt").write_text(disasm, encoding="utf-8")
    check(len(emitted) == count, f"Expected {count} DXIL files, got {list(emitted)}")
    if revision == "base" and jobs == 1:
        check(list(emitted) == ["INPUT_" + order[0]], "Expected only the first input semantic")
    print(f"{revision} {order} --jobs {jobs}: " + ", ".join(sorted(emitted)))
    return emitted


def compare(args, output):
    payload = native_vertex_program()
    corpus = {tag: container(payload, "INPUT_" + tag) for tag in "AB"}
    reference = {}
    for revision, cli in [("base", args.base), ("head", args.head)]:
        for order, jobs in [("ABA", 1), ("BAB", 1), ("ABA", 4)]:
            folder = output / f"{revision}-{order}-{jobs}"
            emitted = verify_case(cli.resolve(), corpus, folder, order, jobs, revision, args.dxc)
            for name, binary in emitted.items():
                check(reference.setdefault(name, binary) == binary, f"{name} changed across runs")
    check(reference["INPUT_A"] != reference["INPUT_B"], "Both semantics produced identical bytes")
    for name, binary in sorted(reference.items()):
        print(name, hashlib.sha256(binary).hexdigest())


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base", type=Path, help="victory.exe built at the direct parent of PR #3003")
    parser.add_argument("head", type=Path, help="victory.exe built at PR #3003")
    parser.add_argument("--dxc", type=Path, help="optional path to the SDK's dxc.exe")
    parser.add_argument("--out", type=Path, help="keep results in a new directory outside the repository")
    args = parser.parse_args()
    if args.dxc:
        args.dxc = args.dxc.resolve(strict=True)
    if args.out:
        output = args.out.resolve()
        output.mkdir(parents=True)
        compare(args, output)
    else:
        with tempfile.TemporaryDirectory(prefix="victory-translate-") as temp:
            compare(args, Path(temp))


if __name__ == "__main__":
    main()

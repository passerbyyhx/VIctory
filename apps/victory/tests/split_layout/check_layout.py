"""Build tiny native title binaries, then run each directory layout in a fresh VIctory process."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

here = Path(__file__).resolve().parent
p = argparse.ArgumentParser()
p.add_argument("victory", type=Path)
p.add_argument("output", type=Path)
p.add_argument("--baseline", action="store_true")
args = p.parse_args()
binary = args.victory.resolve()
vs = Path(os.environ["ProgramFiles"]) / "Microsoft Visual Studio/2022"
tool = sorted(vs.glob("*/VC/Tools/MSVC/*/bin/Hostx64/x64/cl.exe"))[-1].parent
sdk = Path(os.environ["ProgramFiles(x86)"]) / "Windows Kits/10/Lib"
kernel = sorted(sdk.glob("*/um/x64/kernel32.lib"), key=lambda p: tuple(map(int, p.parents[2].name.split("."))))[-1]
results = []
with tempfile.TemporaryDirectory(prefix="vi-layout-") as temp:
    root = Path(temp)
    def build(*argv):
        subprocess.run(list(map(str, argv)), cwd=root, check=True, capture_output=True)
    for value in (3, 7):
        out = root / str(value)
        out.mkdir()
        build(tool/"cl.exe", "/nologo", "/c", "/GS-", "/O1", f"/DMARKER={value}", here/"marker.c", "/Fomarker.obj")
        build(tool/"link.exe", "/nologo", "/dll", "/noentry", "/nodefaultlib", "/base:0x186000000",
              "/machine:x64", "/out:"+str(out/"layoutmark.dll"), "/implib:layoutmark.lib", "marker.obj")
    build(tool/"cl.exe", "/nologo", "/c", "/GS-", "/O1", here/"layout_probe.c", "/Fotitle.obj")
    build(tool/"link.exe", "/nologo", "/subsystem:console", "/machine:x64", "/entry:entry",
          "/base:0x140000000", "/nodefaultlib", "/out:probe.exe", "title.obj", "layoutmark.lib", kernel)
    # (case, EXE DLL present, EXE manifest present, package DLL present, same folder)
    for case, edll, eman, gdll, same in [
        ("single", True, True, False, True), ("data_only", False, False, True, False),
        ("competing_dll", True, False, True, False), ("competing_manifest", False, True, True, False),
        ("both_compete", True, True, True, False)]:
        run = root / case
        exe = run/"bin"
        data = exe if same else run/"package"
        exe.mkdir(parents=True)
        data.mkdir(exist_ok=True)
        shutil.copyfile(root/"probe.exe", exe/"probe.exe")
        if gdll: shutil.copyfile(root/"7/layoutmark.dll", data/"layoutmark.dll")
        if edll: shutil.copyfile(root/"3/layoutmark.dll", exe/"layoutmark.dll")
        for folder, ident, value in [(data, "AAA00007", 13), (exe, "BBB00003", 11)]:
            (folder/"fixture.bin").write_bytes(bytes([value]))
            if folder != exe or eman:
                (folder/"MicrosoftGame.config").write_text(f"<Game><TitleId>{ident}</TitleId></Game>", encoding="ascii")
        cfg = run/"victory.toml"
        cfg.write_text("[paths]\n", encoding="ascii")
        env = dict(os.environ, VICTORY_CONFIG=str(cfg), VICTORY_GAME_DIR=str(data),
                   VICTORY_CACHE_DIR=str(run/"cache"), VICTORY_DATA_DIR=str(run/"user"))
        proc = subprocess.run([binary, "run", exe/"probe.exe", "--execute"], cwd=run, env=env,
                              text=True, encoding="utf-8", errors="replace", capture_output=True, timeout=90)
        observed = re.search(r"^title\s+([0-9A-F]{8}),", proc.stdout, re.M)
        expected_id = "BBB00003" if eman else "AAA00007"
        expected_code = 16 * (3 if edll else 7) + (11 if eman else 13)
        passed = bool(observed and observed[1] == expected_id and proc.returncode == expected_code
                      and f"title exited with {expected_code}" in proc.stdout)
        normalized = (proc.stdout + proc.stderr).replace(str(root), "<scratch>")
        results.append(dict(case=case, observed_id=observed[1] if observed else None,
                            expected_id=expected_id, code=proc.returncode, expected_code=expected_code,
                            passed=passed, log=normalized))
args.output.write_text(json.dumps(results, indent=2)+"\n", encoding="utf-8")
print([(r["case"], r["observed_id"], r["code"], r["passed"]) for r in results])
assert results[0]["passed"] and (args.baseline or all(r["passed"] for r in results))


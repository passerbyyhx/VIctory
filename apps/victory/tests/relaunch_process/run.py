"""Build two synthetic guest PEs and exercise the unmodified Windows relaunch entry."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

here = Path(__file__).resolve().parent
victory, output = map(lambda s: Path(s).resolve(), sys.argv[1:3])
vs = Path(os.environ["ProgramFiles"]) / "Microsoft Visual Studio/2022"
tool = sorted(vs.glob("*/VC/Tools/MSVC/*/bin/Hostx64/x64/cl.exe"))[-1].parent
sdk = Path(os.environ["ProgramFiles(x86)"]) / "Windows Kits/10/Lib"
kernel = max(sdk.glob("*/um/x64/kernel32.lib"), key=lambda p: tuple(map(int, p.parents[2].name.split("."))))
with tempfile.TemporaryDirectory(prefix="vi-relaunch-") as tmp:
    root = Path(tmp)
    guest = root / "Title"
    guest.mkdir()
    (root/"runtime.def").write_text("LIBRARY xgameruntime.dll\nEXPORTS\n QueryApiImpl\n")
    def build(*args):
        subprocess.run(list(map(str,args)),cwd=root,check=True,capture_output=True)
    build(tool/"lib.exe","/nologo","/machine:x64","/def:runtime.def","/out:runtime.lib")
    for name, flags in [(guest/"Start.exe",[]),(root/"Other.exe",["/DCHILD"])]:
        build(tool/"cl.exe","/nologo","/c","/GS-","/O1",*flags,here/"guest.c","/Foprobe.obj")
        build(tool/"link.exe","/nologo","/subsystem:console","/entry:entry","/nodefaultlib",
              "/base:0x140000000","/machine:x64","/out:"+str(name),"probe.obj","runtime.lib",kernel)
    cfg = root/"victory.toml"
    cfg.write_text("[paths]\n")
    env = dict(os.environ,VICTORY_CONFIG=str(cfg),VICTORY_GAME_DIR=str(root),VICTORY_CACHE_DIR=str(root/"cache"),
               VICTORY_DATA_DIR=str(root/"user"))
    rows = []
    for case,arg in [("absolute",str(guest/"Start.exe")),("relative-mixed",r".\tITLE/staRT.ExE"),
                     ("absolute-mixed",str(guest/"Start.exe").swapcase().replace("\\","/")),
                     ("verbatim","\\\\?\\"+str(guest/"Start.exe"))]:
        proc = subprocess.run([victory,"run",arg,"--execute","--","first-instance"],cwd=root,env=env,
                              capture_output=True,text=True,encoding="utf8",errors="replace",timeout=60)
        log = (proc.stdout+proc.stderr).replace(str(root),"<tmp>")
        markers = re.findall(r"title exited with (\d+)",log)
        rows.append(dict(case=case,path=arg.replace(str(root),"<tmp>"),parent_exit=proc.returncode,guest_codes=markers,log=log))
    for case, path, cookie, expected in [("old-child-control",guest/"Start.exe","child-cookie",41),
                                          ("new-child-control",root/"Other.exe","child-cookie",73),
                                          ("stale-args-control",root/"Other.exe","first-instance",74)]:
        proc = subprocess.run([victory,"run",path,"--execute","--",cookie],cwd=root,env=env,
                              capture_output=True,text=True,encoding="utf8",errors="replace",timeout=60)
        log = (proc.stdout+proc.stderr).replace(str(root),"<tmp>")
        markers = re.findall(r"title exited with (\d+)",log)
        rows.append(dict(case=case,expected=expected,parent_exit=proc.returncode,guest_codes=markers,log=log))
    output.write_text(json.dumps(rows,indent=2)+"\n",encoding="utf8")
    print([(x["case"],x["parent_exit"],x["guest_codes"]) for x in rows])
    sys.exit(int(any(x["parent_exit"] != x.get("expected",0) or x["guest_codes"] != [str(x.get("expected",73))] for x in rows)))

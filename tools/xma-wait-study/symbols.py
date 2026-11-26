"""Resolve local benchmark offsets using its exact archived PDB, plus Windows exports."""
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import sys
import shutil

rev=sys.argv[1]
root=Path(sys.argv[2]).resolve() if len(sys.argv)>2 else Path(__file__).resolve().parent
bin_dir=Path(sys.argv[3]).resolve() if len(sys.argv)>3 else root.parent.parent/"run-six"/"05-audio"/"bin"
exe=bin_dir/(rev+"-time.exe")
scratch=root/("symbol-"+rev)
scratch.mkdir(exist_ok=True)
image=exe.read_bytes()
i=image.find(b"RSDS")
assert i>=0
pdb_name=Path(image[i+24:image.index(b"\0",i+24)].decode()).name
shutil.copyfile(exe.with_suffix(".pdb"),scratch/pdb_name)

u64=c.c_uint64
u32=c.c_uint32
class Symbol(c.Structure):
    _fields_=[("size",u32),("type",u32),("reserved",u64*2),("index",u32),("bytes",u32),
              ("base",u64),("flags",u32),("value",u64),("address",u64),("reg",u32),
              ("scope",u32),("tag",u32),("length",u32),("maxlen",u32),("name",c.c_char*1)]
class Line(c.Structure):
    _fields_=[("size",u32),("key",c.c_void_p),("line",u32),("file",c.c_char_p),("address",u64)]

dbg=c.WinDLL("dbghelp",use_last_error=True)
kernel=c.WinDLL("kernel32",use_last_error=True)
kernel.GetCurrentProcess.restype=w.HANDLE
handle=kernel.GetCurrentProcess()
def api(name,ret,types):
    f=getattr(dbg,name);f.argtypes=types;f.restype=ret
    return f
init=api("SymInitializeW",w.BOOL,[w.HANDLE,w.LPCWSTR,w.BOOL])
load=api("SymLoadModuleExW",u64,[w.HANDLE,w.HANDLE,w.LPCWSTR,w.LPCWSTR,u64,u32,c.c_void_p,u32])
lookup=api("SymFromAddr",w.BOOL,[w.HANDLE,u64,c.POINTER(u64),c.POINTER(Symbol)])
lines=api("SymGetLineFromAddr64",w.BOOL,[w.HANDLE,u64,c.POINTER(u32),c.POINTER(Line)])
api("SymSetOptions",u32,[u32])(0x2|0x10|0x200|0x80000)
assert init(handle,str(scratch),False), c.get_last_error()
mods={}
for j,mod in enumerate([exe,Path(os.environ["SystemRoot"])/"System32/ntdll.dll",
                         Path(os.environ["SystemRoot"])/"System32/kernelbase.dll",
                         Path(os.environ["SystemRoot"])/"System32/kernel32.dll"]):
    base=load(handle,None,str(mod),None,0x100000000*(j+1),0,None,0)
    if not base: raise OSError(c.get_last_error(),str(mod))
    mods[mod.name.lower()]=base
tokens=set()
for row in (root/(rev+"-stacks.tsv")).read_text().splitlines():
    tokens.update(row.split("\t")[1:])
out={}
for t in sorted(tokens):
    mod, off=t.rsplit("+",1)
    if mod.lower() not in mods: continue
    addr=mods[mod.lower()]+int(off,16)
    buf=c.create_string_buffer(c.sizeof(Symbol)+2048)
    sym=c.cast(buf,c.POINTER(Symbol))
    sym.contents.size=c.sizeof(Symbol); sym.contents.maxlen=2048
    distance=u64()
    if not lookup(handle,addr,c.byref(distance),sym): continue
    name=c.string_at(c.addressof(buf)+Symbol.name.offset,sym.contents.length).decode(errors="replace")
    ln=Line();ln.size=c.sizeof(Line);disp=u32()
    file=""
    if lines(handle,addr,c.byref(disp),c.byref(ln)) and ln.file:
        file=Path(ln.file.decode(errors="replace")).name+":"+str(ln.line)
    out[t]=[name,distance.value,file]
(root/(rev+"-symbols.json")).write_text(json.dumps(out,indent=1),encoding="utf-8")
own=[v for k,v in out.items() if k.startswith(rev+"-time.exe")]
print(len(own),"symbolized target offsets;",sum(bool(v[2]) for v in own),"with source lines")
assert any("voice_bench" in str(v) for v in own)


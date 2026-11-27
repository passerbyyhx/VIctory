"""Extend the local 40bcd0f fixture with two independently controlled VRS sources."""
from pathlib import Path
import subprocess
import sys

tree = Path(sys.argv[1]).resolve()
here = Path(__file__).resolve().parent
module = Path("crates/victory-gpu/src/pipeline/shading_rate_test.rs")
source = subprocess.check_output(["git","-C",tree,"show","40bcd0fbffbf19a7e0f3342c4e5f6638d83d6e31:"+module.as_posix()]).decode("utf8")
for stage, profile in [("vs","vs_6_4"),("ps","ps_6_4")]:
    output = tree / module.parent / ("au-"+stage+".dxil")
    subprocess.run([sys.argv[2],"-T",profile,"-E",stage+"_main","-Fo",output,here/"inputs.hlsl"],check=True)
source = source[:source.index("fn compile_hlsl(")] + """fn compiled_shader(bytes: &[u8], stage: &'static str, hw: victory_shader::HwStage) -> Arc<Shader> {
""" + source[source.index("    let shader = shader(stage, bytes);"):]
source = source[:source.index("    let vs = compile_hlsl")] + """    let vs = compiled_shader(include_bytes!("au-vs.dxil"), "vertex", victory_shader::HwStage::Vs);
    let ps = compiled_shader(include_bytes!("au-ps.dxil"), "pixel", victory_shader::HwStage::Ps);

""" + source[source.index("    #[repr(C)]"):]
source = source[:source.index("    let draw_quad =")] + source[source.index("    let read_target ="):]
check = source[source.index("    let check_pixels ="):]
check = check[:check.index("\n    };")+len("\n    };")]
end = source[source.rindex("    finish();"):]
source = source[:source.index("    bind();\n    clear();")] + check+"\n"+(here/"cases.rs").read_text()+"\n"+end
source = source.replace("    let native_pipeline = made(pso);","    let _ = made(pso);").replace("use windows::Win32::Graphics::Direct3D::{Fxc::D3DCompile, ID3DBlob};\n","").replace("use windows::core::{GUID, Interface, PCSTR, s};","use windows::core::{GUID, Interface, PCSTR};")
source = source.replace("fn shading_rate_controls_coarse_pixel_rasterization()", "fn au_independent_vrs_inputs()")
(tree/module).write_text(source,encoding="utf8")

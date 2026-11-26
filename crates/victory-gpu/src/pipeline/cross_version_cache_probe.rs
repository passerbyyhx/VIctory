use super::{Shader, Translated, host::GraphicsState, library, make};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};
use victory_shader::{HwStage, emit::HostShader};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{PCSTR, s};

fn shader(stage: &'static str, hw_stage: HwStage, target: PCSTR, source: &str) -> Arc<Shader> {
    let mut blob = None;
    // SAFETY: the source and C strings live for the compilation.
    let code = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            s!("main"),
            target,
            0,
            0,
            &mut blob,
            None,
        )
        .expect("compile synthetic shader");
        let blob = blob.unwrap();
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
            .to_vec()
    };
    let shader = Shader::new(stage, &code);
    let host = HostShader {
        stage: hw_stage,
        dxil: code,
        user_data: Vec::new(),
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        root_uavs: Vec::new(),
        memory: false,
        memory_writes: false,
        view_addresses: false,
        heap: false,
        heap_writes: false,
        view_marks: false,
    };
    assert!(
        shader
            .translated
            .set(Some(Translated {
                host,
                used: Vec::new()
            }))
            .is_ok()
    );
    Arc::new(shader)
}

fn graphics(mask: u32) -> Box<GraphicsState> {
    let mut state = GraphicsState::default();
    state.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
    state.targets.NumRenderTargets = 1;
    state.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
    let keep = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    #[repr(C)]
    struct Depth(u32, D3D12_DEPTH_STENCIL_DESC);
    let depth = Depth(
        11,
        D3D12_DEPTH_STENCIL_DESC {
            DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
            DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
            FrontFace: keep,
            BackFace: keep,
            ..Default::default()
        },
    );
    let sample_mask = [9, mask];
    // SAFETY: both pointers address whole graphics-state subobjects.
    unsafe {
        state.read(11, std::ptr::from_ref(&depth).cast());
        state.read(9, sample_mask.as_ptr().cast());
    }
    Box::new(state)
}

fn drain() {
    let started = Instant::now();
    while make::waiting() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "pipeline worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a dedicated process and paired cache directories"]
fn synthetic_psos_share_the_early_list_across_executables() {
    let phase = std::env::var("TEN_CACHE_PHASE").expect("first, second or verify");
    let cache = PathBuf::from(std::env::var_os("TEN_CACHE_DIR").expect("fresh cache directory"));
    library::use_pipeline_cache(cache.clone());
    let device = &crate::host::host().expect("D3D12 device").device;
    // SAFETY: plain DXGI query for the live device.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device.GetAdapterLuid()).unwrap();
        let desc = adapter.GetDesc1().unwrap();
        let end = desc
            .Description
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(desc.Description.len());
        eprintln!(
            "pid={} phase={phase} name={:?} vendor={:04x} luid={:08x}:{:08x} flags={:x}",
            std::process::id(),
            String::from_utf16_lossy(&desc.Description[..end]),
            desc.VendorId,
            desc.AdapterLuid.HighPart,
            desc.AdapterLuid.LowPart,
            desc.Flags
        );
        assert_eq!(desc.VendorId, 0x10de, "this probe uses the NVIDIA adapter");
    }
    let cs = shader(
        "compute",
        HwStage::Cs,
        s!("cs_5_1"),
        "[numthreads(4, 1, 1)] void main() {}",
    );
    let vs = shader(
        "vertex",
        HwStage::Vs,
        s!("vs_5_1"),
        "float4 main(uint v: SV_VertexID): SV_Position { return float4(float(v & 1),float(v >> 1),0,1); }",
    );
    let ps = shader(
        "pixel",
        HwStage::Ps,
        s!("ps_5_1"),
        "float4 main(): SV_Target0 { return float4(0.2,0.4,0.6,1); }",
    );
    let candidates = [
        (vec![cs], None),
        (vec![vs.clone(), ps.clone()], Some(graphics(u32::MAX))),
        (vec![vs, ps], Some(graphics(0xFFFF_FF00))),
    ];
    let keys = candidates
        .each_ref()
        .map(|(s, g)| make::key(s, g.as_deref()));
    assert!(keys[0] != keys[1] && keys[0] != keys[2] && keys[1] != keys[2]);
    let psos = candidates.map(|(s, g)| {
        Arc::new(make::Pending::new(
            s,
            g,
            #[cfg(feature = "diagnostics")]
            &[],
        ))
    });
    let early_file = cache.join("early-pipelines.txt");
    let (use_idx, before_idx, after_idx, hits, misses) = match phase.as_str() {
        "first" => (0, vec![], vec![0], 0, 1),
        "second" => (1, vec![0], vec![1, 0], 0, 1),
        "verify" => (2, vec![1, 0], vec![2, 1, 0], 2, 1),
        _ => panic!("unknown phase {phase}"),
    };
    let key_lines = |indexes: &[usize]| {
        indexes
            .iter()
            .map(|&i| keys[i].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = std::fs::read_to_string(&early_file).unwrap_or_default();
    let wanted = keys.each_ref().map(|k| library::wanted_early(k));
    eprintln!("keys={keys:?} before={before:?} wanted={wanted:?}");
    assert_eq!(before, key_lines(&before_idx));
    assert_eq!(wanted, std::array::from_fn(|i| before_idx.contains(&i)));

    if phase == "verify" {
        // Offer the cold control first. Only A and B may start before first use.
        for i in [2, 0, 1] {
            make::early(&psos[i]);
        }
        drain();
        assert_eq!(library::EARLY.load(Relaxed), 2);
        assert_eq!(library::LOADED.load(Relaxed), 2);
        assert_eq!(library::MADE.load(Relaxed), 0);
        eprintln!("before-use started_early=2 host_loaded=2 host_made=0");
    }
    let pso = make::ready(&psos[use_idx]).expect("synthetic PSO ready");
    assert_eq!(pso.compute, use_idx == 0);
    drain();
    assert_eq!(library::LOADED.load(Relaxed), hits);
    assert_eq!(library::MADE.load(Relaxed), misses);
    library::save(true);
    let written = std::fs::read_to_string(&early_file).expect("saved early list");
    assert_eq!(written, key_lines(&after_idx));
    let blob = std::fs::metadata(cache.join("host-pipelines.bin")).expect("saved PSO library");
    assert!(blob.len() > 0);
    eprintln!(
        "after={written:?} host_loaded={hits} host_made={misses} library_bytes={}",
        blob.len()
    );
}

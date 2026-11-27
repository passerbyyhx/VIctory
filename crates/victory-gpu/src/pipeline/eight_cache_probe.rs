use super::{Shader, Translated, library};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use victory_shader::{HwStage, emit::HostShader};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D12::*;
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
    translated_shader(stage, hw_stage, code)
}

fn translated_shader(stage: &'static str, hw_stage: HwStage, code: Vec<u8>) -> Arc<Shader> {
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

fn identify_adapter() {
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
            "pid={} name={:?} vendor={:04x} luid={:08x}:{:08x} flags={:x}",
            std::process::id(),
            String::from_utf16_lossy(&desc.Description[..end]),
            desc.VendorId,
            desc.AdapterLuid.HighPart,
            desc.AdapterLuid.LowPart,
            desc.Flags
        );
        let warp = std::env::var_os("VICTORY_D3D12_WARP").is_some();
        assert_eq!(desc.Flags & 2 != 0, warp, "software adapter flag");
        if !warp { assert_eq!(desc.VendorId, 0x10de); }
    }
}

#[repr(C, align(8))]
struct Depth1(u32, D3D12_DEPTH_STENCIL_DESC1);

fn graphics(mask: u32, bounds: bool) -> super::host::GraphicsState {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM,
    };

    let mut state = super::host::GraphicsState::default();
    state.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
    state.targets.NumRenderTargets = 1;
    state.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
    let dsv_format = [16, DXGI_FORMAT_D32_FLOAT.0 as u32];
    let keep = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    let depth = Depth1(
        21,
        D3D12_DEPTH_STENCIL_DESC1 {
            DepthEnable: true.into(),
            DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
            DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
            DepthBoundsTestEnable: bounds.into(),
            FrontFace: keep,
            BackFace: keep,
            ..Default::default()
        },
    );
    let sample_mask = [9, mask];
    // SAFETY: each pointer addresses its complete graphics-state subobject.
    unsafe {
        state.read(21, std::ptr::from_ref(&depth).cast());
        state.read(16, dsv_format.as_ptr().cast());
        state.read(9, sample_mask.as_ptr().cast());
    }
    state
}

#[test]
#[ignore = "uses dedicated processes and an on-disk D3D12 pipeline library"]
fn stream_padding_persistence() {
    let cache = PathBuf::from(std::env::var_os("EIGHT_CACHE_DIR").unwrap());
    let phase = std::env::var("EIGHT_CACHE_PHASE").unwrap();
    let kind = std::env::var("EIGHT_CACHE_KIND").unwrap();
    let expectation = std::env::var("EIGHT_CACHE_EXPECT").unwrap();
    eprintln!("phase={phase} kind={kind} cache={cache:?} expectation={expectation}");
    identify_adapter();
    let host = crate::host::host().unwrap();
    let mesh = match kind.as_str() {
        "mesh" => true,
        "vertex" => false,
        _ => panic!("unexpected pipeline kind"),
    };
    if (mesh && !host.mesh_shaders()) || (!mesh && !host.depth_bounds_supported) {
        eprintln!("UNSUPPORTED kind={kind} mesh_supported={} depth_bounds={}", host.mesh_shaders(), host.depth_bounds_supported);
        assert!(std::env::var_os("VICTORY_D3D12_WARP").is_some());
        return;
    }
    let existing = cache.join("host-pipelines.bin");
    assert_eq!(existing.is_file(), phase != "cold");
    library::use_pipeline_cache(cache);
    let first = if mesh {
        let path = std::env::var_os("EIGHT_CACHE_MS").unwrap();
        translated_shader("mesh", HwStage::Ms, std::fs::read(path).unwrap())
    } else {
        shader(
            "vertex",
            HwStage::Vs,
            s!("vs_5_1"),
            "float4 main(uint v: SV_VertexID): SV_Position { return float4(float(v & 1),float(v >> 1),0,1); }",
        )
    };
    let ps = if mesh {
        let path = std::env::var_os("EIGHT_CACHE_MS_PS").unwrap();
        translated_shader("pixel", HwStage::Ps, std::fs::read(path).unwrap())
    } else {
        shader(
            "pixel",
            HwStage::Ps,
            s!("ps_5_1"),
            "float4 main(): SV_Target0 { return float4(0.2,0.4,0.6,1); }",
        )
    };
    let mask = if phase == "changed-field" { 0xffff_ff00 } else { u32::MAX };
    let state = graphics(mask, !mesh);
    let shaders = [first, ps];
    let pso = super::host::HostPipeline::new(&shaders, Some(&state)).expect("real stream PSO");
    assert_eq!(pso.mesh, mesh);
    assert!(!pso.compute);
    let (name, loaded) = library::LAST.take().unwrap();
    let hits = library::LOADED.load(Relaxed);
    let misses = library::MADE.load(Relaxed);
    assert_eq!((hits, misses), (u64::from(loaded), u64::from(!loaded)));
    match expectation.as_str() {
        "load" => assert!(loaded),
        "make" => assert!(!loaded),
        "observe" => {}
        _ => panic!("unexpected expectation"),
    }
    library::save(true);
    let bytes = std::fs::metadata(existing).unwrap().len();
    assert!(bytes > 0);
    eprintln!("library_name={name} host_loaded={hits} host_made={misses} library_bytes={bytes}");
    // SAFETY: the device is live until process exit.
    assert!(unsafe { host.device.GetDeviceRemovedReason() }.is_ok());
}

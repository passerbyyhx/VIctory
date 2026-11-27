use super::super::{
    Bytecode, PIPELINE_IIDS, StreamDesc, create_stream, host_pipeline, read_stream, shader,
};
use super::*;
use crate::host::{Host, transition};
use crate::object::{S_OK, This, slot};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};
use windows::core::s;

const FORMATS: [DXGI_FORMAT; 3] = [
    DXGI_FORMAT_R8G8B8A8_UINT,
    DXGI_FORMAT_R8G8B8A8_SINT,
    DXGI_FORMAT_R8G8B8A8_UNORM,
];
const SOURCE: &str = r#"
float4 vs(uint id : SV_VertexID) : SV_Position {
    return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.25, 1);
}
struct Colors {
    uint4 u : SV_Target0;
    int4 s : SV_Target1;
    float4 n : SV_Target2;
};
Colors ps() {
    Colors color;
    color.u = uint4(101, 102, 103, 104);
    color.s = int4(-101, -102, -103, -104);
    color.n = float4(64.0, 64.0, 64.0, 64.0) / 255.0;
    return color;
}
"#;

fn diagnostics(host: &Host) -> ID3D12InfoQueue {
    assert_eq!(std::env::var("VICTORY_D3D12_DEBUG").as_deref(), Ok("1"));
    // SAFETY: queries on a live device and DXGI adapter.
    let desc = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(host.device.GetAdapterLuid())
            .unwrap();
        adapter.GetDesc1().unwrap()
    };
    let name = String::from_utf16_lossy(&desc.Description);
    let name = name.trim_end_matches('\0');
    assert!(
        name.contains(&std::env::var("EIGHT_EXPECT_ADAPTER").unwrap()),
        "{name}"
    );
    println!(
        "adapter={name} vendor={:04x} device={:04x} luid={:08x}:{:08x} flags={:#x}",
        desc.VendorId,
        desc.DeviceId,
        desc.AdapterLuid.HighPart,
        desc.AdapterLuid.LowPart,
        desc.Flags
    );
    let queue = host
        .device
        .cast::<ID3D12InfoQueue>()
        .expect("debug layer info queue");
    println!("info_queue=active");
    queue
}

fn messages(queue: &ID3D12InfoQueue, label: &str) -> u64 {
    // SAFETY: each message uses a buffer with the measured size and native alignment.
    unsafe {
        let n = queue.GetNumStoredMessages();
        for i in 0..n {
            let mut size = 0;
            queue.GetMessage(i, None, &mut size).unwrap();
            let mut words = vec![0usize; size.div_ceil(size_of::<usize>())];
            let message = words.as_mut_ptr().cast::<D3D12_MESSAGE>();
            queue.GetMessage(i, Some(message), &mut size).unwrap();
            let m = &*message;
            let desc = std::ffi::CStr::from_ptr(m.pDescription.cast()).to_string_lossy();
            println!(
                "{label} message severity={} id={} {desc}",
                m.Severity.0, m.ID.0
            );
        }
        println!("{label} messages={n}");
        queue.ClearStoredMessages();
        n
    }
}

fn compiled(
    stage: &'static str,
    hw_stage: victory_shader::HwStage,
    entry: PCSTR,
    model: PCSTR,
) -> Arc<Shader> {
    let mut code = None;
    // SAFETY: source and names are live and code is the output pointer.
    unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("eight_mrt"),
            None,
            None,
            entry,
            model,
            0,
            0,
            &mut code,
            None,
        )
        .unwrap()
    };
    let code = code.unwrap();
    // SAFETY: the code blob owns its bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
    };
    let shader = shader(stage, bytes);
    shader.translated.get_or_init(|| {
        Some(Translated {
            host: victory_shader::emit::HostShader {
                stage: hw_stage,
                dxil: bytes.to_vec(),
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
            },
            used: Vec::new(),
        })
    });
    shader
}

fn write<T>(stream: &mut [u8], serial_offset: usize, kind: u32, data_offset: usize, value: T) {
    stream[serial_offset..serial_offset + 4].copy_from_slice(&kind.to_le_bytes());
    let at = serial_offset + data_offset;
    assert!(at + size_of::<T>() <= stream.len());
    // SAFETY: this writes the native payload to the designated fully sized subobject.
    unsafe {
        stream
            .as_mut_ptr()
            .add(at)
            .cast::<T>()
            .write_unaligned(value)
    };
}

fn pipeline(
    vs: &Shader,
    ps: &Shader,
    independent: bool,
    blend_control: bool,
    parent: bool,
) -> This {
    let mut stream = [0u8; 536];
    let bytecode = |shader: &Shader| Bytecode {
        code: shader.container.as_ptr(),
        size: shader.container.len(),
    };
    write(&mut stream, 0, 1, 8, bytecode(vs));
    write(&mut stream, 24, 2, 8, bytecode(ps));
    let mut g = GraphicsState::default();
    g.blend.IndependentBlendEnable = independent.into();
    for (i, target) in g.blend.RenderTarget[..3].iter_mut().enumerate() {
        target.BlendEnable = (i != 2 || !blend_control).into();
        target.DestBlend = D3D12_BLEND_ONE;
        target.DestBlendAlpha = D3D12_BLEND_ONE;
        target.RenderTargetWriteMask = [5, 10, 6][i];
    }
    g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
    g.depth.DepthEnable = false.into();
    g.targets.NumRenderTargets = 3;
    g.targets.RTFormats[..3].copy_from_slice(&FORMATS);
    write(&mut stream, 48, 8, 4, g.blend);
    write(&mut stream, 384, 10, 4, g.rasterizer);
    write(&mut stream, 432, 11, 4, g.depth);
    write(
        &mut stream,
        488,
        14,
        4,
        D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
    );
    write(&mut stream, 496, 15, 4, g.targets);

    let masks = if independent { [5, 10, 6] } else { [5; 3] };
    let blend = |b: &D3D12_BLEND_DESC| {
        b.RenderTarget[..3]
            .iter()
            .map(|t| t.BlendEnable.as_bool())
            .collect::<Vec<_>>()
    };
    let write_masks = |b: &D3D12_BLEND_DESC| {
        b.RenderTarget[..3]
            .iter()
            .map(|t| t.RenderTargetWriteMask)
            .collect::<Vec<_>>()
    };
    let (_, parsed) = read_stream(&stream, |_, _| None);
    let effective = host_blend(&parsed.blend, &parsed.targets);
    println!(
        "case independent={independent} float_blend_control={blend_control} parent={parent} parsed_independent={} parsed_blend={:?} parsed_masks={:?} host_blend={:?} host_masks={:?}",
        parsed.blend.IndependentBlendEnable.as_bool(),
        blend(&parsed.blend),
        write_masks(&parsed.blend),
        blend(&effective),
        write_masks(&effective)
    );
    let host_expected = [false, false, !blend_control];
    let parsed_expected = if parent {
        [true, true, !blend_control]
    } else {
        host_expected
    };
    assert_eq!(blend(&parsed.blend), parsed_expected);
    assert_eq!(
        parsed.blend.IndependentBlendEnable.as_bool(),
        independent || !parent
    );
    assert_eq!(
        write_masks(&parsed.blend),
        if parent { [5, 10, 6] } else { masks }
    );
    assert_eq!(blend(&effective), host_expected);
    assert_eq!(write_masks(&effective), masks);

    let desc = StreamDesc {
        size: stream.len(),
        stream: stream.as_ptr(),
    };
    let mut pso = std::ptr::null();
    assert_eq!(
        create_stream(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut pso),
        S_OK
    );
    pso
}

fn draw(host: &Host, pso: This) -> [Vec<[u8; 4]>; 3] {
    let started = std::time::Instant::now();
    let pipeline = loop {
        if let Some(pipeline) = host_pipeline(pso) {
            break pipeline;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "D3D12 PSO was not made"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let list = host.new_list().unwrap();
    let mut views = [D3D12_CPU_DESCRIPTOR_HANDLE::default(); 3];
    let resources = FORMATS.map(|format| {
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 4,
            Height: 2,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            ..Default::default()
        };
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let mut tex = None;
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut bytes = 0;
        // SAFETY: creation and footprint query use valid descriptions and outputs.
        unsafe {
            host.device
                .CreateCommittedResource(
                    &heap,
                    D3D12_HEAP_FLAG_NONE,
                    &desc,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    None,
                    &mut tex,
                )
                .unwrap();
            host.device.GetCopyableFootprints(
                &desc,
                0,
                1,
                0,
                Some(&mut footprint),
                None,
                None,
                Some(&mut bytes),
            );
        }
        let (readback, data) = list.readback(host, bytes).unwrap();
        // SAFETY: this is the mapped region for this readback.
        unsafe { data.write_bytes(0xa5, bytes as usize) };
        (tex.unwrap(), footprint, readback, data)
    });
    let commands = &list.list;
    for (i, (texture, ..)) in resources.iter().enumerate() {
        views[i] = host.rtv(texture, None);
        let clear = if i == 2 {
            [64.0 / 255.0; 4]
        } else {
            [11.0, 22.0, 33.0, 44.0]
        };
        // SAFETY: this texture starts in render target state and its RTV stays live.
        unsafe { commands.ClearRenderTargetView(views[i], &clear, None) };
    }
    // SAFETY: the list is open, every RTV matches the PSO, and transfer locations borrow live resources.
    unsafe {
        commands.OMSetRenderTargets(3, Some(views.as_ptr()), false, None);
        commands.RSSetViewports(&[D3D12_VIEWPORT {
            Width: 4.0,
            Height: 2.0,
            MaxDepth: 1.0,
            ..Default::default()
        }]);
        commands.RSSetScissorRects(&[RECT {
            left: 1,
            top: 0,
            right: 3,
            bottom: 1,
        }]);
        commands.SetGraphicsRootSignature(&pipeline.root);
        commands.SetPipelineState(&pipeline.pso);
        commands.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        commands.DrawInstanced(3, 1, 0, 0);
        for (tex, footprint, readback, _) in &resources {
            commands.ResourceBarrier(&[transition(
                tex,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(tex),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                ..Default::default()
            };
            let to = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(readback),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: *footprint,
                },
            };
            commands.CopyTextureRegion(&to, 0, 0, 0, &from, None);
        }
    }
    list.close();
    host.execute(&[&list]);
    // SAFETY: the fence completed and the three readbacks remain mapped and live.
    unsafe {
        host.device.GetDeviceRemovedReason().unwrap();
        resources.map(|(_, footprint, _, data)| {
            (0..8)
                .map(|i| {
                    data.add(
                        footprint.Offset as usize
                            + (i / 4) * footprint.Footprint.RowPitch as usize
                            + (i % 4) * 4,
                    )
                    .cast::<[u8; 4]>()
                    .read()
                })
                .collect()
        })
    }
}

#[test]
#[ignore = "requires the Windows D3D12 debug layer and a selected adapter"]
fn integer_mrt_masks_are_visible_in_pixels() {
    let host = host().expect("D3D12 device");
    let queue = diagnostics(host);
    messages(&queue, "initialization");
    let parent = std::env::var("EIGHT_EXPECT_PARENT").as_deref() == Ok("1");
    let vs = compiled(
        "vertex",
        victory_shader::HwStage::Vs,
        s!("vs"),
        s!("vs_5_1"),
    );
    let ps = compiled("pixel", victory_shader::HwStage::Ps, s!("ps"), s!("ps_5_1"));
    for (independent, control) in [(false, false), (true, false), (true, true)] {
        let pso = pipeline(&vs, &ps, independent, control, parent);
        let pixels = draw(host, pso);
        for (i, (format, values)) in FORMATS.iter().zip(&pixels).enumerate() {
            println!(
                "case independent={independent} float_blend_control={control} rt={i} format={} pixels={values:02x?}",
                format.0
            );
        }
        assert_eq!(messages(&queue, "pipeline and draw"), 0);
        let mut expected = [[[11, 22, 33, 44]; 8], [[11, 22, 33, 44]; 8], [[64; 4]; 8]];
        for i in [1, 2] {
            expected[0][i] = [101, 22, 103, 44];
            expected[1][i] = if independent {
                [11, -102i8 as u8, 33, -104i8 as u8]
            } else {
                [-101i8 as u8, 22, -103i8 as u8, 44]
            };
            expected[2][i] = if control {
                [64; 4]
            } else if independent {
                [64, 128, 128, 64]
            } else {
                [128, 64, 128, 64]
            };
        }
        for (actual, expected) in pixels.iter().zip(expected) {
            assert_eq!(actual, &expected);
        }
        assert_eq!(slot::<extern "system" fn(This) -> u32>(pso, 0x10)(pso), 0);
    }
}

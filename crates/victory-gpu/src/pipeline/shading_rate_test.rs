//! Synthetic shaders, color target and readback verifying `RSSetShadingRate`
//! through the title's command list entry points.

use super::*;
use crate::object::{S_OK, slot};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{Fxc::D3DCompile, ID3DBlob};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};
use windows::core::{GUID, Interface, PCSTR, s};

static DEBUG_ERRORS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HEAP_PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static MAKING_HEAPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "system" fn debug_message(
    _: D3D12_MESSAGE_CATEGORY,
    severity: D3D12_MESSAGE_SEVERITY,
    id: D3D12_MESSAGE_ID,
    description: PCSTR,
    _: *mut std::ffi::c_void,
) {
    if severity.0 <= D3D12_MESSAGE_SEVERITY_ERROR.0 {
        let description = unsafe { description.to_string() }.unwrap_or_default();
        if id.0 == 645
            && MAKING_HEAPS.load(std::sync::atomic::Ordering::Relaxed)
            && description.contains("NumDescriptors 2097152")
            && description.contains("1000000")
        {
            HEAP_PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            eprintln!("D3D12 setup probe {}: {description}", id.0);
            return;
        }
        DEBUG_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!("D3D12 error {}: {}", id.0, description);
    }
}

fn compile_hlsl(
    source: &[u8],
    profile: PCSTR,
    stage: &'static str,
    hw: victory_shader::HwStage,
) -> Arc<Shader> {
    let mut code: Option<ID3DBlob> = None;
    // SAFETY: `source` and `profile` are valid and `code` is the output pointer.
    unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            s!("main"),
            profile,
            0,
            0,
            &mut code,
            None,
        )
        .unwrap();
    }
    let code = code.unwrap();
    // SAFETY: the blob owns these bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
    };
    let shader = shader(stage, bytes);
    shader.translated.get_or_init(|| {
        Some(Translated {
            host: victory_shader::emit::HostShader {
                stage: hw,
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

fn made(pso: This) -> &'static host::HostPipeline {
    let started = std::time::Instant::now();
    loop {
        if let Some(p) = host_pipeline(pso) {
            return p;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the host refused the shading-rate test pipeline, or did not make it in a minute"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn shading_rate_controls_coarse_pixel_rasterization() {
    let debug_requested =
        std::env::var("VICTORY_D3D12_DEBUG").as_deref() == Ok("1") && cfg!(feature = "diagnostics");
    let Some(host) = crate::host::host() else {
        assert!(!debug_requested, "selected D3D12 host unavailable");
        eprintln!("skipped shading rate render: no host D3D12 device");
        return;
    };
    let device = &host.device;
    // SAFETY: plain adapter query on a live device.
    let adapter = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device.GetAdapterLuid()).unwrap();
        adapter.GetDesc1().unwrap()
    };
    let name = String::from_utf16_lossy(&adapter.Description);
    let queue: Option<ID3D12InfoQueue1> =
        debug_requested.then(|| device.cast().expect("active D3D12 Debug Layer"));
    let luid = adapter.AdapterLuid;
    let warp = std::env::var("VICTORY_D3D12_WARP").as_deref() == Ok("1");
    assert_eq!(adapter.Flags & 2 != 0, warp);
    let mut options6 = D3D12_FEATURE_DATA_D3D12_OPTIONS6::default();
    let mut options10 = D3D12_FEATURE_DATA_D3D12_OPTIONS10::default();
    let sum_query = unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS6,
                (&raw mut options6).cast(),
                size_of_val(&options6) as u32,
            )
            .unwrap();
        device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS10,
            (&raw mut options10).cast(),
            size_of_val(&options10) as u32,
        )
    };
    let tier = options6.VariableShadingRateTier.0 as u32;
    let additional = options6.AdditionalShadingRatesSupported.as_bool();
    let sum = sum_query.is_ok() && options10.VariableRateShadingSumCombinerSupported.as_bool();
    eprintln!(
        "shading rate on {}: tier={tier}, additional={}, tile={}, sum={sum}, options10={sum_query:?}, luid={:08x}:{:08x}, flags={:#x}, debug={}",
        name.trim_end_matches('\0'),
        additional,
        options6.ShadingRateImageTileSize,
        luid.HighPart as u32,
        luid.LowPart,
        adapter.Flags,
        queue.is_some()
    );
    let mut cookie = 0;
    if let Some(queue) = &queue {
        unsafe {
            queue
                .RegisterMessageCallback(
                    Some(debug_message),
                    D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                    std::ptr::null_mut(),
                    &mut cookie,
                )
                .unwrap();
        }
    }
    let finish = || {
        if let Some(queue) = &queue {
            unsafe { queue.UnregisterMessageCallback(cookie).unwrap() };
            let errors = DEBUG_ERRORS.load(std::sync::atomic::Ordering::Relaxed);
            let probes = HEAP_PROBES.load(std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "D3D12 Debug Layer errors={errors}; separately recorded setup probes={probes}"
            );
            assert_eq!(errors, 0);
        }
        unsafe { device.GetDeviceRemovedReason() }.unwrap();
    };

    let vs = compile_hlsl(
        b"float4 main(uint id : SV_VertexID) : SV_Position {
            return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1);
        }",
        s!("vs_5_1"),
        "vertex",
        victory_shader::HwStage::Vs,
    );
    let ps = compile_hlsl(
        b"float4 main(float4 pos : SV_Position) : SV_Target {
            return float4(pos.x, pos.y, 0.0, 1.0);
        }",
        s!("ps_5_1"),
        "pixel",
        victory_shader::HwStage::Ps,
    );

    #[repr(C)]
    struct RtvFormats {
        kind: u32,
        formats: [u32; 8],
        count: u32,
    }
    #[repr(C)]
    struct Stream {
        vs_kind: u64,
        vs: Bytecode,
        ps_kind: u64,
        ps: Bytecode,
        topology: [u32; 2],
        rtv: RtvFormats,
    }
    let stream = Stream {
        vs_kind: 1,
        vs: Bytecode {
            code: vs.container.as_ptr(),
            size: vs.container.len(),
        },
        ps_kind: 2,
        ps: Bytecode {
            code: ps.container.as_ptr(),
            size: ps.container.len(),
        },
        topology: [14, D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE.0 as u32],
        rtv: RtvFormats {
            kind: 15,
            formats: [DXGI_FORMAT_R32G32B32A32_FLOAT.0 as u32, 0, 0, 0, 0, 0, 0, 0],
            count: 1,
        },
    };
    let desc = StreamDesc {
        size: size_of_val(&stream),
        stream: (&raw const stream).cast(),
    };
    let null = std::ptr::null();
    let mut pso = null;
    assert_eq!(
        create_stream(null, &desc, &PIPELINE_IIDS[0], &mut pso),
        S_OK
    );
    let native_pipeline = made(pso);

    let mut allocator = null;
    let mut list = null;
    let iid = |g: GUID| guid(g.data1, g.data2, g.data3, g.data4);
    let title_heap = |kind| {
        let desc = [kind, 8u32, 1, 0, 0, 0, 0, 0];
        let mut heap = null;
        assert_eq!(
            crate::descriptor::create_heap(
                null,
                desc.as_ptr().cast(),
                &iid(ID3D12DescriptorHeap::IID),
                &mut heap
            ),
            S_OK
        );
        heap
    };
    MAKING_HEAPS.store(true, std::sync::atomic::Ordering::Relaxed);
    let title_heaps = [title_heap(0), title_heap(1)];
    let native_heaps = title_heaps.map(|h| {
        Some(
            crate::descriptor::heap(h)
                .host()
                .expect("shader-visible heap")
                .heap
                .clone(),
        )
    });
    MAKING_HEAPS.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        crate::list::create_allocator(null, 0, &iid(ID3D12CommandAllocator::IID), &mut allocator),
        S_OK
    );
    assert_eq!(
        crate::list::create_list(
            null,
            0,
            0,
            allocator,
            null,
            &iid(ID3D12GraphicsCommandList::IID),
            &mut list
        ),
        S_OK
    );
    let commands = || crate::list::host_list(list).unwrap();
    let newer = crate::list::has_newer_table(list);
    let legacy = std::env::var("EIGHT_3511_PARENT").as_deref() == Ok("1");
    let rate_slot = if newer { 0x3f0 } else { 0x3e0 };
    let set_rate = |list, rate, combiners| {
        if legacy {
            slot::<extern "system" fn(This)>(list, rate_slot)(list);
        } else {
            slot::<extern "system" fn(This, u32, *const u32)>(list, rate_slot)(
                list, rate, combiners,
            );
        }
    };
    eprintln!("title slot={rate_slot:#x} parent={legacy}");
    let scissors = slot::<extern "system" fn(This, u32, *const RECT)>(list, 0xb0);
    let draw = slot::<extern "system" fn(This, u32, u32, u32, u32)>(list, 0x60);

    let rt_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: 4,
        Height: 4,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        ..Default::default()
    };
    let buffer_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: 256 * 4,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: rt_desc.SampleDesc,
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let resource = |desc: &D3D12_RESOURCE_DESC, kind, state| {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: kind,
            ..Default::default()
        };
        let mut result: Option<ID3D12Resource> = None;
        // SAFETY: valid resource descriptions and output pointer.
        unsafe {
            device
                .CreateCommittedResource(
                    &heap,
                    D3D12_HEAP_FLAG_NONE,
                    desc,
                    state,
                    None,
                    &mut result,
                )
                .unwrap();
        }
        result.unwrap()
    };
    let target = resource(
        &rt_desc,
        D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_STATE_RENDER_TARGET,
    );
    let readback = resource(
        &buffer_desc,
        D3D12_HEAP_TYPE_READBACK,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    let rtv = host.rtv(&target, None);
    let clear = || unsafe {
        commands()
            .list
            .ClearRenderTargetView(rtv, &[-19.0; 4], None)
    };

    let bind = || {
        slot::<extern "system" fn(This, u32, *const This)>(list, 0x100)(
            list,
            2,
            title_heaps.as_ptr(),
        );
        // SAFETY: an open list and a live render-target descriptor.
        unsafe {
            commands()
                .list
                .OMSetRenderTargets(1, Some(&rtv), false, None)
        };
        slot::<extern "system" fn(This, This)>(list, 0xc8)(list, pso);
        slot::<extern "system" fn(This, u32)>(list, 0xa0)(list, 4); // triangle list
        slot::<extern "system" fn(This, u32, *const D3D12_VIEWPORT)>(list, 0xa8)(
            list,
            1,
            &D3D12_VIEWPORT {
                Width: 4.0,
                Height: 4.0,
                MaxDepth: 1.0,
                ..Default::default()
            },
        );
    };
    let draw_quad = |x: i32, y: i32| {
        scissors(
            list,
            1,
            &RECT {
                left: x,
                top: y,
                right: x + 2,
                bottom: y + 2,
            },
        );
        draw(list, 3, 1, 0, 0);
    };
    let read_target = || -> [[[f32; 4]; 4]; 4] {
        let l = &commands().list;
        let mut from = D3D12_TEXTURE_COPY_LOCATION {
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            ..Default::default()
        };
        let mut to = D3D12_TEXTURE_COPY_LOCATION {
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: rt_desc.Format,
                        Width: 4,
                        Height: 4,
                        Depth: 1,
                        RowPitch: 256,
                    },
                },
            },
            ..Default::default()
        };
        // SAFETY: live resources and exact full-subresource copy.
        unsafe {
            from.pResource = std::mem::transmute_copy(&target);
            to.pResource = std::mem::transmute_copy(&readback);
            l.ResourceBarrier(&[crate::host::transition(
                &target,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            l.CopyTextureRegion(&to, 0, 0, 0, &from, None);
            l.ResourceBarrier(&[crate::host::transition(
                &target,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);
        }
        assert_eq!(
            slot::<extern "system" fn(This) -> HRESULT>(list, 0x48)(list),
            S_OK
        );
        let segments = crate::list::segments(list);
        host.execute(&segments.iter().map(Arc::as_ref).collect::<Vec<_>>());
        let mut mapped = std::ptr::null_mut();
        let mut rows = [[[0.0f32; 4]; 4]; 4];
        // SAFETY: GPU copy completed; buffer has 4 rows at 256-byte stride.
        unsafe {
            readback
                .Map(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: 256 * 4,
                    }),
                    Some(&mut mapped),
                )
                .unwrap();
            for (y, row) in rows.iter_mut().enumerate() {
                *row = mapped
                    .cast::<u8>()
                    .add(y * 256)
                    .cast::<[[f32; 4]; 4]>()
                    .read();
            }
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
        }
        rows
    };

    bind();
    clear();
    // Top-left (0..2, 0..2): fresh list defaults to 1x1.
    draw_quad(0, 0);
    // Top-right (2..4, 0..2): 2x2 with null combiners.
    set_rate(list, 0x5, std::ptr::null());
    draw_quad(2, 0);
    // Bottom-left (0..2, 2..4): 2x1 with null combiners.
    set_rate(list, 0x4, std::ptr::null());
    draw_quad(0, 2);
    // Bottom-right (2..4, 2..4): 1x2 with explicit passthrough combiners.
    let passthrough = [0u32, 0u32];
    set_rate(list, 0x1, passthrough.as_ptr());
    draw_quad(2, 2);
    // Leave a coarse rate active before reset to verify Reset restores 1x1.
    set_rate(list, 0x5, std::ptr::null());
    let pass1 = read_target();
    eprintln!("fresh, 2x2, 2x1, 1x2 quadrants: {pass1:?}");

    let check_pixels = |case: &str, rows: [[[f32; 4]; 4]; 4], x_rate: usize, y_rate: usize| {
        eprintln!("{case}: expected {x_rate}x{y_rate}, actual {rows:?}");
        for (y, row) in rows.iter().enumerate() {
            for (x, px) in row.iter().enumerate() {
                assert_eq!([px[2], px[3]], [0.0, 1.0], "{case}: draw pixel ({x},{y})");
                for (oy, other) in rows.iter().enumerate() {
                    for (ox, opx) in other.iter().enumerate() {
                        assert_eq!(
                            px == opx,
                            x / x_rate == ox / x_rate && y / y_rate == oy / y_rate,
                            "{case}: shading groups ({x},{y}) vs ({ox},{oy})"
                        );
                    }
                }
            }
        }
    };
    if legacy {
        check_pixels("parent: title shading-rate slot is a no-op", pass1, 1, 1);
        finish();
        for object in [list, allocator, pso, title_heaps[0], title_heaps[1]] {
            assert_eq!(
                slot::<extern "system" fn(This) -> u32>(object, 0x10)(object),
                0
            );
        }
        return;
    }

    // Fresh list 1x1 quadrant: every pixel has a distinct SV_Position.
    assert_ne!(pass1[0][0][0], pass1[0][1][0], "fresh 1x1 must separate X");
    assert_ne!(pass1[0][0][1], pass1[1][0][1], "fresh 1x1 must separate Y");

    if tier >= 1 {
        // 2x2: all four pixels share one coarse PS invocation.
        assert_eq!(pass1[0][2][0], pass1[0][3][0], "2x2 must broadcast X");
        assert_eq!(pass1[0][2][1], pass1[1][2][1], "2x2 must broadcast Y");
        assert_eq!(pass1[0][2], pass1[1][3], "2x2 must broadcast across block");
        // 2x1: horizontal pairs match, rows remain distinct.
        assert_eq!(pass1[2][0][0], pass1[2][1][0], "2x1 must broadcast X");
        assert_eq!(
            pass1[3][0][0], pass1[3][1][0],
            "2x1 must broadcast X on row 3"
        );
        assert_ne!(pass1[2][0][1], pass1[3][0][1], "2x1 must keep Y distinct");
        // 1x2: vertical pairs match, columns remain distinct.
        assert_eq!(pass1[2][2][1], pass1[3][2][1], "1x2 must broadcast Y");
        assert_eq!(
            pass1[2][3][1], pass1[3][3][1],
            "1x2 must broadcast Y on col 3"
        );
        assert_ne!(pass1[2][2][0], pass1[2][3][0], "1x2 must keep X distinct");
    } else {
        // Tier 0 ignores RSSetShadingRate cleanly and rasterizes at 1x1.
        assert_ne!(pass1[0][2][0], pass1[0][3][0]);
        assert_ne!(pass1[0][2][1], pass1[1][2][1]);
    }

    // Reset must restore default 1x1 even though 2x2 was set prior to submission.
    assert_eq!(
        slot::<extern "system" fn(This, This, This) -> HRESULT>(list, 0x50)(list, allocator, null),
        S_OK
    );
    bind();
    clear();
    // Top-left (0..2, 0..2): right after Reset, must be 1x1.
    draw_quad(0, 0);
    // Top-right (2..4, 0..2): invalid base rate (0x7) must fall back to 1x1
    // cleanly without tripping the D3D12 debug layer.
    set_rate(list, 0x7, std::ptr::null());
    draw_quad(2, 0);
    // Bottom-left (0..2, 2..4): 2x2 with non-passthrough combiner [PASSTHROUGH, MAX].
    // Tier 1 strips combiners (avoiding debug-layer error); Tier 2 computes MAX(2x2, 1x1) = 2x2.
    let non_passthrough = [0u32, 3u32];
    set_rate(list, 0x5, non_passthrough.as_ptr());
    draw_quad(0, 2);
    let pass2 = read_target();
    eprintln!("reset, invalid rate, max-combiner quadrants: {pass2:?}");

    assert_ne!(pass2[0][0][0], pass2[0][1][0], "reset must restore 1x1 X");
    assert_ne!(pass2[0][0][1], pass2[1][0][1], "reset must restore 1x1 Y");
    assert_ne!(
        pass2[0][2][0], pass2[0][3][0],
        "invalid rate must fall back to 1x1 X"
    );
    assert_ne!(
        pass2[0][2][1], pass2[1][2][1],
        "invalid rate must fall back to 1x1 Y"
    );
    if tier >= 1 {
        assert_eq!(
            pass2[2][0], pass2[3][1],
            "2x2 with [PASSTHROUGH, MAX] must produce 2x2 on Tier 1 and Tier 2"
        );
    } else {
        assert_ne!(pass2[2][0][0], pass2[2][1][0]);
    }

    // When the adapter advertises AdditionalShadingRatesSupported, 4x4 (0xA)
    // must broadcast one coarse PS invocation across the full 4x4 target.
    if tier >= 1 && additional {
        assert_eq!(
            slot::<extern "system" fn(This, This, This) -> HRESULT>(list, 0x50)(
                list, allocator, null
            ),
            S_OK
        );
        bind();
        clear();
        set_rate(list, 0xA, std::ptr::null());
        scissors(
            list,
            1,
            &RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
        );
        draw(list, 3, 1, 0, 0);
        let pass3 = read_target();
        check_pixels("author 4x4", pass3, 4, 4);
        for row in &pass3 {
            for px in row {
                assert_eq!(
                    *px, pass3[0][0],
                    "4x4 must broadcast across the entire 4x4 block"
                );
            }
        }
    }

    let reset = || {
        assert_eq!(
            slot::<extern "system" fn(This, This, This) -> HRESULT>(list, 0x50)(
                list, allocator, null
            ),
            S_OK
        );
    };
    let full = || {
        scissors(
            list,
            1,
            &RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
        )
    };
    let begin = || {
        reset();
        bind();
        clear();
        full();
    };
    let verify = |label, x, y| check_pixels(label, read_target(), x, y);

    if tier >= 1 {
        let coarse = |combiners: [u32; 2]| set_rate(list, 0x5, combiners.as_ptr());
        for (label, combiners, on_tier2) in [
            ("MAX image", [0, 3], 2),
            ("MIN primitive (default source 1x1)", [2, 0], 1),
            ("MIN image (default source 1x1)", [0, 2], 1),
            ("OVERRIDE image (default source 1x1)", [0, 1], 1),
            (
                "invalid image combiner falls back to passthrough",
                [0, 99],
                2,
            ),
        ] {
            begin();
            coarse(combiners);
            draw(list, 3, 1, 0, 0);
            let rate = if tier >= 2 { on_tier2 } else { 2 };
            verify(label, rate, rate);
        }
        if tier >= 2 && sum {
            begin();
            coarse([4, 4]);
            draw(list, 3, 1, 0, 0);
            verify("SUM with default 1x1 sources", 2, 2);
        }

        // The wait path closes the first segment and calls State::cut. Bind
        // only native PSO/RTV/root on the next list so State::bind cannot
        // reapply the rate and hide a missing cut replay. No guest wait is
        // needed here because Host::execute submits the recorded segments.
        begin();
        set_rate(list, 0x4, null.cast::<u32>());
        draw(list, 3, 1, 0, 0);
        clear();
        assert_eq!(crate::list::segments(list).len(), 1);
        crate::list::wait64(list, 0, 0, 0, 0);
        assert_eq!(crate::list::segments(list).len(), 2);
        unsafe {
            let l = &commands().list;
            l.SetDescriptorHeaps(&native_heaps);
            l.SetPipelineState(&native_pipeline.pso);
            l.SetGraphicsRootSignature(&native_pipeline.root);
            l.OMSetRenderTargets(1, Some(&rtv), false, None);
            l.DrawInstanced(3, 1, 0, 0);
        }
        verify("wait split: cut replay isolated from State::bind", 2, 1);

        // A native 1x1 perturbation leaves the title's tracked 2x2 untouched.
        // With or without the rebind notification, the same title draw runs.
        // The paired control ensures 2x2 actually depends on production replay.
        for replay in [false, true] {
            begin();
            coarse([0, 0]);
            draw(list, 3, 1, 0, 0);
            clear();
            let cmd: ID3D12GraphicsCommandList5 = commands().list.cast().unwrap();
            unsafe { cmd.RSSetShadingRate(D3D12_SHADING_RATE_1X1, None) };
            if replay {
                commands().need_rebind();
            }
            draw(list, 3, 1, 0, 0);
            let rate = if replay { 2 } else { 1 };
            verify(
                if replay {
                    "graphics PSO rebind replay"
                } else {
                    "rebind negative control"
                },
                rate,
                rate,
            );
        }
        begin();
        draw(list, 3, 1, 0, 0);
        verify("title Reset restores 1x1 after rebind", 1, 1);
    }

    finish();
    for object in [list, allocator, pso, title_heaps[0], title_heaps[1]] {
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(object, 0x10)(object),
            0
        );
    }
}

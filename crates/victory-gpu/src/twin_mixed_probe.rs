//! Native D3D12 readback of the mixed depth/stencil missing-converter path.
use super::*;
use crate::host::Host;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};
use windows::core::Interface;

fn adapter(host: &Host) {
    // SAFETY: the adapter is resolved from the actual device LUID.
    let d = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let a: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(host.device.GetAdapterLuid())
            .unwrap();
        a.GetDesc1().unwrap()
    };
    let name = String::from_utf16_lossy(&d.Description);
    let name = name.trim_end_matches('\0');
    eprintln!(
        "adapter={name} vendor={:#x} device={:#x} flags={:#x} luid={:08x}:{:08x} debug_queue={}",
        d.VendorId,
        d.DeviceId,
        d.Flags,
        d.AdapterLuid.HighPart,
        d.AdapterLuid.LowPart,
        host.device.cast::<ID3D12InfoQueue>().is_ok()
    );
    if let Ok(required) = std::env::var("VI3_EXPECT_ADAPTER") {
        assert!(
            name.contains(&required),
            "{name} does not contain {required}"
        );
    }
}

fn depth_texture(
    host: &Host,
    console: u32,
    deny_srv: bool,
) -> windows::core::Result<ID3D12Resource> {
    let desc = crate::resource::twin_probe_desc(console, deny_srv);
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let mut texture: Option<ID3D12Resource> = None;
    // SAFETY: a plain descriptor from the production mapping and our out pointer.
    unsafe {
        host.device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &desc,
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut texture,
        )?
    };
    Ok(texture.unwrap())
}

fn footprint(
    host: &Host,
    texture: &ID3D12Resource,
    sub: u32,
) -> (D3D12_PLACED_SUBRESOURCE_FOOTPRINT, usize, u64) {
    let mut f = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let (mut row_bytes, mut total) = (0, 0);
    // SAFETY: a live resource, one footprint, one output per requested quantity.
    unsafe {
        host.device.GetCopyableFootprints(
            &texture.GetDesc(),
            sub,
            1,
            0,
            Some(&mut f),
            None,
            Some(&mut row_bytes),
            Some(&mut total),
        )
    };
    assert_eq!((f.Footprint.Width, f.Footprint.Height), (4, 2));
    (f, row_bytes as usize, total)
}

#[test]
#[ignore = "native format and production twin construction probe"]
fn console_resources() {
    let host = host().expect("native D3D12 host");
    adapter(host);
    if let Some(prior) = host.log_messages() {
        eprintln!("pre-probe debug message (host initialization): {prior}");
    }
    for console in 118..=120 {
        for deny in [false, true] {
            let mapped = crate::resource::twin_probe_desc(console, deny).Format.0;
            let texture = match depth_texture(host, console, deny) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!(
                        "console={console} deny_srv={deny} host={mapped} create={:#010x}",
                        e.code().0
                    );
                    continue;
                }
            };
            let planes = [0, 1].map(|s| {
                let (f, bytes, _) = footprint(host, &texture, s);
                (f.Footprint.Format.0, bytes, f.Footprint.RowPitch)
            });
            let twin = make(console, &texture);
            let routes = twin.as_ref().map(|t| {
                t.routes
                    .iter()
                    .map(|r| (r.plane, r.depth.is_some()))
                    .collect::<Vec<_>>()
            });
            eprintln!(
                "console={console} deny_srv={deny} host={mapped} create=ok footprints(format,row_bytes,pitch)={planes:?} routes(plane,requires_conversion)={routes:?}"
            );
            eprintln!("format_probe_debug_error={:?}", host.log_messages());
        }
    }
    unsafe { host.device.GetDeviceRemovedReason() }.unwrap();
}

fn sub(texture: &ID3D12Resource, index: u32) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: std::mem::ManuallyDrop::new(Some(texture.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: index,
        },
    }
}

fn placed(
    buffer: &ID3D12Resource,
    f: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: std::mem::ManuallyDrop::new(Some(buffer.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: f },
    }
}

fn record_copy(
    l: &ID3D12GraphicsCommandList,
    mut dst: D3D12_TEXTURE_COPY_LOCATION,
    mut src: D3D12_TEXTURE_COPY_LOCATION,
) {
    // SAFETY: whole subresources, no box. Each location owns its cloned COM reference.
    unsafe {
        l.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        std::mem::ManuallyDrop::drop(&mut src.pResource);
        std::mem::ManuallyDrop::drop(&mut dst.pResource);
    }
}

fn read_planes(
    host: &Host,
    list: &HostList,
    resources: &[(&ID3D12Resource, u32); 4],
) -> Vec<(*mut u8, usize, usize)> {
    resources
        .iter()
        .map(|(texture, index)| {
            let (f, row_bytes, total) = footprint(host, texture, *index);
            let (back, ptr) = list.readback(host, total).unwrap();
            record_copy(&list.list, placed(&back, f), sub(texture, *index));
            (ptr, f.Footprint.RowPitch as usize, row_bytes)
        })
        .collect()
}

fn rows(buffers: &[(*mut u8, usize, usize)]) -> Vec<Vec<u8>> {
    buffers
        .iter()
        .map(|&(ptr, pitch, row_bytes)| {
            // SAFETY: the submission has completed and the list owns each mapped allocation.
            (0..2)
                .flat_map(|y| {
                    unsafe { std::slice::from_raw_parts(ptr.add(y * pitch), row_bytes) }
                        .iter()
                        .copied()
                })
                .collect()
        })
        .collect()
}

fn both_planes(console: u32, to_depth: bool, deny: bool) {
    let host = host().expect("native D3D12 host");
    adapter(host);
    if let Some(prior) = host.log_messages() {
        eprintln!("pre-readback debug message (before any fixture resource): {prior}");
    }
    let depth = Arc::new(HostTexture::new(
        depth_texture(host, console, deny).unwrap(),
        COPY_DEST,
        Vec::new(),
    ));
    let twin = make(console, &depth.texture).expect("production mixed twin for console D16+S8");
    assert_eq!(
        twin.routes
            .iter()
            .map(|r| (r.plane, r.depth.is_some()))
            .collect::<Vec<_>>(),
        [(0, true), (1, false)]
    );
    let textures = [&depth, &twin.planes[0], &twin.planes[1]];
    let resources = [
        (&depth.texture, 0),
        (&depth.texture, 1),
        (&twin.planes[0].texture, 0),
        (&twin.planes[1].texture, 0),
    ];
    let depth_words: [f32; 8] = [0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];
    let twin_words: [u16; 8] = [
        0x1357, 0x2468, 0x7f01, 0xabcd, 0xfe02, 0x6789, 0xcafe, 0xbeef,
    ];
    let seeds: [Vec<u8>; 4] = [
        depth_words.iter().flat_map(|v| v.to_le_bytes()).collect(),
        vec![0x10, 0x22, 0x34, 0x46, 0x58, 0x6a, 0x7c, 0x8e],
        twin_words.iter().flat_map(|v| v.to_le_bytes()).collect(),
        vec![0xef, 0xdd, 0xcb, 0xb9, 0xa7, 0x95, 0x83, 0x71],
    ];
    let list = host.new_list().unwrap();
    for p in &twin.planes {
        unsafe {
            list.list.ResourceBarrier(&[transition(
                &p.texture,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )])
        };
    }
    for ((texture, i), seed) in resources.iter().zip(&seeds) {
        let (f, row_bytes, total) = footprint(host, texture, *i);
        assert_eq!(row_bytes * 2, seed.len());
        let (upload, ptr) = list.upload(host, total).unwrap();
        for (y, row) in seed.chunks_exact(row_bytes).enumerate() {
            // SAFETY: the GPU-provided row pitch and footprint size bound both rows.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr(),
                    ptr.add(y * f.Footprint.RowPitch as usize),
                    row_bytes,
                )
            };
        }
        record_copy(&list.list, sub(texture, *i), placed(&upload, f));
    }
    // A legacy copy without the converter reads from an unwritten output offset.
    // Poison scratch so that the negative control observes a deterministic value.
    let scratch_size = unsafe { twin.scratch.GetDesc() }.Width;
    let (poison, ptr) = list.upload(host, scratch_size).unwrap();
    unsafe {
        ptr.write_bytes(0xa5, scratch_size as usize);
        list.list
            .CopyBufferRegion(&twin.scratch, 0, &poison, 0, scratch_size);
        list.list.ResourceBarrier(&[transition(
            &twin.scratch,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_COMMON,
        )]);
    }
    for texture in textures {
        unsafe {
            list.list.ResourceBarrier(&[transition(
                &texture.texture,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )])
        };
    }
    let before = read_planes(host, &list, &resources);
    let mut states = [COPY_SOURCE; 3];
    let mut moved = Vec::new();
    let mut calls = 0;
    let converted = twin.copy_with(
        &list.list,
        &depth,
        to_depth,
        &mut |t, state| {
            let i = textures.iter().position(|p| Arc::ptr_eq(t, p)).unwrap();
            moved.push((i, state));
            let old = std::mem::replace(&mut states[i], state);
            (old != state).then(|| {
                transition(
                    &t.texture,
                    D3D12_RESOURCE_STATES(old as i32),
                    D3D12_RESOURCE_STATES(state as i32),
                )
            })
        },
        || {
            calls += 1;
            None
        },
    );
    for (texture, state) in textures.into_iter().zip(states) {
        if state != COPY_SOURCE {
            unsafe {
                list.list.ResourceBarrier(&[transition(
                    &texture.texture,
                    D3D12_RESOURCE_STATES(state as i32),
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )])
            };
        }
    }
    let after = read_planes(host, &list, &resources);
    assert_eq!(host.log_messages(), None, "debug layer while recording");
    list.close();
    host.execute(&[&list]);
    unsafe { host.device.GetDeviceRemovedReason() }.unwrap();
    let (before, after) = (rows(&before), rows(&after));
    let mut expected = seeds.to_vec();
    let (from, to) = if to_depth { (3, 1) } else { (1, 3) };
    expected[to] = seeds[from].clone();
    eprintln!(
        "console={console} deny_srv={deny} direction={} converter_calls={calls} converted={converted} moves={moved:?} scratch_poison=a5",
        if to_depth {
            "twin-to-host"
        } else {
            "host-to-twin"
        }
    );
    for (i, name) in ["host-depth", "host-stencil", "twin-depth", "twin-stencil"]
        .into_iter()
        .enumerate()
    {
        eprintln!(
            "{name}: before={:02x?} after={:02x?} expected={:02x?}",
            before[i], after[i], expected[i]
        );
    }
    assert_eq!(before, seeds, "all four independent initial readbacks");
    assert_eq!(after[to], expected[to], "direct stencil destination");
    assert_eq!(after, expected, "both depth payloads and stencil source");
    assert_eq!(calls, 1);
    assert!(!converted);
    assert_eq!(
        moved.iter().map(|m| m.0).collect::<Vec<_>>(),
        [2, 0],
        "only the stencil twin and host resource transition"
    );
}

#[test]
#[ignore = "native depth/stencil readback"]
fn host_to_twin() {
    for console in 118..=120 {
        both_planes(console, false, false);
    }
}

#[test]
#[ignore = "native depth/stencil readback"]
fn host_to_twin_deny_srv() {
    for console in 118..=120 {
        both_planes(console, false, true);
    }
}

#[test]
#[ignore = "native depth/stencil readback"]
fn twin_to_host() {
    for console in 118..=120 {
        both_planes(console, true, false);
    }
}

#[test]
#[ignore = "native depth/stencil readback"]
fn twin_to_host_deny_srv() {
    for console in 118..=120 {
        both_planes(console, true, true);
    }
}

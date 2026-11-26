//! Fenced integration probes for the two callers of Twin::copy.
use super::*;
use crate::host::tests::{issued, replay};
use std::{cell::Cell, collections::HashMap};

thread_local! {
    static ABSENT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(in crate::twin) fn missing_converter() -> bool {
    ABSENT.with(|calls| {
        if let Some(n) = calls.get() {
            calls.set(Some(n + 1));
            true
        } else {
            false
        }
    })
}

fn move_to(list: &HostList, texture: &Arc<HostTexture>, state: u32) {
    if let Some(barrier) = list.move_texture(texture, state) {
        unsafe { list.list.ResourceBarrier(&[barrier]) };
    }
}

fn submit(host: &'static Host, list: &HostList) {
    list.close();
    check_diagnostics(host, "recorded");
    list.defer_after(|| check_diagnostics(crate::host::host().unwrap(), "fenced"));
    host.execute(&[list]);
    unsafe { host.device.GetDeviceRemovedReason() }.unwrap();
}

fn use_side(list: &HostList, depth: &Arc<HostTexture>, twin: &Twin, want_depth: bool) {
    let state = if want_depth {
        DEPTH_WRITE
    } else {
        UNORDERED_ACCESS
    };
    let handled = route(list, depth, twin, state);
    assert_eq!(handled, !want_depth);
    if !handled {
        move_to(list, depth, state);
    }
}

fn state_triplet(textures: &[&Arc<HostTexture>; 3]) -> [u32; 3] {
    textures.map(|t| t.state.load(Relaxed))
}

fn run_case(host: &'static Host, prelude: bool, to_depth: bool) {
    let depth = Arc::new(HostTexture::new(
        depth_texture_mips(host, 118, false, 2).unwrap(),
        COPY_DEST,
        Vec::new(),
    ));
    let twin = depth
        .twin
        .get_or_init(|| make(118, &depth.texture))
        .as_ref()
        .unwrap();
    assert_eq!(
        twin.routes
            .iter()
            .map(|r| (r.plane, r.twin_sub, r.depth_sub, r.depth.is_some()))
            .collect::<Vec<_>>(),
        [
            (0, 0, 0, true),
            (0, 1, 1, true),
            (1, 0, 2, false),
            (1, 1, 3, false)
        ]
    );
    let textures = [&depth, &twin.planes[0], &twin.planes[1]];
    let resources = [
        (&depth.texture, 0),
        (&depth.texture, 1),
        (&depth.texture, 2),
        (&depth.texture, 3),
        (&twin.planes[0].texture, 0),
        (&twin.planes[0].texture, 1),
        (&twin.planes[1].texture, 0),
        (&twin.planes[1].texture, 1),
    ];
    let real = [DEPTH_WRITE, UNORDERED_ACCESS, UNORDERED_ACCESS];
    let setup = host.new_list().unwrap();
    let mut seeds = Vec::new();
    for t in textures {
        move_to(&setup, t, COPY_DEST);
    }
    for (i, (texture, index)) in resources.iter().enumerate() {
        let (f, bytes, total) = footprint(host, texture, *index);
        assert_eq!(
            (f.Footprint.Width, f.Footprint.Height),
            if i % 2 == 0 { (4, 2) } else { (2, 1) }
        );
        let (upload, ptr) = setup.upload(host, total).unwrap();
        let seed: Vec<u8> = (0..bytes * f.Footprint.Height as usize)
            .map(|x| i as u8 * 32 + x as u8 + 1)
            .collect();
        for (y, row) in seed.chunks_exact(bytes).enumerate() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr(),
                    ptr.add(y * f.Footprint.RowPitch as usize),
                    bytes,
                );
            }
        }
        record_copy(&setup.list, sub(texture, *index), placed(&upload, f));
        seeds.push(seed);
    }
    let size = unsafe { twin.scratch.GetDesc() }.Width;
    let (upload, ptr) = setup.upload(host, size).unwrap();
    unsafe {
        ptr.write_bytes(0xa5, size as usize);
        setup
            .list
            .CopyBufferRegion(&twin.scratch, 0, &upload, 0, size);
        setup.list.ResourceBarrier(&[transition(
            &twin.scratch,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_COMMON,
        )]);
    }
    for t in textures {
        move_to(&setup, t, COPY_SOURCE);
    }
    let before = read_planes(host, &setup, &resources);
    for (t, w) in textures.into_iter().zip(real) {
        move_to(&setup, t, w);
    }
    submit(host, &setup);
    assert_eq!(rows(&before), seeds);
    assert_eq!(state_triplet(&textures), real);
    // Each test starts with both sides independently seeded and the chosen source authoritative.
    twin.newer
        .store(if to_depth { TWIN } else { DEPTH }, Relaxed);
    let work = host.new_list().unwrap();
    let (root, pso) = crate::draw::tests::compile_cs(
        r#"
RWByteAddressBuffer outbox : register(u0);
[RootSignature("UAV(u0)")] [numthreads(1,1,1)]
void main() { outbox.Store(0, 0xc0decafe); }
"#,
    );
    let marker = scratch_buffer(256).unwrap();
    let (rb, marker_ptr) = work.readback(host, 4).unwrap();
    // Sentinel covers both the shader's destination and its independent readback.
    let (initial, initial_ptr) = work.upload(host, 4).unwrap();
    unsafe {
        initial_ptr.write_bytes(0x5a, 4);
        marker_ptr.write_bytes(0xa5, 4);
        work.list.CopyBufferRegion(&marker, 0, &initial, 0, 4);
        work.list.ResourceBarrier(&[transition(
            &marker,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )]);
        work.list.SetComputeRootSignature(&root);
        work.list.SetPipelineState(&pso);
        work.list
            .SetComputeRootUnorderedAccessView(0, marker.GetGPUVirtualAddress());
    }
    issued();
    let copies = COPIES.load(Relaxed);
    ABSENT.set(Some(0));
    if !prelude {
        use_side(&work, &depth, twin, !to_depth);
    }
    use_side(&work, &depth, twin, to_depth);
    let recorded_calls = ABSENT.get().unwrap();
    let recorded_copies = COPIES.load(Relaxed) - copies;
    let recording_rebind = work.take_rebind();
    assert!(!recording_rebind);
    // No rebinding after route: the shader must still use this root UAV and PSO.
    unsafe {
        work.list.Dispatch(1, 1, 1);
        work.list.ResourceBarrier(&[transition(
            &marker,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        )]);
        work.list.CopyBufferRegion(&rb, 0, &marker, 0, 4);
    }
    let recorded = issued();
    submit(host, &work);
    let submitted = issued();
    let total_calls = ABSENT.replace(None).unwrap();
    let total_copies = COPIES.load(Relaxed) - copies;
    let after_rebind = work.take_rebind();
    let marker_value = unsafe { marker_ptr.cast::<u32>().read_unaligned() };
    assert_eq!(
        (recorded_calls, recorded_copies),
        if prelude { (0, 0) } else { (1, 1) }
    );
    assert_eq!(
        (total_calls, total_copies, after_rebind, marker_value),
        (1, 1, false, 0xc0decafe)
    );
    assert_eq!(
        twin.newer.load(Relaxed),
        if to_depth { DEPTH } else { TWIN }
    );
    let ids = textures.map(|t| t.texture.as_raw() as usize);
    let mut replayed = HashMap::from_iter(ids.into_iter().zip(real));
    replay(&submitted, &mut replayed).unwrap();
    replay(&recorded, &mut replayed).unwrap();
    let end = if prelude {
        real
    } else if to_depth {
        [DEPTH_WRITE, UNORDERED_ACCESS, COPY_SOURCE]
    } else {
        [COPY_SOURCE, UNORDERED_ACCESS, UNORDERED_ACCESS]
    };
    assert_eq!(ids.map(|id| replayed[&id]), end);
    assert_eq!(state_triplet(&textures), end);
    for (i, id) in ids.into_iter().enumerate() {
        let changes = |events: &[(usize, u32, u32)]| {
            events
                .iter()
                .filter(|(r, _, _)| *r == id)
                .map(|(_, a, b)| (*a, *b))
                .collect::<Vec<_>>()
        };
        let (p, r) = (changes(&submitted), changes(&recorded));
        eprintln!("texture={i} prelude_transitions={p:?} list_transitions={r:?}");
        if prelude {
            let required = match i {
                0 => {
                    if to_depth {
                        COPY_DEST
                    } else {
                        COPY_SOURCE
                    }
                }
                2 => {
                    if to_depth {
                        COPY_SOURCE
                    } else {
                        COPY_DEST
                    }
                }
                _ => real[i],
            };
            let expected: Vec<_> = if required != real[i] {
                vec![(real[i], required), (required, real[i])]
            } else {
                vec![]
            };
            assert_eq!(p, expected);
        }
    }
    let read = host.new_list().unwrap();
    for t in textures {
        move_to(&read, t, COPY_SOURCE);
    }
    let after = read_planes(host, &read, &resources);
    for (t, w) in textures.into_iter().zip(end) {
        move_to(&read, t, w);
    }
    submit(host, &read);
    let after = rows(&after);
    let mut expected = seeds.clone();
    let (src, dst) = if to_depth { (6, 2) } else { (2, 6) };
    expected[dst..dst + 2].clone_from_slice(&seeds[src..src + 2]);
    for (i, (initial, actual)) in seeds.iter().zip(&after).enumerate() {
        eprintln!(
            "plane={} mip={} before={initial:02x?} after={actual:02x?} expected={:02x?}",
            ["host-depth", "host-stencil", "twin-depth", "twin-stencil"][i / 2],
            i % 2,
            expected[i]
        );
    }
    assert_eq!(after, expected);
    assert_eq!(state_triplet(&textures), end);
    eprintln!(
        "PASS path={} direction={} copies_recorded={recorded_copies} copies_total={total_copies} converter_calls_recorded={recorded_calls} converter_calls_total={total_calls} rebind_recorded={recording_rebind} rebind_submitted={after_rebind} marker={marker_value:#010x} state_before={real:?} state_after={end:?}",
        if prelude { "prelude" } else { "route" },
        if to_depth {
            "twin-to-host"
        } else {
            "host-to-twin"
        }
    );
}

#[test]
#[ignore = "native D3D12 mixed depth/stencil state routing and GPU readback"]
fn two_mip_state_paths() {
    let host = host().expect("native D3D12");
    adapter(host);
    if let Some(prior) = host.log_messages() {
        eprintln!("host initialization: {prior}");
    }
    for prelude in [false, true] {
        for to_depth in [false, true] {
            run_case(host, prelude, to_depth);
        }
    }
}

use super::*;
use crate::host::Host;
use crate::object::slot;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};
use windows::core::Interface;

fn info_queue(host: &Host) -> ID3D12InfoQueue {
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
    println!("info_queue=active init={}", unsafe {
        queue.GetNumStoredMessages()
    });
    queue
}

fn messages(queue: &ID3D12InfoQueue, label: &str) -> usize {
    let mut errors = 0;
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
            if m.Severity == D3D12_MESSAGE_SEVERITY_ERROR
                || m.Severity == D3D12_MESSAGE_SEVERITY_CORRUPTION
            {
                errors += 1;
            }
            println!(
                "{label} message severity={} id={} {desc}",
                m.Severity.0, m.ID.0
            );
        }
        println!("{label} messages={n} errors={errors}");
        queue.ClearStoredMessages();
    }
    errors
}

fn release(object: This) {
    assert_eq!(
        slot::<extern "system" fn(This) -> u32>(object, 0x10)(object),
        0
    );
}

fn bind_clear_readback(
    host: &Host,
    queue: &ID3D12InfoQueue,
    image: This,
    label: &str,
    requested: Option<i32>,
    expected_format: i32,
    pattern: &ClearPattern,
    renderable: bool,
) {
    let copy = host_copy(image).unwrap();
    let texture = &copy.texture;
    let null = std::ptr::null();
    let iid = |g: windows::core::GUID| guid(g.data1, g.data2, g.data3, g.data4);
    let (mut allocator, mut guest_list) = (null, null);
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
            &mut guest_list
        ),
        S_OK
    );
    let list = crate::list::host_list(guest_list).unwrap();
    let mut record = [0u64; 14];
    let handle = record.as_mut_ptr() as u64;
    let view_desc = requested.map(|format| {
        [
            format as u32,
            D3D12_RTV_DIMENSION_TEXTURE2D.0 as u32,
            0,
            0,
            0,
            0,
        ]
    });
    crate::descriptor::create_rtv(
        null,
        image,
        view_desc.as_ref().map_or(null.cast(), std::ptr::from_ref),
        handle,
    );
    let desc = crate::descriptor::rtv_record(handle).unwrap().desc();
    let carrier = unsafe { texture.GetDesc() }.Format.0;
    let effective = desc.map_or(carrier, |d| d.Format.0);
    println!(
        "{label} requested={requested:?} host_desc={:?} effective={effective} carrier={carrier}",
        desc.map(|d| d.Format.0)
    );
    assert_eq!(effective, expected_format);
    if !renderable {
        println!("{label} no draw: resolved format {effective} lacks native RT support");
        release(guest_list);
        release(allocator);
        return;
    }
    crate::list::ensure_state(list, image, D3D12_RESOURCE_STATE_RENDER_TARGET.0 as u32);
    slot::<extern "system" fn(This, u32, *const u64, i32, *const u64)>(guest_list, 0x190)(
        guest_list,
        1,
        &handle,
        0,
        null.cast(),
    );
    let bound = messages(queue, &format!("{label} guest bind"));
    assert_eq!(bound, 0);

    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut bytes = 0;
    // SAFETY: the outputs describe one subresource of the live texture.
    unsafe {
        host.device.GetCopyableFootprints(
            &texture.GetDesc(),
            0,
            1,
            0,
            Some(&mut footprint),
            None,
            None,
            Some(&mut bytes),
        )
    };
    assert_eq!(
        (footprint.Footprint.Width, footprint.Footprint.Height),
        (4, 2)
    );
    let (readback, data) = list.readback(host, bytes).unwrap();
    let clear =
        slot::<extern "system" fn(This, u64, *const [f32; 4], u32, *const RECT)>(guest_list, 0x1a0);
    clear(guest_list, handle, &pattern.colors[0], 0, null.cast());
    let rect = RECT {
        left: 1,
        top: 1,
        right: 3,
        bottom: 2,
    };
    clear(guest_list, handle, &pattern.colors[1], 1, &rect);
    crate::list::ensure_state(list, image, D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32);
    // SAFETY: these resources and descriptors live until the submission completes; the readback is mapped.
    unsafe {
        data.write_bytes(0xa5, bytes as usize);
        let commands = &list.list;
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(texture),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            ..Default::default()
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&readback),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: footprint,
            },
        };
        commands.CopyTextureRegion(&to, 0, 0, 0, &from, None);
    }
    assert_eq!(
        slot::<extern "system" fn(This) -> HRESULT>(guest_list, 0x48)(guest_list),
        S_OK
    );
    host.execute(&[list]);
    unsafe { host.device.GetDeviceRemovedReason().unwrap() };
    println!(
        "readback footprint format={} pitch={} bytes={bytes}",
        footprint.Footprint.Format.0, footprint.Footprint.RowPitch
    );
    let [pixel, other] = pattern.pixels;
    let rows: Vec<_> = (0..2)
        .map(|y| {
            // SAFETY: the footprint names two live 4-pixel rows in the fenced readback.
            unsafe {
                std::slice::from_raw_parts(
                    data.add(footprint.Offset as usize + y * footprint.Footprint.RowPitch as usize),
                    4 * pixel.len(),
                )
            }
            .to_vec()
        })
        .collect();
    let expected = [pixel.repeat(4), [pixel, other, other, pixel].concat()];
    println!("{label} rows={rows:02x?}");
    assert_eq!(
        messages(queue, &format!("{label} guest clear and readback")),
        0
    );
    assert_eq!(rows, expected);
    release(guest_list);
    release(allocator);
}

struct ClearPattern {
    // Floating-point input to the guest clear and the expected storage bytes of each pixel.
    colors: [[f32; 4]; 2],
    pixels: [&'static [u8]; 2],
}

fn pattern(guest_format: i32, view_format: i32) -> ClearPattern {
    let colors = match guest_format {
        115 => [
            [3. / 15., 6. / 15., 12. / 15., 1.],
            [12. / 15., 3. / 15., 6. / 15., 9. / 15.],
        ],
        67 | 28 => [[2., 0.75, 0.25, 1.], [0.25, 2., 0.75, 1.]],
        _ => unreachable!(),
    };
    let pixels: [&[u8]; 2] = match view_format {
        115 => [&[0x6c, 0xf3], &[0x36, 0x9c]], // 0xARGB, low nibble is blue.
        49 => [&[0x33, 0x66], &[0xcc, 0x33]],  // R8G8_UNORM, no blue or alpha.
        67 => [&[0x00, 0xc1, 0x80, 0x88], &[0x20, 0x00, 0x82, 0x89]], // Native RGB9E5.
        28 => [&[0xff, 0xbf, 0x40, 0xff], &[0x40, 0xff, 0xbf, 0xff]], // RGBA8_UNORM.
        _ => unreachable!(),
    };
    ClearPattern { colors, pixels }
}

#[test]
#[ignore = "requires a selected D3D12 adapter and an active debug layer"]
fn asymmetric_guest_rtv_readback() {
    let host = crate::host::host().expect("D3D12 device");
    let queue = info_queue(host);
    messages(&queue, "initialization");
    let null = std::ptr::null();
    for (format, flags, alias) in [(28, 1, 28), (115, 1, 49), (67, 5, 28)] {
        let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
            Format: DXGI_FORMAT(format),
            ..Default::default()
        };
        unsafe {
            host.device
                .CheckFeatureSupport(
                    D3D12_FEATURE_FORMAT_SUPPORT,
                    (&raw mut support).cast(),
                    size_of_val(&support) as u32,
                )
                .unwrap();
        }
        let original_rt = support
            .Support1
            .contains(D3D12_FORMAT_SUPPORT1_RENDER_TARGET);
        let desc = ResourceDesc {
            dimension: 3,
            alignment: 0,
            width: 4,
            height: 2,
            depth_or_array_size: 1,
            mip_levels: 1,
            format: format as u32,
            sample_count: 1,
            sample_quality: 0,
            layout: 0,
            flags,
        };
        let bg = pattern(format, alias).colors[0];
        let hint = [
            format as u32,
            bg[0].to_bits(),
            bg[1].to_bits(),
            bg[2].to_bits(),
            bg[3].to_bits(),
        ];
        let props = HeapProperties {
            kind: 1,
            cpu_pages: 0,
            pool: 0,
            creation_nodes: 0,
            visible_nodes: 0,
        };
        let mut image = null;
        assert_eq!(
            create_committed(
                null,
                &props,
                0,
                &desc,
                0,
                hint.as_ptr().cast(),
                &RESOURCE_IIDS[0],
                &mut image
            ),
            S_OK
        );
        let copy = host_copy(image).expect("production texture");
        let actual = unsafe { copy.texture.GetDesc() };
        println!(
            "guest={format} carrier={} guest_rt_supported={original_rt}",
            actual.Format.0
        );
        assert_eq!(messages(&queue, "texture creation"), 0);
        for (kind, requested) in [
            ("alias", Some(alias)),
            ("explicit", Some(format)),
            ("default", None),
        ] {
            if format == 28 && kind != "alias" {
                continue;
            }
            let is_original = kind != "alias";
            let effective = if is_original { format } else { alias };
            let clear = pattern(format, effective);
            let label = format!("guest={format} view={kind}");
            bind_clear_readback(
                host,
                &queue,
                image,
                &label,
                requested,
                effective,
                &clear,
                !is_original || original_rt,
            );
        }
        release(image);
    }
}

use super::*;
use crate::host::{Host, transition};
use crate::object::slot;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4};
use windows::core::Interface;

fn info_queue(host: &Host) -> ID3D12InfoQueue {
    assert_eq!(std::env::var("VICTORY_D3D12_DEBUG").as_deref(), Ok("1"));
    // SAFETY: queries on a live device and DXGI adapter.
    let desc = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(host.device.GetAdapterLuid()).unwrap();
        adapter.GetDesc1().unwrap()
    };
    let name = String::from_utf16_lossy(&desc.Description);
    let name = name.trim_end_matches('\0');
    assert!(name.contains(&std::env::var("EIGHT_EXPECT_ADAPTER").unwrap()), "{name}");
    println!("adapter={name} vendor={:04x} device={:04x} luid={:08x}:{:08x} flags={:#x}",
        desc.VendorId, desc.DeviceId, desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart, desc.Flags);
    let queue = host.device.cast::<ID3D12InfoQueue>().expect("debug layer info queue");
    println!("info_queue=active init={}", unsafe { queue.GetNumStoredMessages() });
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
            errors += usize::from(m.Severity == D3D12_MESSAGE_SEVERITY_ERROR || m.Severity == D3D12_MESSAGE_SEVERITY_CORRUPTION);
            println!("{label} message severity={} id={} {desc}", m.Severity.0, m.ID.0);
        }
        println!("{label} messages={n} errors={errors}");
        queue.ClearStoredMessages();
    }
    errors
}

fn clear_readback(host: &Host, image: This, view: DXGI_FORMAT, bpp: usize) -> Vec<Vec<u8>> {
    let copy = host_copy(image).unwrap();
    let texture = &copy.texture;
    let list = host.new_list().unwrap();
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut bytes = 0;
    // SAFETY: the outputs describe one subresource of the live texture.
    unsafe { host.device.GetCopyableFootprints(&texture.GetDesc(), 0, 1, 0, Some(&mut footprint), None, None, Some(&mut bytes)) };
    assert_eq!((footprint.Footprint.Width, footprint.Footprint.Height), (4, 2));
    let (readback, data) = list.readback(host, bytes).unwrap();
    let mut record = [0u64; 14];
    let handle = record.as_mut_ptr() as u64;
    let view_desc = [view.0 as u32, D3D12_RTV_DIMENSION_TEXTURE2D.0 as u32, 0, 0, 0, 0];
    crate::descriptor::create_rtv(std::ptr::null(), image, &view_desc, handle);
    let desc: D3D12_RENDER_TARGET_VIEW_DESC = crate::descriptor::rtv_record(handle).unwrap().desc().unwrap();
    assert_eq!(desc.Format, view);
    let rtv = host.rtv(texture, Some(&desc));
    crate::list::ensure_state(&list, image, D3D12_RESOURCE_STATE_RENDER_TARGET.0 as u32);
    // SAFETY: these resources and descriptors live until the submission completes; the readback is mapped.
    unsafe {
        data.write_bytes(0xa5, bytes as usize);
        let commands = &list.list;
        commands.OMSetRenderTargets(1, Some(&rtv), false, None);
        commands.ClearRenderTargetView(rtv, &[1.0, 0.0, 1.0, 1.0], None);
        let rect = RECT { left: 1, top: 1, right: 3, bottom: 2 };
        commands.ClearRenderTargetView(rtv, &[0.0, 1.0, 0.0, 1.0], Some(&[rect]));
        commands.ResourceBarrier(&[transition(texture, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(texture),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            ..Default::default()
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&readback),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
        };
        commands.CopyTextureRegion(&to, 0, 0, 0, &from, None);
    }
    list.close();
    host.execute(&[&list]);
    unsafe { host.device.GetDeviceRemovedReason().unwrap() };
    println!("readback footprint format={} pitch={} bytes={bytes}", footprint.Footprint.Format.0, footprint.Footprint.RowPitch);
    (0..2).map(|y| {
        // SAFETY: the footprint names two live 4-pixel rows in the fenced readback.
        unsafe { std::slice::from_raw_parts(data.add(footprint.Offset as usize + y * footprint.Footprint.RowPitch as usize), 4 * bpp) }.to_vec()
    }).collect()
}

#[test]
#[ignore = "requires the Windows D3D12 debug layer and a selected adapter"]
fn promotion_binds_and_clears() {
    let host = crate::host::host().expect("D3D12 device");
    let queue = info_queue(host);
    messages(&queue, "initialization");
    let parent = std::env::var("EIGHT_EXPECT_PARENT").as_deref() == Ok("1");
    let null = std::ptr::null();
    let mut negatives = 0;
    for (format, flags, view, pixel, other) in [
        (28, 1, 28, &[255, 0, 255, 255][..], &[0, 255, 0, 255][..]),
        (115, 1, 49, &[255, 0][..], &[0, 255][..]),
        (67, 5, 28, &[255, 0, 255, 255][..], &[0, 255, 0, 255][..]),
    ] {
        let label = format!("guest={format} flags={flags:#x} rtv={view}");
        let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT { Format: DXGI_FORMAT(format), ..Default::default() };
        // SAFETY: the feature query is passed its matching output type.
        unsafe { host.device.CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, (&raw mut support).cast(), size_of_val(&support) as u32).unwrap() };
        let desc = ResourceDesc { dimension: 3, alignment: 0, width: 4, height: 2, depth_or_array_size: 1, mip_levels: 1,
            format: format as u32, sample_count: 1, sample_quality: 0, layout: 0, flags };
        let hint = [format as u32, 1f32.to_bits(), 0, 1f32.to_bits(), 1f32.to_bits()];
        let mut image = null;
        let heap = HeapProperties { kind: 1, cpu_pages: 0, pool: 0, creation_nodes: 0, visible_nodes: 0 };
        let hr = create_committed(null, &heap, 0, &desc, 0, hint.as_ptr().cast(), &RESOURCE_IIDS[0], &mut image);
        println!("{label} support1={:#x} support2={:#x} guest_create_hr={hr:#x}", support.Support1.0, support.Support2.0);
        assert_eq!(hr, S_OK);
        let copy = host_copy(image);
        let errors = messages(&queue, &format!("{label} creation"));
        if copy.is_none() && parent && format != 28 {
            println!("{label} parent negative: no host resource");
            assert!(errors > 0, "missing resource without debug evidence");
            negatives += 1;
        } else {
            assert_eq!(errors, 0, "{label}");
            let copy = copy.expect("production host resource");
            let actual = unsafe { copy.texture.GetDesc() };
            println!("{label} carrier={} host_flags={:#x} castable={:?}", actual.Format.0, actual.Flags.0, copy.castable);
            assert_eq!(actual.Flags.0 as u32 & flags, flags);
            assert!(actual.Format.0 == format || copy.castable.contains(&(format as u32)), "original format was lost");
            assert!(actual.Format.0 == view || copy.castable.contains(&(view as u32)), "explicit RTV alias unavailable");
            let rows = clear_readback(host, image, DXGI_FORMAT(view), pixel.len());
            let expected = [pixel.repeat(4), [pixel, other, other, pixel].concat()];
            println!("{label} rows={rows:02x?}");
            assert_eq!(messages(&queue, &format!("{label} draw")), 0);
            assert_eq!(rows, expected);
        }
        assert_eq!(slot::<extern "system" fn(This) -> u32>(image, 0x10)(image), 0);
    }
    if parent {
        assert!(negatives > 0, "expected at least one parent creation failure");
    }
}

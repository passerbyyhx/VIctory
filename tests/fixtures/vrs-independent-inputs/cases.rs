    // A 4x4 target fits in a single 16x16 shading-rate tile on this adapter.
    assert!(tier >= 2 && options6.ShadingRateImageTileSize >= 4);
    let image_desc = D3D12_RESOURCE_DESC {
        Width: 1,
        Height: 1,
        Format: DXGI_FORMAT_R8_UINT,
        Flags: D3D12_RESOURCE_FLAG_NONE,
        ..rt_desc
    };
    let image = resource(&image_desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    let upload = resource(&buffer_desc, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    let mut addr = std::ptr::null_mut();
    unsafe {
        upload.Map(0, Some(&D3D12_RANGE::default()), Some(&mut addr)).unwrap();
        addr.cast::<u8>().write(D3D12_SHADING_RATE_1X2.0 as u8);
        upload.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 1 }));
    }
    let set_image = || {
        let cmd: ID3D12GraphicsCommandList5 = commands().list.cast().unwrap();
        unsafe { cmd.RSSetShadingRateImage(&image) };
    };
    let cases = [
        ("both pass", Some([0,0]), 1, 1),
        ("primitive override", Some([1,0]), 2, 1),
        ("image override", Some([0,1]), 1, 2),
        ("both max", Some([3,3]), 2, 2),
        ("reset, sources still supplied", None, 1, 1),
    ];
    for (i,(label, combiners, x, y)) in cases.into_iter().enumerate() {
        if i != 0 {
            assert_eq!(slot::<extern "system" fn(This, This, This) -> HRESULT>(list, 0x50)(list, allocator, null), S_OK);
        }
        bind();
        clear();
        scissors(list, 1, &RECT { left: 0, top: 0, right: 4, bottom: 4 });
        if i == 0 {
            let mut src = D3D12_TEXTURE_COPY_LOCATION {
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_R8_UINT, Width: 1, Height: 1, Depth: 1, RowPitch: 256
                        },
                    },
                },
                ..Default::default()
            };
            let mut dst = D3D12_TEXTURE_COPY_LOCATION {
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                ..Default::default()
            };
            // Borrow the resources for the recording call; both live to the final fence.
            unsafe {
                src.pResource = std::mem::transmute_copy(&upload);
                dst.pResource = std::mem::transmute_copy(&image);
                commands().list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                commands().list.ResourceBarrier(&[crate::host::transition(&image,
                    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_SHADING_RATE_SOURCE)]);
            }
        }
        set_image(); // Host control: the guest has no shading-rate image setter on this head.
        if let Some(combiners) = combiners { set_rate(list, 0, combiners.as_ptr()); }
        draw(list, 3, 1, 0, 0);
        check_pixels(label, read_target(), x, y);
    }

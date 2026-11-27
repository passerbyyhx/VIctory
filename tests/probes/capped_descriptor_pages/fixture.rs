    #[cfg(feature = "diagnostics")]
    #[test]
    fn capped_heap_page_scan_and_last_texel() {
        let host = crate::host::host().expect("D3D12 device");
        let natural = std::env::var_os("VICTORY_PROBE_NATURAL").is_some();
        let parent = std::env::var_os("VICTORY_PROBE_PARENT").is_some();
        let title = if natural { 1_245_184u32 } else { 8192u32 };
        let page_slots = (crate::watch::PAGE / 32) as u32;
        let (memory, _) = crate::watch::tests::twin_views(title.div_ceil(page_slots) as usize);
        let bytes = u64::from(title) * 32;
        let desc = [0, title, 1, 0, memory as u32, (memory as u64 >> 32) as u32, bytes as u32, 0];
        let null = std::ptr::null();
        let mut heap = null;
        let iid = crate::object::guid(0x8efb471d, 0x616c, 0x4f49, [0x90,0xf7,0x12,0x7b,0xb7,0x63,0xfa,0x51]);
        assert_eq!(crate::descriptor::create_heap(null, desc.as_ptr().cast(), &iid, &mut heap), S_OK);
        let title_heap = crate::descriptor::heap(heap);
        if !natural { title_heap.probe_cap(3073); }
        let gpu = title_heap.host().expect("host heap");
        let cap = gpu.count();
        println!("natural={natural} title={title} host={cap} slots/page={page_slots}");
        assert!(cap < title && cap % page_slots != 0);
        let at = |i:u32| title_heap.start + u64::from(i) * 32;
        let (src,dst) = (resource(1,256,1,0,1),resource(1,256,1,0,1));
        let tex = resource(3,4,1,28,LINEAR);
        let (src_bytes,_) = crate::resource::buffer_bytes(src).unwrap();
        let (dst_bytes,_) = crate::resource::buffer_bytes(dst).unwrap();
        let (tex_bytes,td) = crate::resource::texture_memory(tex).unwrap();
        let n = crate::resource::texture_layout(&td).unwrap().planes[0].layout.size as usize;
        unsafe {
            for i in 0..64 { src_bytes.cast::<u32>().add(i).write(1000 + i as u32); }
            std::ptr::write_bytes(tex_bytes.cast_mut(), 0x42, n);
        }
        let srv = [39,1,0x1688,0,0,0,64,0,1,0];
        let uav = [39,1,0,0,64,0,0,0,1,0];
        crate::descriptor::create_srv(null,src,&srv,at(cap-3));
        crate::descriptor::create_uav(null,dst,null,&uav,at(cap-2));
        crate::descriptor::create_srv(null,tex,std::ptr::null(),at(1));
        // Discard initial dirty pages to time only explicit writes. Each following
        // sync runs the normal watch, scanner and descriptor mirroring code.
        title_heap.probe_drain_initial();
        let scan = |label:&str, expected:u64| {
            let before = crate::descriptor::probe_counts();
            crate::descriptor::sync_heaps();
            let after = crate::descriptor::probe_counts();
            let diff = (after.0-before.0,after.1-before.1);
            println!("{label} diagnostic_slots={} record_reads={} highest_warning={:?}",diff.0,diff.1,after.2);
            assert_eq!(diff,(expected,expected),"{label}");
        };
        scan("clean",0);
        let copy = |slot:u32| unsafe { std::ptr::copy_nonoverlapping(at(1) as *const u8, at(slot) as *mut u8, 32) };
        copy(9);
        scan("below",u64::from(page_slots));
        copy(cap-1);
        scan("straddles",u64::from(if parent {page_slots} else {cap % page_slots}));
        copy((cap.div_ceil(page_slots)+1)*page_slots);
        scan("past",u64::from(if parent {page_slots} else {0}));
        scan("clean_again",0);
        let (root,pso) = compile();
        let list = host.new_list().unwrap();
        let out = crate::shadow::find(dst_bytes as u64,256).expect("output host copy");
        unsafe {
            list.list.SetDescriptorHeaps(&[Some(gpu.heap.clone())]);
            list.list.SetComputeRootSignature(&root);
            list.list.SetComputeRootDescriptorTable(0,gpu.gpu(cap-3));
            list.list.SetPipelineState(&pso);
            list.list.Dispatch(4,1,1);
        }
        list.defer_heap(&gpu.viewed);
        list.close();
        host.execute(&[&list]);
        let words = out.read_host(dst_bytes as u64,64);
        assert_eq!(words.len(),64);
        for (i,v) in words.iter().enumerate() { assert_eq!(*v,1066+i as u32); }
        println!("texture_slot={} first={} last={} matched=64/64",cap-1,words[0],words[63]);
    }

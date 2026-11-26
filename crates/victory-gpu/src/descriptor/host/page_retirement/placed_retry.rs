use super::registered_textures::RegisteredTextures;
use super::translated_reads::{record_read, translated_load};
use super::{A, B, PAGE, host, report_adapter, view_pages};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;

#[test]
#[ignore = "run alone on a paged D3D12 device with normal page reclamation"]
fn placed_and_committed_textures_after_page_restoration() {
    check(false);
}

#[test]
#[ignore = "run alone on a paged D3D12 device with normal page reclamation"]
fn placed_texture_previously_sampled_with_no_free_page() {
    check(true);
}

fn check(early: bool) {
    let host = host().expect("D3D12 device");
    assert!(view_pages(), "requires descriptor paging");
    report_adapter(host);
    let count = 1 << 20;
    let source = RegisteredTextures::new(host, count);
    let heap = source.host_heap();
    let pages = heap.pages.as_ref().unwrap();
    let pressure = count - PAGE + 7;
    let physical = |slot| {
        let page = heap.page_table_entry(slot / PAGE);
        (page != 0).then_some(page * PAGE + slot % PAGE)
    };
    source.write_placed_green(A);
    source.write_yellow(A + 1);
    source.write_placed_green(A + 2);
    let original = physical(A).unwrap();
    let free = pages.counts().0;
    let fillers: Vec<_> = (0..count / PAGE)
        .filter(|p| ![A / PAGE, B / PAGE, pressure / PAGE].contains(p))
        .take(free as usize)
        .map(|p| {
            let i = p * PAGE + 13;
            super::super::null_view(heap.cpu(i));
            i
        })
        .collect();
    assert_eq!(pages.counts().0, 0);
    source.write_yellow(B + 1);
    source.write_placed_green(B + 2);
    assert_eq!(physical(B), None);
    if early {
        source.write_placed_green(B);
    }

    let outputs = [count + 7, count + 8];
    let buffers = outputs.map(|i| {
        let (buffer, mapped) = host.shared_buffer(16).unwrap();
        let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    NumElements: 4,
                    Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                    ..Default::default()
                },
            },
        };
        // SAFETY: each pinned dynamic slot owns a separate live output buffer.
        unsafe {
            host.device
                .CreateUnorderedAccessView(&buffer, None, Some(&desc), heap.cpu(i))
        };
        (buffer, mapped)
    });
    let shader = [A, B].map(|i| [translated_load(i), translated_load(i + 1)]);
    let green = [0., 1., 0., 1.];
    let yellow = [1., 1., 0., 1.];
    let black = [0.; 4];
    let sample = |index: usize, n: usize| {
        let slot = [A, B][index];
        let state = || {
            (
                physical(slot),
                [heap.kept(slot), heap.kept(slot + 1)],
                pages.counts(),
            )
        };
        let before = state();
        let list = host.new_list().unwrap();
        list.defer_heap(&heap.viewed);
        for (i, (_, mapped)) in buffers.iter().enumerate() {
            // SAFETY: the preceding submission has finished.
            unsafe { mapped.cast::<[f32; 4]>().write([f32::NAN; 4]) };
            record_read(&list, heap, &shader[index][i], outputs[i]);
        }
        list.close();
        host.execute(&[&list]);
        assert!(!heap.kept(slot + 2), "unread placed neighbor");
        let colors = buffers.each_ref().map(|(_, p)| {
            // SAFETY: the queue finished both dispatches.
            unsafe { p.cast::<[f32; 4]>().read() }
        });
        eprintln!(
            "virtual {slot}/{} D{n}: {before:?} -> {:?}; placed/committed={colors:?}",
            slot + 1,
            state()
        );
        colors
    };
    // Initialize the resident placed SRV through ordinary translated reads.
    for n in 1..=3 {
        let colors = sample(0, n);
        assert_eq!(colors[1], yellow);
        if n == 3 {
            assert_eq!(colors[0], green);
        }
    }
    if early {
        assert_eq!(sample(1, 0), [black, black]);
        assert_eq!((physical(A), physical(B)), (Some(original), None));
    }
    let empty = host.new_list().unwrap();
    empty.close();
    let mut transfers = Vec::new();
    for (pass, (from, to)) in [(A, B), (B, A)].into_iter().enumerate() {
        while pages.submission_age(from) < 32 {
            for &slot in &fillers {
                heap.gpu(slot);
            }
            host.execute(&[&empty]);
        }
        source.write_green(pressure + pass as u32);
        if pass == 0 && !early {
            source.write_placed_green(B);
        }
        assert_eq!((physical(from), physical(to)), (Some(original), None));
        let mut trace = Vec::new();
        for n in 1..=4 {
            let rgba = sample(1 - pass, n);
            assert_eq!(
                rgba[1],
                if n == 1 { black } else { yellow },
                "committed D{n}"
            );
            assert_eq!((physical(from), physical(to)), (None, Some(original)));
            trace.push(rgba[0]);
        }
        assert_eq!(trace[3], green, "placed D4");
        transfers.push(trace);
    }
    for sequence in transfers {
        assert_eq!(sequence, [black, green, green, green], "placed first retry");
    }
}

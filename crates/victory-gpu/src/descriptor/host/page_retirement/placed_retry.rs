use super::registered_textures::RegisteredTextures;
use super::translated_reads::{record_read, translated_load};
use super::{A, B, PAGE, host, report_adapter, view_pages};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;

#[test]
#[ignore = "run alone on a paged D3D12 device with normal page reclamation"]
fn placed_and_committed_textures_after_page_restoration() {
    check(false, Probe::FirstRetry);
}

#[test]
#[ignore = "run alone on a paged D3D12 device with normal page reclamation"]
fn placed_texture_previously_sampled_with_no_free_page() {
    check(true, Probe::FirstRetry);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    FirstRetry,
    Control,
    Alias,
    NewPixel,
}

#[test]
#[ignore = "one process per arm and adapter; needs descriptor paging"]
fn compare_placed_alias_and_new_pixel() {
    let mode = std::env::var("VICTORY_TEST_PLACED_ARM").unwrap_or_else(|_| "control".into());
    let probe = match mode.as_str() {
        "control" => Probe::Control,
        "alias" => Probe::Alias,
        "new-pixel" => Probe::NewPixel,
        _ => panic!("expected VICTORY_TEST_PLACED_ARM=control, alias or new-pixel"),
    };
    let early = std::env::var("VICTORY_TEST_PLACED_D0").as_deref() == Ok("1");
    eprintln!("experiment arm={mode} D0={early}");
    check(early, probe);
}

fn check(early: bool, probe: Probe) {
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
    // This filler page exists in every arm, is kept live while aging and is
    // distinct from the page being reclaimed. No additional title page is taken.
    let alias = fillers[0] + 1;
    let alias_physical = physical(alias).unwrap();
    assert_ne!(alias_physical / PAGE, original / PAGE);
    let with_alias = probe == Probe::Alias;
    let new_pixel = probe == Probe::NewPixel;
    let records_before = pages.counts();
    if with_alias {
        source.write_placed_green(alias);
        assert_eq!(source.placed_record(A), source.placed_record(alias));
    }
    assert_eq!(physical(alias), Some(alias_physical));
    assert_eq!(pages.counts(), records_before);
    eprintln!(
        "full-pool before/after alias registration={records_before:?}, reference virtual={alias} physical={alias_physical}, reused physical={original}"
    );
    source.write_yellow(B + 1);
    source.write_placed_green(B + 2);
    assert_eq!(physical(B), None);
    if early {
        source.write_placed_green(B);
    }

    let outputs = [count + 7, count + 8, count + 9];
    let output_count = if with_alias { 3 } else { 2 };
    let make_output = |&i: &u32| {
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
    };
    let buffers: Vec<_> = outputs[..output_count].iter().map(make_output).collect();
    let shader = [A, B].map(|i| [translated_load(i), translated_load(i + 1)]);
    let reference = with_alias.then(|| translated_load(alias));
    let held = |slot: u32| {
        let kept = heap.kept.lock().unwrap();
        kept[slot as usize]
            .as_ref()
            .map(|(r, _)| windows::core::Interface::as_raw(r) as usize)
    };
    let green = [0., 1., 0., 1.];
    let yellow = [1., 1., 0., 1.];
    let blue = [0., 0., 1., 1.];
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
        let handles = || [slot, slot + 1, alias].map(held);
        let ids_before = handles();
        let uploads = crate::bindless::test_heap_uploads();
        let list = host.new_list().unwrap();
        list.defer_heap(&heap.viewed);
        let pipelines = shader[index].iter().chain(reference.iter());
        for ((pipeline, (_, mapped)), output) in pipelines.zip(&buffers).zip(outputs) {
            // SAFETY: the preceding submission has finished.
            unsafe { mapped.cast::<[f32; 4]>().write([f32::NAN; 4]) };
            record_read(&list, heap, pipeline, output);
        }
        list.close();
        host.execute(&[&list]);
        assert!(!heap.kept(slot + 2), "unread placed neighbor");
        let colors: Vec<_> = buffers
            .iter()
            .map(|(_, p)| {
                // SAFETY: the queue finished all dispatches.
                unsafe { p.cast::<[f32; 4]>().read() }
            })
            .collect();
        assert_eq!(physical(alias), Some(alias_physical));
        let ids_after = handles();
        let uploads_after = crate::bindless::test_heap_uploads();
        eprintln!(
            "D{n} target/committed/alias kept COM={ids_before:x?} -> {ids_after:x?}, heap uploads={uploads:?} -> {uploads_after:?}"
        );
        eprintln!(
            "virtual {slot}/{} D{n}: {before:?} -> {:?}; target/committed/(alias)={colors:?}",
            slot + 1,
            state()
        );
        if with_alias && ids_before[2].is_some() {
            assert_eq!(ids_after[2], ids_before[2], "persistent alias COM");
            if let Some(target) = ids_after[0] {
                assert_eq!(Some(target), ids_after[2], "matching live D3D12 resource");
            }
            assert_eq!(colors[2], green, "resident alias");
        }
        if !with_alias {
            assert_eq!(ids_after[2], None);
        }
        [colors[0], colors[1]]
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
        assert_eq!(physical(to), None);
        if pass == 1 && new_pixel {
            let record = source.placed_record(A);
            assert_eq!(record, source.placed_record(B));
            source.change_placed_green_to_blue();
            assert_eq!(source.placed_record(A), record);
            assert_eq!(source.placed_record(B), record);
            eprintln!(
                "CPU title texel green -> blue after last B, A unmapped, pool={:?}, uploads={:?}",
                pages.counts(),
                crate::bindless::test_heap_uploads()
            );
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
        assert_eq!(
            trace[3],
            if new_pixel && pass == 1 { blue } else { green },
            "placed D4"
        );
        transfers.push(trace);
    }
    if probe != Probe::FirstRetry {
        eprintln!("transfer traces A->B then B->A={transfers:?}");
        return;
    }
    for sequence in transfers {
        assert_eq!(sequence, [black, green, green, green], "placed first retry");
    }
}

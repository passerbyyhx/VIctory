//! Verify same-slot reuse at the real low-water and age thresholds. Run alone.
use std::time::{Duration, Instant};

use super::super::{HostHeap, null_view};
use super::registered_textures::RegisteredTextures;
use super::translated_reads::{record_read, translated_load};
use super::{A, B, BLUE, GREEN, Host, NULL, PAGE, RED, while_pending};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;

/// Looking up a GPU or CPU handle stamps its page. Read the mapped table directly.
fn placement(heap: &HostHeap, slot: u32) -> Option<u32> {
    let page = heap.page_table_entry(slot / PAGE);
    (page != 0).then_some(page * PAGE + slot % PAGE)
}

/// A translated read stamped the resident page in the last submission. The next
/// submission after these 299 is the first that can make that stamp 300 old.
fn age(host: &Host, heap: &HostHeap, fillers: &[u32], resident: u32, original: u32) {
    let started = Instant::now();
    let empty = host.new_list().unwrap();
    empty.close();
    for i in 0..299 {
        if i % 16 == 0 {
            // Keep the filler pages inside the 32-submission pressure window.
            // Request handles just as a command binding those other pages would.
            for &slot in fillers {
                heap.gpu(slot);
            }
        }
        host.execute(&[&empty]);
    }
    std::thread::sleep(Duration::from_millis(10_050).saturating_sub(started.elapsed()));
    assert_eq!(
        placement(heap, resident),
        Some(original),
        "resident slot {resident} before the age boundary"
    );
    eprintln!(
        "299 real idle submissions in {:?}; virtual slot {resident} still at {original}",
        started.elapsed()
    );
}

#[derive(Clone, Copy)]
pub(super) enum Age {
    Strict,
    Pressured,
}

/// Leave every filler hot. Inspect the resident's real submission stamp without
/// acquiring a descriptor handle, which would make that page hot too.
fn idle_until_age(host: &Host, heap: &HostHeap, fillers: &[u32], resident: u32, target_age: u32) {
    let pages = heap.pages.as_ref().unwrap();
    let empty = host.new_list().unwrap();
    empty.close();
    let initial_age = pages.submission_age(resident);
    assert!(initial_age <= target_age);
    for age in initial_age..target_age {
        if (age - initial_age).is_multiple_of(16) {
            for &slot in fillers {
                heap.gpu(slot);
            }
        }
        host.execute(&[&empty]);
    }
    assert_eq!(pages.submission_age(resident), target_age);
}

pub(super) fn check(host: &Host, title_slots: u32, policy: Age) {
    let control_slot = title_slots + 7;
    let output_slot = control_slot + 1;
    let slots = [A, B, control_slot];
    // A partial final title page also owns pinned dynamic slots.
    let unread = (title_slots / PAGE - 1) * PAGE + 7;
    let source = RegisteredTextures::new(host, title_slots);
    let heap = source.host_heap();
    let placements = || slots.map(|slot| placement(heap, slot));
    let pages = heap.pages.as_ref().expect("paged title heap");
    let colors = source.colors();
    source.write_red_a();
    colors.bind_blue(host, heap.cpu(control_slot));
    let (out, mapped) = host.shared_buffer(16).unwrap();
    let output_view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
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
    // SAFETY: out and this fixed dynamic descriptor outlive every dispatch.
    unsafe {
        host.device
            .CreateUnorderedAccessView(&out, None, Some(&output_view), heap.cpu(output_slot))
    };
    let output = || {
        // SAFETY: called only after the submitting queue's fence is complete.
        unsafe { mapped.cast::<[u32; 4]>().read() }.map(f32::from_bits)
    };
    let clear_output = || {
        // SAFETY: the queue is idle before recording each new dispatch. A quiet
        // NaN in every channel cannot match any expected normalized color.
        unsafe { mapped.cast::<[u32; 4]>().write([f32::NAN.to_bits(); 4]) };
    };
    let red = translated_load(A);
    let green = translated_load(B);
    let original = placements()[0].expect("red A has a physical slot");
    let control = placements()[2].unwrap();
    eprintln!(
        "title slots={title_slots}; virtual control={control_slot}, output={output_slot}; physical control={control}, output={:?}",
        placement(heap, output_slot)
    );
    let free = pages.counts().0;
    let mut fillers = Vec::new();
    for page in (0..title_slots / PAGE)
        .filter(|&p| p != A / PAGE && p != B / PAGE && p != unread / PAGE)
        .take(free as usize)
    {
        let slot = page * PAGE + 13;
        null_view(heap.cpu(slot));
        fillers.push(slot);
    }
    let full = pages.counts();
    assert_eq!(full.0, 0, "full physical heap");
    assert_eq!((full.2, full.3, full.4), (0, 0, 0));
    source.write_green(B);
    // A second distinct refused page triggers the allocator's incremental
    // overflow message. It gets a title record but no shader ever requests it.
    source.write_green(unread);
    assert_eq!(heap.page_table_entry(unread / PAGE), 0);
    eprintln!(
        "unsampled overflow control: virtual slot {unread}, page {} unmapped",
        unread / PAGE
    );
    assert_eq!(placements(), [Some(original), None, Some(control)]);
    clear_output();
    let (first, first_pixels) = colors.record_with(host, heap, slots, |list| {
        record_read(list, heap, &red, output_slot)
    });
    host.execute(&[&first]);
    assert_eq!(output(), [1., 0., 0., 1.]);
    assert_eq!(
        first_pixels.checked_colors(slots, placements()),
        [RED, NULL, BLUE]
    );
    eprintln!(
        "A={A} physical={original}; B={B} unmapped; control={control}; (free, in use, taken back, restored after take-back, restored after refusal)={full:?}"
    );

    match policy {
        Age::Strict => age(host, heap, &fillers, A, original),
        Age::Pressured => idle_until_age(host, heap, &fillers, A, 31),
    }
    clear_output();
    let (old, old_pixels) = colors.record_with(host, heap, slots, |list| {
        record_read(list, heap, &red, output_slot)
    });
    let (pending_placements, before, completed) = while_pending(host, &old, || {
        assert!(
            pages.slot(B, true).is_none(),
            "no page may be handed to B here"
        );
        assert_eq!(pages.counts(), full);
        placements()
    });
    assert_eq!(pending_placements, [Some(original), None, Some(control)]);
    assert_eq!(
        old_pixels.checked_colors(slots, pending_placements),
        [RED, NULL, BLUE]
    );
    assert_eq!(output(), [1., 0., 0., 1.]);
    assert_eq!(
        pages.counts(),
        full,
        "the old shader's page-read mark kept A"
    );
    eprintln!(
        "gated fence {before}->{completed}; old pixels red/null/blue, marked A stays resident"
    );

    let demand_missing = |slot, pipeline| {
        clear_output();
        let demand = host.new_list().unwrap();
        record_read(&demand, heap, pipeline, output_slot);
        demand.close();
        host.execute(&[&demand]);
        let rgba = output();
        eprintln!("translated demand for unmapped virtual slot {slot} returned {rgba:?}");
        assert_eq!(rgba, [0.; 4], "first demand before the slot was remade");
    };
    let sample_restored = |pipeline| {
        clear_output();
        // Only the translated load sets a page-read mark. The independent
        // three-color probe echoes the page table and pixels into a fresh readback.
        let (list, pixels) = colors.record_with(host, heap, slots, |list| {
            record_read(list, heap, pipeline, output_slot)
        });
        host.execute(&[&list]);
        (output(), pixels.checked_colors(slots, placements()))
    };

    let prepare_transfer = |resident, check_one_shot: bool| {
        if matches!(policy, Age::Strict) {
            age(host, heap, &fillers, resident, original);
            return None;
        }
        let start = Instant::now();
        assert_eq!(
            pages.submission_age(resident),
            1,
            "previous translated read"
        );
        let probing_slot = if check_one_shot {
            unread + 1
        } else {
            unread + 3
        };
        if check_one_shot {
            idle_until_age(host, heap, &fillers, resident, 31);
            // A separate, previously unwritten slot on the unsampled overflow page.
            source.write_green(probing_slot);
            idle_until_age(host, heap, &fillers, resident, 32);
            assert_eq!(placement(heap, resident), Some(original), "pressured at 32");
            idle_until_age(host, heap, &fillers, resident, 33);
            assert_eq!(
                placement(heap, resident),
                Some(original),
                "no fresh refusal at 33"
            );
            eprintln!(
                "slot {resident}: retained after refusal at age 32 and without fresh pressure at age 33"
            );
        } else {
            idle_until_age(host, heap, &fillers, resident, 32);
        }
        source.write_green(probing_slot + 1);
        assert_eq!(placement(heap, probing_slot + 1), None);
        let eligible_age = pages.submission_age(resident) + 1;
        eprintln!("slot {resident}: renewed refused write, next pass at age {eligible_age}");
        Some(start)
    };
    let under_pressure = prepare_transfer(A, true);
    demand_missing(B, &green);
    let after_b = pages.counts();
    let b_mapping = placements();
    let b_readback = sample_restored(&green);
    if let Some(start) = under_pressure {
        eprintln!(
            "short A->B: {b_mapping:?}, A/B/control={:?}, counts={after_b:?}, elapsed={:?}",
            b_readback.1,
            start.elapsed()
        );
        assert!(start.elapsed() < Duration::from_secs(10));
    }
    assert_eq!(b_mapping, [None, Some(original), Some(control)]);
    assert_eq!(heap.page_table_entry(unread / PAGE), 0);
    assert_eq!(
        after_b,
        (0, full.1, 1, 0, 1),
        "B was refused on its first write"
    );
    assert_eq!(b_readback, ([0., 1., 0., 1.], [NULL, GREEN, BLUE]));
    eprintln!(
        "B={B} reused physical {original}; demand null, retry green; A/B/control=null/green/blue; counts={after_b:?}"
    );

    let under_pressure = prepare_transfer(B, false);
    demand_missing(A, &red);
    assert_eq!(placements(), [Some(original), None, Some(control)]);
    let after_a = pages.counts();
    assert_eq!(
        after_a,
        (0, full.1, 2, 1, 1),
        "A was resident before the allocator took it back"
    );
    let a_readback = sample_restored(&red);
    if let Some(start) = under_pressure {
        eprintln!(
            "short B->A: A/B/control={:?}, counts={after_a:?}, elapsed={:?}",
            a_readback.1,
            start.elapsed()
        );
        assert!(start.elapsed() < Duration::from_secs(10));
    }
    assert_eq!(a_readback, ([1., 0., 0., 1.], [RED, NULL, BLUE]));
    assert_eq!(heap.page_table_entry(unread / PAGE), 0);
    eprintln!(
        "A={A} regained physical {original}; demand null, retry red; A/B/control=red/null/blue; unused slot {unread} still unmapped; counts={after_a:?}"
    );
}

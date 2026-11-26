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

/// A translated read stamped A just before the last submission ended. The next
/// submission after these 299 is the first that can make that stamp 300 old.
fn age(host: &Host, heap: &HostHeap, fillers: &[u32], original: u32) {
    let started = Instant::now();
    let empty = host.new_list().unwrap();
    empty.close();
    for i in 0..299 {
        if i % 100 == 0 {
            // Request handles just as a command binding those other pages would.
            for &slot in fillers {
                heap.gpu(slot);
            }
        }
        host.execute(&[&empty]);
    }
    std::thread::sleep(Duration::from_millis(10_050).saturating_sub(started.elapsed()));
    assert_eq!(
        placement(heap, A),
        Some(original),
        "A before the age boundary"
    );
    eprintln!(
        "299 real idle submissions in {:?}; A still at {original}",
        started.elapsed()
    );
}

pub(super) fn check(host: &Host, title_slots: u32) {
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
    assert_eq!(full.2, 0);
    assert_eq!(full.3, 0);
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
        "A={A} physical={original}; B={B} unmapped; control={control}; (free, in use, taken back, placed again)={full:?}"
    );

    age(host, heap, &fillers, original);
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

    age(host, heap, &fillers, original);
    clear_output();
    let demand = host.new_list().unwrap();
    record_read(&demand, heap, &green, output_slot);
    demand.close();
    host.execute(&[&demand]);
    let first_b = output();
    eprintln!("translated first demand for B returned {first_b:?}");
    assert_eq!(
        first_b, [0.; 4],
        "the demand read saw unmapped B before its record was remade"
    );
    assert_eq!(placements(), [None, Some(original), Some(control)]);
    assert_eq!(heap.page_table_entry(unread / PAGE), 0);
    eprintln!("unsampled overflow control: virtual slot {unread} remains unmapped");
    let final_counts = pages.counts();
    assert_eq!(
        final_counts,
        (0, full.1, 1, 1),
        "one page returned and one lost page restored"
    );
    clear_output();
    let (new, new_pixels) = colors.record_with(host, heap, slots, |list| {
        record_read(list, heap, &green, output_slot)
    });
    host.execute(&[&new]);
    assert_eq!(output(), [0., 1., 0., 1.]);
    assert_eq!(
        new_pixels.checked_colors(slots, placements()),
        [NULL, GREEN, BLUE]
    );
    eprintln!(
        "B={B} reused physical {original}; demand null, retry green; new A/B/control=null/green/blue; (free, in use, taken back, placed again)={final_counts:?}"
    );
}

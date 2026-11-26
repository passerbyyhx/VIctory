//! Exercise the runtime's page table with owned RGBA8 textures. The small
//! controls cap the available title pages. The reclamation test fills a real
//! registered title heap and runs the production policy at its normal limits.

mod colors;
mod reclaim;
mod registered_textures;
mod translated_reads;

use super::{HostHeap, PAGE, view_pages};
use crate::host::test_submission::while_pending;
use crate::host::{Host, host};
use colors::Colors;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const NULL: [u8; 4] = [0; 4];
const TITLE_SLOTS: u32 = 3 * PAGE;
const A: u32 = 2 * PAGE + 7;
const B: u32 = 7;
// A known-blue SRV on an already placed dynamic page. It costs no title page.
const CONTROL: u32 = TITLE_SLOTS + 7;
const SLOTS: [u32; 3] = [A, B, CONTROL];

fn fixture(available: u32) -> HostHeap {
    let mut heap = HostHeap::new(false, TITLE_SLOTS).expect("a live D3D12 view heap");
    let pages = heap.pages.as_mut().expect("paged heap");
    pages.limit_unused_pages(available);
    heap
}

fn read(host: &Host, heap: &HostHeap, textures: &Colors, slots: [u32; 3]) -> [[u8; 4]; 3] {
    let (list, readback) = textures.record(host, heap, slots);
    let pages = heap.pages.as_ref().unwrap();
    let placements = slots.map(|slot| pages.slot(slot, false));
    host.execute(&[&list]);
    readback.checked_colors(slots, placements)
}

fn report_adapter(host: &Host) {
    // SAFETY: use the LUID of the actual device that owns this test's queue.
    let desc = unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByLuid(host.device.GetAdapterLuid())
            .unwrap();
        adapter.GetDesc1().unwrap()
    };
    let length = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    eprintln!(
        "D3D12 adapter: {}",
        String::from_utf16_lossy(&desc.Description[..length])
    );
}

fn check_control(host: &Host, textures: &Colors) {
    let heap = fixture(2);
    textures.bind_red(host, heap.cpu(A));
    textures.bind_blue(host, heap.cpu(CONTROL));
    let pages = heap.pages.as_ref().unwrap();
    let red = pages.slot(A, false).unwrap();
    let control = pages.slot(CONTROL, false).unwrap();
    let before = read(host, &heap, textures, SLOTS);
    assert_eq!(before, [RED, NULL, BLUE]);

    // Record while B is still unmapped. Submit this same closed list only
    // after mapping B into the second free title page.
    let (closed, readback) = textures.record(host, &heap, SLOTS);
    let at_close = SLOTS.map(|slot| pages.slot(slot, false));
    assert_eq!(at_close, [Some(red), None, Some(control)]);
    eprintln!(
        "at close: virtual A/B/control={SLOTS:?}, physical={at_close:?}, baseline={before:?}"
    );
    textures.bind_blue_as_green(host, heap.cpu(B));
    let green = pages
        .slot(B, false)
        .expect("the second title page is available");
    assert_ne!(red / PAGE, green / PAGE);
    assert_ne!(control / PAGE, green / PAGE);
    let at_submit = SLOTS.map(|slot| pages.slot(slot, false));
    assert_eq!(at_submit, [Some(red), Some(green), Some(control)]);
    eprintln!(
        "before submission: physical={at_submit:?}, pages={:?}",
        [red, green, control].map(|slot| slot / PAGE)
    );

    host.execute(&[&closed]);
    let after = readback.checked_colors(SLOTS, at_submit);
    eprintln!("previously closed list: A, B, control = {after:?}");
    assert_eq!(after, [RED, GREEN, BLUE]);
}

/// Returns true if the production allocator actually recycled A's page for B.
fn check_under_pressure(host: &Host, textures: &Colors) -> bool {
    let heap = fixture(1);
    textures.bind_red(host, heap.cpu(A));
    textures.bind_blue(host, heap.cpu(CONTROL));
    let pages = heap.pages.as_ref().unwrap();
    let original = pages.slot(A, false).unwrap();
    let control = pages.slot(CONTROL, false).unwrap();
    assert_eq!(pages.slot(B, false), None);
    let before = read(host, &heap, textures, SLOTS);
    eprintln!("before gated submission: A, B, control = {before:?}");
    assert_eq!(before, [RED, NULL, BLUE]);

    let (old_list, old_pixels) = textures.record(host, &heap, SLOTS);
    let (pending_slots, pending, completed) = while_pending(host, &old_list, || {
        // The queue has accepted the list, but cannot execute it yet.
        // Only ask the allocator for a page here. Never rewrite an SRV
        // that an outstanding list could still read.
        assert_eq!(pages.slot(B, true), None);
        SLOTS.map(|slot| pages.slot(slot, false))
    });
    assert!(
        pending < completed,
        "the host submission fence must advance"
    );
    assert_eq!(pending_slots, [Some(original), None, Some(control)]);
    eprintln!(
        "while pending: completed fence value={pending}, physical placements A/B/control={pending_slots:?}"
    );
    let old_colors = old_pixels.checked_colors(SLOTS, pending_slots);
    eprintln!(
        "after submission: completed fence value={completed}; old A, B, control = {old_colors:?}"
    );
    assert_eq!(old_colors, [RED, NULL, BLUE]);

    // Also cross the runtime's normal end-of-submission retirement point.
    // Touch only the reserved control slot, leaving A idle for this submission.
    assert_eq!(read(host, &heap, textures, [CONTROL; 3]), [BLUE; 3]);
    // Use the normal write path: an unplaced B writes into the scratch page.
    textures.bind_blue_as_green(host, heap.cpu(B));
    let mapped_b = pages.slot(B, false);
    let after = read(host, &heap, textures, SLOTS);
    eprintln!("after completion and retry: A, B, control = {after:?}; B mapped = {mapped_b:?}");
    if let Some(reused) = mapped_b {
        assert_eq!(reused, original, "only one title page was available");
        assert_eq!(
            pages.slot(A, false),
            None,
            "the old virtual page must be unmapped"
        );
        assert_eq!(after, [NULL, GREEN, BLUE]);
    } else {
        assert_eq!(pages.slot(A, false), Some(original));
        assert_eq!(after, [RED, NULL, BLUE]);
    }
    mapped_b.is_some()
}

#[test]
fn two_colors_across_a_submission_fence() {
    let Some(host) = host() else {
        eprintln!("skipped: no D3D12 device");
        return;
    };
    if !view_pages() {
        eprintln!("skipped: unpaged device; set VICTORY_VIEW_PAGES=1 to exercise paging");
        return;
    }
    report_adapter(host);
    let textures = Colors::new(host);
    check_control(host, &textures);
    let reclaimed = check_under_pressure(host, &textures);
    eprintln!("automatic page reuse observed: {reclaimed}");
}

#[test]
#[ignore = "requires a paged D3D12 device and a runtime that recycles descriptor pages"]
fn expect_reuse_after_submission_fence() {
    expect_reuse(1 << 20, false);
}

#[test]
#[ignore = "requires a paged D3D12 device and a runtime that recycles descriptor pages"]
fn expect_reuse_at_paged_title_limit() {
    expect_reuse(super::PAGED_TITLE_VIEWS, false);
}

#[test]
#[ignore = "requires a paged D3D12 device and a runtime that recycles descriptor pages"]
fn expect_reuse_with_populated_neighbor() {
    expect_reuse(1 << 20, true);
}

fn expect_reuse(title_slots: u32, write_b_neighbor: bool) {
    victory_core::logging::init("victory_gpu::descriptor::pages=info");
    let host = host().expect("D3D12 device required to verify page reuse");
    assert!(view_pages(), "set VICTORY_VIEW_PAGES=1 to exercise paging");
    report_adapter(host);
    reclaim::check(host, title_slots, write_b_neighbor);
}

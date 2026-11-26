use super::registered_textures::RegisteredTextures;
use super::shared_read_window::sample;
use super::{A, B, Host, PAGE, host, report_adapter, view_pages};

fn check(host: &Host, repeat_at_33: bool) {
    let count = 1 << 20;
    let source = RegisteredTextures::new(host, count);
    let heap = source.host_heap();
    let pages = heap.pages.as_ref().unwrap();
    let mapping = || [A, B].map(|slot| heap.page_table_entry(slot / PAGE));
    source.write_red_a();
    let original = mapping()[0];
    assert_ne!(original, 0);
    let fillers: Vec<_> = (0..count / PAGE)
        .filter(|p| ![A / PAGE, B / PAGE].contains(p))
        .take(pages.counts().0 as usize)
        .map(|p| {
            let i = p * PAGE + 13;
            super::super::null_view(heap.cpu(i));
            i
        })
        .collect();
    assert_eq!(pages.counts().0, 0);
    source.write_green(B);
    assert!(pages.pressure_pending());
    assert_eq!(sample(host, heap, A), [1., 0., 0., 1.]);
    // Establish B's slot-read mark while no page is available. A repeated
    // read will mark its page without needing another title descriptor write.
    assert_eq!(sample(host, heap, B), [0.; 4]);
    assert_eq!(mapping(), [original, 0]);
    let start_counts = pages.counts();
    let empty = host.new_list().unwrap();
    empty.close();
    while pages.submission_age(A) < 31 {
        for &slot in &fillers {
            heap.gpu(slot);
        }
        host.execute(&[&empty]);
    }
    assert_eq!(pages.submission_age(A), 31);
    assert!(
        !pages.pressure_pending(),
        "old refused writes no longer press"
    );
    assert_eq!(sample(host, heap, B), [0.; 4]);
    assert_eq!(pages.submission_age(A), 32);
    assert_eq!(mapping(), [original, 0]);
    assert_eq!(pages.counts(), start_counts);
    assert!(
        !pages.pressure_pending(),
        "no deferred scratch write presses age 33"
    );
    eprintln!("age 32: GPU read B, null, donor still mapped; next pass starts unpressed");
    if !repeat_at_33 {
        host.execute(&[&empty]);
        assert_eq!(pages.submission_age(A), 33);
        assert_eq!(mapping(), [original, 0]);
        assert_eq!(pages.counts(), start_counts);
        assert!(!pages.pressure_pending());
        eprintln!("age 33: no GPU demand, donor still mapped; no pending pressure");
    }
    let age = pages.submission_age(A) + 1;
    assert_eq!(sample(host, heap, B), [0.; 4]);
    assert_eq!(mapping(), [0, original]);
    assert_eq!(pages.counts(), (0, start_counts.1, 1, 0, 1));
    assert_eq!(sample(host, heap, B), [0., 1., 0., 1.]);
    eprintln!(
        "age {age}: GPU demand alone restored B, following read green at physical slot {}",
        original * PAGE + B % PAGE
    );
}

#[test]
#[ignore = "run alone on a paged D3D12 device with normal reclamation"]
fn a_repeated_gpu_page_read_presses_only_its_age_pass() {
    let host = host().expect("D3D12 device");
    assert!(view_pages());
    report_adapter(host);
    check(host, true);
    check(host, false);
}

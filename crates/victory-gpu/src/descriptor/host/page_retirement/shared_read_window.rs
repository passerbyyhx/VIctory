//! A descriptor page retired in one heap must not erase another heap's eviction history.
use super::registered_textures::RegisteredTextures;
use super::translated_reads::{record_read, translated_load};
use super::{A, B, Host, HostHeap, PAGE, host, report_adapter, view_pages};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;

pub(super) fn sample(host: &Host, heap: &HostHeap, index: u32) -> [f32; 4] {
    let output = heap.count() + 7;
    let (buffer, data) = host.shared_buffer(16).unwrap();
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
    // SAFETY: the pinned output slot and buffer stay live through the dispatch.
    unsafe {
        host.device
            .CreateUnorderedAccessView(&buffer, None, Some(&desc), heap.cpu(output));
        data.cast::<[f32; 4]>().write([f32::NAN; 4]);
    }
    let list = host.new_list().unwrap();
    list.defer_heap(&heap.viewed);
    let pipeline = translated_load(index);
    record_read(&list, heap, &pipeline, output);
    list.close();
    host.execute(&[&list]);
    // SAFETY: the dispatch finished before reading its mapped output.
    unsafe { data.cast::<[f32; 4]>().read() }
}

fn check(host: &Host, shared_record: bool, donor: u32) -> (bool, bool) {
    let count = 1 << 20;
    let crowded = RegisteredTextures::new(host, count);
    let other = RegisteredTextures::new(host, 3 * PAGE);
    let h1 = crowded.host_heap();
    let h2 = other.host_heap();
    let probe = A + 42;
    let pressure = count - PAGE + 7;
    crowded.write_green(donor);
    if shared_record {
        crowded.write_yellow(probe);
    }
    other.write_yellow(probe);
    let pages = h1.pages.as_ref().unwrap();
    let original = h1.page_table_entry(donor / PAGE);
    let other_page = h2.page_table_entry(probe / PAGE);
    assert_ne!(original, 0);
    assert_ne!(other_page, 0);
    let fillers: Vec<_> = (0..count / PAGE)
        .filter(|p| ![A / PAGE, donor / PAGE, B / PAGE, pressure / PAGE].contains(p))
        .take(pages.counts().0 as usize)
        .map(|p| {
            let slot = p * PAGE + 13;
            super::super::null_view(h1.cpu(slot));
            slot
        })
        .collect();
    assert_eq!(pages.counts().0, 0);
    crowded.write_green(B);
    assert_eq!(h1.page_table_entry(B / PAGE), 0);

    crate::shadow::reads_since();
    assert_eq!(sample(host, h2, probe), [1., 1., 0., 1.]);
    let owner = h2.textures_at(&[probe])[0];

    let idle = host.new_list().unwrap();
    idle.close();
    while pages.submission_age(donor) < 32 {
        if donor / PAGE != probe / PAGE {
            h1.gpu(probe);
        }
        for &i in &fillers {
            h1.gpu(i);
        }
        host.execute(&[&idle]);
    }
    crowded.write_green(pressure);
    assert_eq!(sample(host, h1, B), [0.; 4]);
    assert_eq!(h1.page_table_entry(donor / PAGE), 0);
    if donor / PAGE != probe / PAGE {
        assert!(h1.kept(probe), "the H1 probe's own page stayed resident");
    }
    assert_eq!(h1.page_table_entry(B / PAGE), original);
    host.execute(&[&idle]);
    assert_eq!(h2.page_table_entry(probe / PAGE), other_page);
    assert_eq!(h2.textures_at(&[probe]), [owner]);

    let window = crate::shadow::reads_since();
    let named = crate::descriptor::read_textures(&window);
    assert!(
        crate::shadow::reads_since().is_empty(),
        "eviction window consumed once"
    );
    let result = (window.contains(&probe), named.contains(&owner));
    eprintln!(
        "H1 shared record={shared_record}, retired H1 page={}, H2 page={other_page}, H2 yellow slot={probe}, window={window:?}, H2 slot and owner found={result:?}",
        donor / PAGE
    );
    result
}

#[test]
#[ignore = "run alone on a paged D3D12 device with reclamation"]
fn a_retired_heap_preserves_a_second_heaps_read_window() {
    let host = host().expect("D3D12 device");
    assert!(view_pages(), "requires paged view heaps");
    report_adapter(host);
    let unshared = check(host, false, A);
    let unrelated_page = check(host, true, A + PAGE);
    let shared = check(host, true, A);
    assert_eq!(unshared, (true, true));
    assert_eq!(unrelated_page, (true, true));
    assert_eq!(shared, (true, true));
}

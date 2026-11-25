//! Read the virtual texture with translated GFX10 and collect the runtime's read marks.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::super::HostHeap;
use super::{A, B, BLUE, CONTROL, Colors, RED, fixture, host, report_adapter, view_pages};
use crate::descriptor::tests::WHILE_MAKING;
use crate::host::HostList;
use crate::pipeline::{Translated, host::HostPipeline};
use victory_shader::emit::{VIEW_STRIDES, emit_with};
use victory_shader::native::{ComputeIo, StageIo};
use victory_shader::{
    Container, HwStage, NativeProgram, ResourceType, RootParam, RootType, UsedResource,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS;

/// The texture is in the resource heap. A table UAV receives the four float channels.
fn translated_load(slot: u32) -> HostPipeline {
    let mut code = vec![
        0xbe81_0380, // s_mov_b32 s1, 0
        0xf408_0200,
        0xfa00_0000, // s_load_dwordx4 s[8:11], s[0:1], 0 (output table)
        0xbe85_0380, // s_mov_b32 s5, 0
        0xf40c_0302,
        0xfa00_0000 | (slot * 32), // s_load_dwordx8 s[12:19], s[4:5], slot*32
        0xbf8c_0000,               // s_waitcnt
        0x7e00_0280,
        0x7e02_0280, // v[0:1] = (0,0)
        0xf000_1f08,
        0x0003_0200, // image_load v[2:5], v[0:1], s[12:19]
        0xbf8c_0000, // s_waitcnt
    ];
    for i in 0..4 {
        // buffer_store_dword v(2+i), off, s[8:11], 0 offset:4*i
        code.extend([0xe070_0000 | (4 * i), 0x8002_0000 | ((2 + i) << 8)]);
    }
    code.push(0xbf81_0000); // s_endpgm
    let param = |root, user_data, slot| RootParam {
        root,
        user_data,
        slot,
        sgprs: 1,
        base_register: 0,
        table_offset_dwords: 0,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![
            param(RootType::Table, 0, 0),
            param(RootType::ResourceHeap, 4, 1),
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 0,
            slot: 0,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [1, 1, 1],
        }),
        code: code.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let (p, structured) =
        victory_shader::range::build(&native.code, native.wave64, native.vgprs).unwrap();
    let st = structured.unwrap();
    let res = victory_shader::resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
    let io = victory_shader::io::raise(&p, &native).unwrap();
    let container = Container {
        digest: [0; 16],
        version: (1, 0),
        parts: vec![],
    };
    let options = crate::shader_cache::options();
    assert!(options.view_pages && options.view_marks);
    let host = emit_with(&container, &native, &p, &st, &io, &res, options).unwrap();
    assert!(host.heap && host.view_marks && !host.memory);
    assert_eq!(host.root_srvs, [VIEW_STRIDES]);
    crate::pipeline::test_compute_stage(Translated {
        host,
        used: native.used,
    })
}

const OUTPUT: u32 = CONTROL + 1;

fn record_read(list: &HostList, heap: &HostHeap, pipeline: &HostPipeline) {
    let stage = &pipeline.stages[0];
    let (constants, size) = stage.user_data.unwrap();
    let mut user_data = vec![0; size as usize];
    for u in &stage.translated().host.user_data {
        assert_eq!((u.root, u.slot), (RootType::Table, 0));
        user_data[u.index as usize] = OUTPUT * 32;
    }
    let [(VIEW_STRIDES, stride_param)] = stage.root_srvs[..] else {
        panic!("expected view-page table")
    };
    let (mark_table, bitmap) = pipeline.marks.unwrap();
    // SAFETY: the caller keeps the heap, pipeline, textures, and output alive through completion.
    unsafe {
        list.list.SetDescriptorHeaps(&[Some(heap.heap.clone())]);
        list.list.SetComputeRootSignature(&pipeline.root);
        list.list
            .SetComputeRoot32BitConstants(constants, size, user_data.as_ptr().cast(), 0);
        list.list.SetComputeRootShaderResourceView(
            stride_param,
            heap.strides().unwrap().GetGPUVirtualAddress(),
        );
        list.list.SetComputeRootShaderResourceView(
            mark_table,
            heap.marks().unwrap().GetGPUVirtualAddress(),
        );
        list.list
            .SetComputeRootUnorderedAccessView(bitmap, crate::shadow::view_writes(false).unwrap());
        list.list.SetPipelineState(&pipeline.pso);
        list.list.Dispatch(1, 1, 1);
    }
}

#[test]
#[ignore = "run alone on paged D3D12 hardware; observes process-wide read windows"]
fn translated_texture_reports_its_virtual_slot_in_each_read_window() {
    let host = host().expect("a D3D12 device for this opt-in test");
    assert!(view_pages(), "enable view paging for this experiment");
    report_adapter(host);
    let heap = fixture(2);
    let textures = Colors::new(host);
    textures.bind_red(host, heap.cpu(A));
    textures.bind_blue(host, heap.cpu(B));
    let page = heap.pages.as_ref().unwrap();
    let physical = [A, B, OUTPUT].map(|slot| page.slot(slot, false).unwrap());
    let all: std::collections::BTreeSet<_> = [A, B, OUTPUT].into_iter().chain(physical).collect();
    assert_eq!(
        all.len(),
        6,
        "each virtual and physical slot must be distinct"
    );
    eprintln!(
        "virtual A/B/output={:?}, physical={physical:?}",
        [A, B, OUTPUT]
    );
    let red = translated_load(A);
    let blue = translated_load(B);
    let (output, mapped) = host.shared_buffer(16).unwrap();
    heap.set_mark(OUTPUT, None);
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
    // SAFETY: the writable slot and its four-dword buffer stay live through all waits.
    unsafe {
        host.device
            .CreateUnorderedAccessView(&output, None, Some(&desc), heap.cpu(OUTPUT))
    };
    // Start a fresh read epoch. The runtime drains the forgotten slot on this empty submission.
    crate::shadow::forget_read(A);
    crate::shadow::forget_read(B);
    let empty = host.new_list().unwrap();
    empty.defer_after(|| {
        crate::shadow::reads_since();
    });
    empty.close();
    host.execute(&[&empty]);
    for (label, read, first) in [
        ("first A", Some((A, &red, RED)), true),
        ("empty", None, false),
        ("repeat A", Some((A, &red, RED)), false),
        ("first B", Some((B, &blue, BLUE)), true),
    ] {
        let list = host.new_list().unwrap();
        // SAFETY: the queue is idle. Initialize a sentinel to distinguish an empty window.
        unsafe { mapped.cast::<[u32; 4]>().write([0xcdcd_cdcd; 4]) };
        if let Some((_, pipeline, _)) = read {
            record_read(&list, &heap, pipeline);
        }
        let new = Arc::new(AtomicBool::new(false));
        let seen = new.clone();
        *WHILE_MAKING.lock().unwrap() = Some((
            read.map_or(A, |r| r.0),
            Box::new(move || seen.store(true, Ordering::Relaxed)),
        ));
        let window = Arc::new(OnceLock::new());
        let result = window.clone();
        // The normal submission has already called take_reads and make_read; close our
        // window under the same submission lock, before the next eviction pass.
        list.defer_after(move || {
            result.set(crate::shadow::reads_since()).unwrap();
        });
        list.close();
        host.execute(&[&list]);
        WHILE_MAKING.lock().unwrap().take();
        let slots = window.get().unwrap();
        let reported_new = new.load(Ordering::Relaxed);
        assert_eq!(reported_new, first, "{label}: newly reported");
        assert_eq!(slots, read.map(|r| r.0).as_slice());
        // SAFETY: the completed GPU submission owns no pending writes to this buffer.
        let words = unsafe { mapped.cast::<[u32; 4]>().read() };
        if let Some((_, _, expected)) = read {
            let rgba = words.map(f32::from_bits);
            assert_eq!(
                rgba,
                expected.map(|c| f32::from(c) / 255.0),
                "{label}: translated texture color"
            );
        } else {
            assert_eq!(words, [0xcdcd_cdcd; 4]);
        }
        eprintln!(
            "{label}: output f32 bits={words:08x?}, newly reported={reported_new}, read window={slots:?}"
        );
    }
}

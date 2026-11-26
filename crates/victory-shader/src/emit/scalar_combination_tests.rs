use super::*;
use windows::Win32::Graphics::Dxgi::*;

const RED: u32 = 0xff00_00ff;
const GREEN: u32 = 0xff00_ff00;
const SENTINEL: u32 = 0xdead_beef;
const TRAILER: u32 = 0xabad_1dea;
const EXPECTED: [u32; 15] = [
    RED, GREEN, 0, 0, 0, RED, GREEN, 0, RED, GREEN, 0, 0, 0, 0, SENTINEL,
];
const LABELS: [&str; 15] = [
    "heap.red",
    "heap.green",
    "heap.sum-carry",
    "heap.wide.first",
    "heap.wide.carry",
    "CODE.red",
    "CODE.green",
    "CODE.limit",
    "CODE.independent-alignment",
    "CODE.wide.last-valid",
    "CODE.wide.first-invalid",
    "CODE.sum-carry",
    "CODE.wide.high",
    "CODE.wide.carry",
    "unwritten",
];

/// One invocation reads the heap constant buffer and the getpc-relative CODE
/// table, then writes one dword per observation into a host-prefilled UAV.
fn fixture(omit_sum_store: bool) -> HostShader {
    let mut words = vec![
        0xbe98_1f00, // s_getpc_b64 s[24:25]
        0x801c_ff18,
        0, // s_add_u32 s28, s24, table start - 4 + 2 (unaligned base)
        0x881d_ff19,
        4 << 16,     // s_or_b32 s29, s25, stride 4
        0xbe9e_0385, // s30 = NUM_RECORDS 5 (20 bytes)
        0xbe9f_03ff,
        0x3101_6fac, // s31 = descriptor flags
    ];
    // Bind s[8:11] to the heap input and s[16:19] to the table output.
    words.extend_from_slice(&HEAP_CONSTANT_WAVE32[..10]);
    words.extend([
        0x8f04_8100, // s4 = heap index 2 << 1
        0x8085_0480, // s5 = 0 - s4 = 0xffff_fffc
        0x8032_8c00, // s50 = s0 + 12 = 0xe (runtime scalar)
        0xbf8c_c07f,
    ]);
    let load = |op: u32, dst: u32, base: u32, so: u32, imm: u32| {
        [
            0xf400_0000u32 | op << 18 | dst << 6 | base >> 1,
            so << 25 | imm,
        ]
    };
    for (op, dst, base, so, imm) in [
        (8, 32, 8, 125, 0),
        (8, 33, 8, 125, 4),
        (8, 34, 8, 5, 8),
        (9, 35, 8, 5, 0),
        (8, 37, 28, 125, 12),
        (8, 38, 28, 125, 16),
        (8, 39, 28, 125, 20),
        (8, 40, 28, 50, 2),
        (9, 41, 28, 125, 16),
        (8, 43, 28, 5, 8),
        (9, 44, 28, 5, 0),
    ] {
        words.extend(load(op, dst, base, so, imm));
    }
    words.push(0xbf8c_c07f);
    for k in 0..14 {
        words.push(0x7e02_0200 | (32 + k)); // v1 = s[32+k]
        if omit_sum_store && k == 2 {
            words.extend([0xbf80_0000; 2]); // two s_nops preserve the table address
        } else {
            words.extend([0xe070_0000 | (4 * k), 0x8004_0100]); // store v1 through s[16:19]
        }
    }
    words.push(0xbf81_0000);
    words[2] = words.len() as u32 * 4 - 4 + 2;
    words.extend([0xc0de_0000, 0xc0de_0001, 0xc0de_0002, RED, GREEN, TRAILER]);
    build(
        &words,
        vec![
            param(RootType::Constants, 0),
            param(RootType::ResourceHeap, 1),
            param(RootType::Table, 2),
        ],
        vec![entry(ResourceType::Uav, 4, 2)],
        1,
    )
}

fn run(test: &str, omit_sum_store: bool, verify: impl Fn(&[u32]) -> bool) {
    let shader = fixture(omit_sum_store);
    let _lock = super::super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut fails = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let host = Host::new(warp).expect("both requested adapters must be available");
        // SAFETY: device LUID identifies a live DXGI adapter. Its description is POD.
        let desc = unsafe {
            let factory =
                CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            factory
                .EnumAdapterByLuid::<IDXGIAdapter1>(host.device.GetAdapterLuid())
                .unwrap()
                .GetDesc1()
                .unwrap()
        };
        let name = String::from_utf16_lossy(&desc.Description);
        let name = name.trim_end_matches('\0');
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        assert_eq!(software, warp, "{label}: {name}");
        eprintln!(
            "ADAPTER {label} {name}, vendor={:04x}, flags={:#x}, LUID={:08x}:{:08x}",
            desc.VendorId, desc.Flags, desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart
        );
        let (root, pso) = host.compute_pipeline(&shader).expect("pipeline");
        let mut ud = vec![0; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            ud[u.index as usize] = match u.root {
                RootType::Constants => 2, // heap input at index 2
                RootType::Table => 32,    // table points at UAV index 1
                other => panic!("unexpected {other:?}"),
            };
        }
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![
                        (0, Root::Constants(ud)),
                        (
                            1,
                            Root::View {
                                srv: true,
                                data: vec![4; 3],
                            },
                        ),
                    ],
                    views: &[
                        View::Memory(&[SENTINEL; 15]),
                        View::Pair,
                        View::Buffer {
                            data: &[RED, GREEN, TRAILER],
                            stride: Stride::Raw,
                        },
                    ],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .expect("dispatch");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), LABELS.len());
        for ((field, expected), got) in LABELS.iter().zip(EXPECTED).zip(&out[0]) {
            eprintln!("ROW {test} {label} {field} expected={expected:08x} observed={got:08x}");
        }
        if !verify(&out[0]) {
            fails.push(label);
        }
    }
    assert!(fails.is_empty(), "{test} mismatched on {fails:?}");
}

#[test]
fn heap_range_and_carry() {
    run("heap", false, |out| {
        out[..5] == EXPECTED[..5] && out[14] == SENTINEL
    });
}

#[test]
fn code_num_records_alignment_and_carry() {
    run("code", false, |out| out[5..] == EXPECTED[5..]);
}

#[test]
fn omitted_zero_store_leaves_sentinel() {
    // All other words, including both nonzero heap reads, must still match.
    let mut omitted = EXPECTED;
    omitted[2] = SENTINEL;
    run("omitted", true, |out| out == omitted && out != EXPECTED);
}

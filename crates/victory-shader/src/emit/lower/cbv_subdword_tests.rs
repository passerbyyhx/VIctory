//! Regression tests for sub-dword `buffer_load_{ubyte,sbyte,ushort,sshort}`
//! instructions reading from a constant buffer (`ResourceType::Cbv`), both
//! table-bound (raw view) and root-descriptor-bound (`cbufferLoadLegacy`).

use super::super::host::{Dispatch, Host, Root, Stride, View};
use super::super::{HostShader, StaticSampler, validator};
use super::{Parts, lower};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

/// Encodes a wave32 compute shader that reads a sub-dword field from a CBV
/// (either a table CBV at user data `s0` or a root CBV descriptor at `s[0:1]`)
/// at byte offset `byte_offset + (lane << step_shift)` and
/// stores the resulting 32-bit zero- or sign-extended value to dword `lane` of
/// the raw UAV at table user data `s2`.
fn subdword_cbv_program(
    mubuf_op: u32,
    step_shift: u32,
    table_cbv: bool,
    byte_offset: u32,
) -> Vec<u32> {
    let mut w = Vec::new();
    if table_cbv {
        // s[8:11] = table[0] via s0
        w.extend([
            0xBE84_0300, // s_mov_b32 s4, s0
            0xBE85_0380, // s_mov_b32 s5, 0
            0xF408_0202,
            0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        ]);
    } else {
        // Build V# in s[8:11] directly around the 64-bit root CBV pointer in s[0:1].
        w.extend([
            0xBE88_0300, // s_mov_b32 s8, s0
            0xBE89_0301, // s_mov_b32 s9, s1
            0xBE8A_03C0, // s_mov_b32 s10, 64
            0xBE8B_03FF,
            0x0002_0000, // s_mov_b32 s11, 0x0002_0000
        ]);
    }
    // s[16:19] = UAV table[0] via s2
    w.extend([
        0xBE8C_0302, // s_mov_b32 s12, s2
        0xBE8D_0380, // s_mov_b32 s13, 0
        0xF408_0406,
        0xFA00_0000, // s_load_dwordx4 s[16:19], s[12:13], 0x0
    ]);
    // v1 = v0 << step_shift (byte offset: lane * 1 or lane * 2)
    w.push(0x3402_0080 | step_shift); // v_lshlrev_b32 v1, step_shift, v0
    w.push(0x7E04_0300); // v_mov_b32 v2, v0 (initialized for store/atomic tests)
    w.push(0xBF8C_C07F); // s_waitcnt lgkmcnt(0)
    // buffer_load_{ubyte,sbyte,ushort,sshort} v2, v1, s[8:11], 0 offen
    assert!(byte_offset <= 0xfff);
    w.push(0xE000_1000 | (mubuf_op << 18) | byte_offset);
    w.push(0x8002_0201);
    w.push(0xBF8C_3F70); // s_waitcnt vmcnt(0)
    w.push(0x3406_0082); // v_lshlrev_b32 v3, 2, v0
    w.push(0xE070_1000);
    w.push(0x8004_0203); // buffer_store_dword v2, v3, s[16:19], 0 offen
    w.push(0xBF81_0000); // s_endpgm
    w
}

fn try_lower_shader(
    mubuf_op: u32,
    step_shift: u32,
    table_cbv: bool,
    threads: u32,
    byte_offset: u32,
) -> Result<HostShader, ShaderError> {
    let words = subdword_cbv_program(mubuf_op, step_shift, table_cbv, byte_offset);
    let cbv_param = if table_cbv {
        RootParam {
            root: RootType::Table,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: 0,
            slot: 0,
        }
    } else {
        RootParam {
            root: RootType::Descriptor,
            table_offset_dwords: 0,
            sgprs: 2,
            base_register: 0,
            user_data: 0,
            slot: 0,
        }
    };
    let cbv_used = UsedResource {
        kind: ResourceType::Cbv,
        root: cbv_param.root,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride: 0,
        dwords: 4,
        user_data: 0,
        slot: 0,
    };
    let uav_param = RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data: 2,
        slot: 2,
    };
    let uav_used = UsedResource {
        kind: ResourceType::Uav,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride: 0,
        dwords: 4,
        user_data: 2,
        slot: 2,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![cbv_param, uav_param],
        used: vec![cbv_used, uav_used],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 3 << 1,
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
    let lowered = lower(&Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
        view_marks: false,
    })?;
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        static_samplers: lowered
            .samplers
            .iter()
            .map(|&w| StaticSampler::from_words(w))
            .collect(),
        samplers: lowered.samplers,
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        memory: lowered.memory,
        memory_writes: lowered.memory_writes,
        view_addresses: lowered.view_addresses,
        heap: lowered.heap,
        heap_writes: lowered.heap_writes,
        view_marks: lowered.view_marks,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
    Ok(shader)
}

fn build_shader(mubuf_op: u32, step_shift: u32, table_cbv: bool, threads: u32) -> HostShader {
    try_lower_shader(mubuf_op, step_shift, table_cbv, threads, 0).expect("lower")
}

#[test]
fn cbv_store_and_atomic_are_refused() {
    for table_cbv in [true, false] {
        let mode = if table_cbv { "table_cbv" } else { "root_cbv" };
        for (name, op) in [
            ("buffer_store_dword", 0x01c),
            ("buffer_atomic_add", 0x032),
        ] {
            let err = try_lower_shader(op, 2, table_cbv, 4, 0)
                .expect_err(&format!("{mode} {name}: expected CBV write to be refused"));
            assert!(
                matches!(
                    err,
                    ShaderError::Unsupported {
                        what: "a store or atomic on a constant buffer",
                        ..
                    }
                ),
                "{mode} {name}: expected Unsupported(\"a store or atomic on a constant buffer\"), got {err:?}"
            );
        }
    }
}

#[test]
fn cbv_subdword_dxil_distinguishes_width_and_signedness() {
    for table_cbv in [true, false] {
        let mode = if table_cbv { "table_cbv" } else { "root_cbv" };
        let ubyte = build_shader(0x008, 0, table_cbv, 16).dxil;
        let sbyte = build_shader(0x009, 0, table_cbv, 16).dxil;
        let ushort = build_shader(0x00a, 1, table_cbv, 8).dxil;
        let sshort = build_shader(0x00b, 1, table_cbv, 8).dxil;
        assert_ne!(ubyte, sbyte, "{mode}: ubyte and sbyte must differ");
        assert_ne!(ushort, sshort, "{mode}: ushort and sshort must differ");
        assert_ne!(ubyte, ushort, "{mode}: ubyte and ushort must differ");
        assert_ne!(sbyte, sshort, "{mode}: sbyte and sshort must differ");
    }
}

#[test]
fn cbv_subdword_loads_extract_and_extend_bytes_and_shorts() {
    let cbv_data: [u32; 4] = [0x80FF_7F01, 0xFE02_817E, 0xA55A_00FF, 0x7FFF_8000];
    let raw_bytes: Vec<u8> = cbv_data.iter().flat_map(|w| w.to_le_bytes()).collect();

    // (name, MUBUF opcode, step_shift, threads, expected output per lane)
    let cases: [(&str, u32, u32, u32, Vec<u32>); 4] = [
        (
            "buffer_load_ubyte",
            0x008,
            0,
            16,
            raw_bytes.iter().map(|&b| u32::from(b)).collect(),
        ),
        (
            "buffer_load_sbyte",
            0x009,
            0,
            16,
            raw_bytes.iter().map(|&b| (b as i8) as i32 as u32).collect(),
        ),
        (
            "buffer_load_ushort",
            0x00a,
            1,
            8,
            raw_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u32::from(u16::from_le_bytes(c)))
                .collect(),
        ),
        (
            "buffer_load_sshort",
            0x00b,
            1,
            8,
            raw_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| i16::from_le_bytes(c) as i32 as u32)
                .collect(),
        ),
    ];

    let _one = super::super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut hosts = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        if let Ok(host) = Host::new(warp) {
            hosts.push((label, host));
        }
    }
    assert!(
        !hosts.is_empty(),
        "neither hardware nor WARP SM 6.6 device available"
    );

    for table_cbv in [true, false] {
        let mode = if table_cbv { "table_cbv" } else { "root_cbv" };
        for (op_name, mubuf_op, step_shift, threads, want) in &cases {
            let shader = build_shader(*mubuf_op, *step_shift, table_cbv, *threads);
            for (label, host) in &hosts {
                let words = read_cbv(host, &shader, &cbv_data, table_cbv, *threads)
                    .unwrap_or_else(|e| panic!("{label} {mode} {op_name}: {e}"));
                assert_eq!(words, *want, "{label} {mode} {op_name}");
            }
        }
    }
}

fn read_cbv(
    host: &Host,
    shader: &HostShader,
    data: &[u32],
    table: bool,
    output_words: u32,
) -> Result<Vec<u32>, String> {
    let mut ud = vec![0; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        ud[u.index as usize] = match (u.root, u.slot) {
            (RootType::Table, 2) if table => 32,
            (RootType::Table, 0 | 2) => 0,
            other => panic!("unexpected user data {other:?}"),
        };
    }
    let mut roots = vec![(0, Root::Constants(ud))];
    let mut views = Vec::new();
    if table {
        views.push(View::Buffer {
            data,
            stride: Stride::Raw,
        });
    } else {
        roots.push((
            1,
            Root::View {
                srv: false,
                data: data.to_vec(),
            },
        ));
    }
    views.push(View::RawUav(output_words));
    let (root, pso) = host.compute_pipeline(shader)?;
    let mut output = host.dispatch(
        &root,
        &pso,
        &Dispatch {
            roots,
            views: &views,
            samplers: &[],
            groups: [1, 1, 1],
        },
    )?;
    Ok(output.remove(0))
}

/// Observe an odd halfword inside a 256-byte CBV. The aligned lanes and the untouched
/// output word are controls. The value at offset 3 is reported for comparison across
/// implementations because the console's SH_MEM_CONFIG.alignment_mode is unknown.
#[test]
#[ignore = "readback diagnostic on hardware and WARP; run with --ignored --nocapture"]
fn observe_interior_halfword_offsets() {
    use windows::Win32::Graphics::Dxgi::*;
    let _one = super::super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut data = [0x5a5a_a5a5u32; 64];
    data[0] = 0x8134_a50f;
    let mut validator = validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("Windows SDK DXIL validator");
    for warp in [false, true] {
        let host = Host::new(warp).expect("required D3D12 adapter");
        // SAFETY: the query uses the LUID of the device that will submit the dispatch.
        let desc = unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapterByLuid(host.device.GetAdapterLuid())
                .unwrap();
            adapter.GetDesc1().unwrap()
        };
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        assert_eq!(software, warp, "device identity");
        let len = desc
            .Description
            .iter()
            .position(|&x| x == 0)
            .unwrap_or(desc.Description.len());
        eprintln!(
            "device={} {:04x}:{:04x} software={software}",
            String::from_utf16_lossy(&desc.Description[..len]),
            desc.VendorId,
            desc.DeviceId
        );
        for byte_four in [0x92, 0x16] {
            data[1] = 0x117e_5e00 | byte_four;
            for table in [true, false] {
                for (op, name) in [(0x00a, "ushort"), (0x00b, "sshort")] {
                    // 3 active lanes, byte offset = lane + 2: only reads 2..=5 of 256.
                    let shader = try_lower_shader(op, 0, table, 3, 2).unwrap();
                    validator
                        .validate(&mut shader.dxil.clone())
                        .expect("valid translated DXIL");
                    let words = read_cbv(&host, &shader, &data, table, 4).unwrap();
                    eprintln!(
                        "byte[4]={byte_four:02x} {} {name} offsets [2,3,4] = {:08x?}; output guard = {:08x}",
                        if table { "table" } else { "root" },
                        &words[..3],
                        words[3]
                    );
                    let at_two = if name == "sshort" {
                        0xffff_8134
                    } else {
                        0x8134
                    };
                    assert_eq!(
                        (words[0], words[2], words[3]),
                        (at_two, 0x5e00 | byte_four, 0),
                        "aligned controls and guard"
                    );
                }
            }
        }
    }
}

//! Unit tests for partial-channel typed and indexed sub-dword buffer stores.

use super::tests::{S_ENDPGM, lower_with};
use crate::ShaderError;
use crate::native::{ResourceType, RootParam, RootType, UsedResource};

fn table_uav_layout() -> Vec<RootParam> {
    vec![RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data: 0,
        slot: 0,
    }]
}

fn table_uav_used(stride: u32) -> Vec<UsedResource> {
    vec![UsedResource {
        kind: ResourceType::Uav,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride,
        dwords: 4,
        user_data: 0,
        slot: 0,
    }]
}

#[test]
fn partial_typed_and_indexed_subword_buffer_stores_lower_and_validate() {
    let layout = table_uav_layout();
    // Partial-channel typed buffer stores (32-bit x/xy/xyz, d16 x/xy/xyz, d16_hi_x).
    for op in [0x004u32, 0x005, 0x006, 0x084, 0x085, 0x086, 0x027] {
        let words = [
            0xBE84_0300,              // s_mov_b32 s4, s0
            0xBE85_0380,              // s_mov_b32 s5, 0
            0xF408_0202,
            0xFA00_0000,              // s_load_dwordx4 s[8:11], s[4:5], 0x0
            0x7E02_02F2,              // v_mov_b32 v1, 1.0
            0x7E04_02F0,              // v_mov_b32 v2, 0.5
            0x7E06_0280,              // v_mov_b32 v3, 0
            0xBF8C_C07F,              // s_waitcnt lgkmcnt(0)
            0xE000_2000 | (op << 18), // buffer_store_format_* v[1..], v0, s[8:11], 0 idxen
            0x8002_0100,
            S_ENDPGM,
        ];
        let emitted = lower_with(&words, layout.clone(), table_uav_used(0))
            .unwrap_or_else(|e| panic!("typed store op {op:#x} failed: {e}"));
        assert!(!emitted.dxil.is_empty());
        #[cfg(windows)]
        if let Ok(mut v) = crate::emit::validator::Validator::load(
            crate::emit::validator::Validator::default_path().as_deref(),
        ) {
            v.validate(&mut emitted.dxil.clone())
                .unwrap_or_else(|e| panic!("typed store op {op:#x} invalid DXIL: {e}"));
        }
    }
    // Indexed byte/short (and _d16_hi) stores into a structured UAV (stride 8).
    for (op, off) in [(0x018u32, 1u32), (0x019, 3), (0x01a, 2), (0x01b, 6)] {
        let words = [
            0xBE84_0300,                    // s_mov_b32 s4, s0
            0xBE85_0380,                    // s_mov_b32 s5, 0
            0xF408_0202,
            0xFA00_0000,                    // s_load_dwordx4 s[8:11], s[4:5], 0x0
            0x7E02_0285,                    // v_mov_b32 v1, 5
            0xBF8C_C07F,                    // s_waitcnt lgkmcnt(0)
            0xE000_2000 | (op << 18) | off, // buffer_store_{byte,short}* v1, v0, s[8:11], 0 idxen offset:off
            0x8002_0100,
            S_ENDPGM,
        ];
        let emitted = lower_with(&words, layout.clone(), table_uav_used(8))
            .unwrap_or_else(|e| panic!("indexed subword store op {op:#x} failed: {e}"));
        assert!(!emitted.dxil.is_empty());
        #[cfg(windows)]
        if let Ok(mut v) = crate::emit::validator::Validator::load(
            crate::emit::validator::Validator::default_path().as_deref(),
        ) {
            v.validate(&mut emitted.dxil.clone())
                .unwrap_or_else(|e| panic!("indexed subword store op {op:#x} invalid DXIL: {e}"));
        }
    }
}

#[test]
fn typed_buffer_store_refuses_nonzero_soffset() {
    let words = [
        0xBE84_0300,                 // s_mov_b32 s4, s0
        0xBE85_0380,                 // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000,                 // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0x7E02_02F2,                 // v_mov_b32 v1, 1.0
        0xBF8C_C07F,                 // s_waitcnt lgkmcnt(0)
        0xE000_2000 | (0x004 << 18), // buffer_store_format_x v1, v0, s[8:11], 4 idxen
        0x8402_0100,
        S_ENDPGM,
    ];
    assert!(matches!(
        lower_with(&words, table_uav_layout(), table_uav_used(0)),
        Err(ShaderError::Unsupported {
            what: "a typed buffer store at an offset",
            ..
        })
    ));
}

#[test]
fn half_buffer_store_refuses_static_odd_offset() {
    let words = [
        0xBE84_0300,                     // s_mov_b32 s4, s0
        0xBE85_0380,                     // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000,                     // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0x7E02_0285,                     // v_mov_b32 v1, 5
        0xBF8C_C07F,                     // s_waitcnt lgkmcnt(0)
        0xE000_2000 | (0x01a << 18) | 1, // buffer_store_short v1, v0, s[8:11], 0 idxen offset:1
        0x8002_0100,
        S_ENDPGM,
    ];
    assert!(matches!(
        lower_with(&words, table_uav_layout(), table_uav_used(8)),
        Err(ShaderError::Unsupported {
            what: "an unaligned half buffer store",
            ..
        })
    ));
}

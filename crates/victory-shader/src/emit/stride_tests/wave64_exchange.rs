use super::*;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FEATURE_D3D12_OPTIONS1, D3D12_FEATURE_DATA_D3D12_OPTIONS1,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
};

/// An actual Wave64 guest group exchanges the two halves of its LDS array.
/// Every lane writes a distinct value before loading the other half's value.
#[test]
#[ignore = "standalone D3D12 hardware and WARP readback"]
fn wave64_lds_exchange_reads_every_partner() {
    let _guard = super::super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let host = Host::new(warp).expect("SM 6.6 test adapter");
        let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
        // SAFETY: capabilities are read into a sized output, and the adapter
        // description is obtained from the same device that executes the shader.
        let desc = unsafe {
            host.device
                .CheckFeatureSupport(
                    D3D12_FEATURE_D3D12_OPTIONS1,
                    (&raw mut caps).cast(),
                    size_of_val(&caps) as u32,
                )
                .unwrap();
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapterByLuid(host.device.GetAdapterLuid())
                .unwrap();
            adapter.GetDesc1().unwrap()
        };
        let end = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..end]);
        eprintln!(
            "{label} adapter={name} wave caps={}..{}",
            caps.WaveLaneCountMin, caps.WaveLaneCountMax
        );
        for threads in [32u32, 64] {
            let half = threads / 2;
            let words = [
                0xBE84_0300,
                0xBE85_0380,
                0xF408_0202,
                0xFA00_0000, // SRV s[8:11] from s0
                0xBE8C_0301,
                0xBE8D_0380,
                0xF408_0406,
                0xFA00_0000,        // UAV s[16:19] from s1
                0x3402_0082,        // v_lshlrev_b32 v1, 2, v0 (own byte address)
                0x3A06_0080 | half, // v_xor_b32 v3, half, v0 (partner lane)
                0x3406_0682,        // v_lshlrev_b32 v3, 2, v3 (partner byte address)
                0xBF8C_C07F,        // wait for descriptors
                0xE030_1000,
                0x8002_0201, // buffer_load_dword v2, v1, s[8:11]
                0xBF8C_3F70, // wait for input
                0xD834_0000,
                0x0000_0201, // ds_write_b32 v1, v2
                0xD8D8_0000,
                0x0400_0003, // ds_read_b32 v4, v3
                0xBF8C_C07F, // wait for LDS read
                0xE070_1000,
                0x8004_0401, // buffer_store_dword v4, v1, s[16:19]
                0xBF81_0000,
            ];
            let native = NativeProgram {
                stage: HwStage::Cs,
                wave64: true,
                vgprs: 8,
                layout: vec![param(RootType::Table, 0), param(RootType::Table, 1)],
                used: vec![
                    entry(ResourceType::Srv, 4, 0),
                    entry(ResourceType::Uav, 4, 1),
                ],
                io: StageIo::Cs(ComputeIo {
                    rsrc2: (1 << 15) | (3 << 1),
                    threads: [threads, 1, 1],
                }),
                code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
            };
            let lowered = lower_native(&native);
            assert_eq!(lowered.wave_size, None);
            assert_eq!(lowered.lds_barriers, 3);
            let shader = host_shader(lowered);
            let (signature, pipeline) = host
                .compute_pipeline(&shader)
                .expect("create exchange pipeline");
            let mut user_data = vec![0; shader.user_data_dwords().max(1) as usize];
            for u in &shader.user_data {
                user_data[u.index as usize] = 32 * u32::from(u.slot);
            }
            for round in 0..4 {
                let mut roots = vec![(0, Root::Constants(user_data.clone()))];
                if !shader.root_srvs.is_empty() {
                    assert_eq!(shader.root_srvs, [VIEW_STRIDES]);
                    roots.push((
                        1,
                        Root::View {
                            srv: true,
                            data: vec![4, 4],
                        },
                    ));
                }
                let input: Vec<_> = (0..threads)
                    .map(|lane| 0xa100_0000 | (round << 16) | (lane * 257 + 1))
                    .collect();
                let views = [
                    View::Buffer {
                        data: &input,
                        stride: Stride::Raw,
                    },
                    View::RawUav(threads),
                ];
                let result = host
                    .dispatch(
                        &signature,
                        &pipeline,
                        &Dispatch {
                            roots,
                            views: &views,
                            samplers: &[],
                            groups: [1, 1, 1],
                        },
                    )
                    .expect("dispatch exchange");
                let want: Vec<_> = (0..threads).map(|i| input[(i ^ half) as usize]).collect();
                assert_eq!(result[0], want, "{label} {threads} lanes round {round}");
                eprintln!(
                    "{label} threads={threads} round={round} lanes={} first={:08x} middle={:08x} last={:08x}",
                    want.len(),
                    result[0][0],
                    result[0][half as usize],
                    result[0][threads as usize - 1]
                );
            }
        }
    }
}

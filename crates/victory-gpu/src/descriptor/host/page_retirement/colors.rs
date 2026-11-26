use crate::host::{Host, HostList, transition};
use victory_shader::emit::VIEW_PAGE_SLOTS;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};

use super::super::{HostHeap, NULL_PAGE, PAGE};

pub(super) struct Colors {
    red: ID3D12Resource,
    blue: ID3D12Resource,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
}

pub(super) struct Readback {
    buffer: ID3D12Resource,
    mapped: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Sample {
    page: u32,
    physical: u32,
    rgba: [u8; 4],
}

// Three DWORDs per slot, written by HLSL's Store3.
const _: () = assert!(size_of::<Sample>() == 12);

impl Readback {
    /// Call after the fence for this dispatch has completed.
    pub(super) fn checked_colors(
        &self,
        slots: [u32; 3],
        cpu_slots: [Option<u32>; 3],
    ) -> [[u8; 4]; 3] {
        // SAFETY: buffer owns the mapped output and its GPU writes have completed.
        let samples = unsafe { self.mapped.cast::<[Sample; 3]>().read() };
        eprintln!("GPU samples for virtual slots {slots:?}: {samples:?}");
        for ((virtual_slot, mapped), sample) in slots.into_iter().zip(cpu_slots).zip(samples) {
            let physical = mapped.unwrap_or(NULL_PAGE * PAGE + virtual_slot % PAGE);
            assert_eq!(
                (sample.page, sample.physical),
                (physical / PAGE, physical),
                "GPU page and slot for virtual slot {virtual_slot}, CPU placement {mapped:?}"
            );
        }
        samples.map(|sample| sample.rgba)
    }
}

impl Colors {
    pub(super) fn new(host: &Host) -> Self {
        let list = host.new_list().unwrap();
        let texture = |rgba: [f32; 4]| {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: 1,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                ..Default::default()
            };
            let mut resource: Option<ID3D12Resource> = None;
            // SAFETY: the texture starts as a render target. Both textures outlive
            // the clear and the transition to the state used by the compute shader.
            unsafe {
                host.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        None,
                        &mut resource,
                    )
                    .unwrap();
                let resource = resource.unwrap();
                list.list
                    .ClearRenderTargetView(host.rtv(&resource, None), &rgba, None);
                list.list.ResourceBarrier(&[transition(
                    &resource,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                )]);
                resource
            }
        };
        let red = texture([1.0, 0.0, 0.0, 1.0]);
        let blue = texture([0.0, 0.0, 1.0, 1.0]);
        list.close();
        host.execute(&[&list]);
        Self::from_textures(red, blue)
    }

    pub(super) fn from_textures(red: ID3D12Resource, blue: ID3D12Resource) -> Self {
        let (root, pipeline) = pipeline();
        Self {
            red,
            blue,
            root,
            pipeline,
        }
    }

    pub(super) fn bind_red(&self, host: &Host, at: D3D12_CPU_DESCRIPTOR_HANDLE) {
        // SAFETY: a live typed texture and a writable CBV/SRV/UAV heap slot.
        unsafe { host.device.CreateShaderResourceView(&self.red, None, at) };
    }

    pub(super) fn bind_blue(&self, host: &Host, at: D3D12_CPU_DESCRIPTOR_HANDLE) {
        // SAFETY: a live typed texture and a writable CBV/SRV/UAV heap slot.
        unsafe { host.device.CreateShaderResourceView(&self.blue, None, at) };
    }

    /// The same blue texture as the control, with green and blue swapped in this SRV.
    pub(super) fn bind_blue_as_green(&self, host: &Host, at: D3D12_CPU_DESCRIPTOR_HANDLE) {
        // D3D12 encodes each source component in a three-bit field: (R, B, G, A).
        let mapping = D3D12_SHADER_COMPONENT_MAPPING_ALWAYS_SET_BIT_AVOIDING_ZEROMEM_MISTAKES
            | (2 << 3)
            | (1 << 6)
            | (3 << 9);
        let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: mapping,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MipLevels: 1,
                    ..Default::default()
                },
            },
        };
        // SAFETY: the view matches the live RGBA8 texture and the slot is writable.
        unsafe {
            host.device
                .CreateShaderResourceView(&self.blue, Some(&desc), at)
        };
    }

    pub(super) fn record(
        &self,
        host: &Host,
        heap: &HostHeap,
        slots: [u32; 3],
    ) -> (HostList, Readback) {
        self.record_with(host, heap, slots, |_| {})
    }

    pub(super) fn record_with(
        &self,
        host: &Host,
        heap: &HostHeap,
        slots: [u32; 3],
        record: impl FnOnce(&HostList),
    ) -> (HostList, Readback) {
        let bytes = size_of::<[Sample; 3]>();
        let (buffer, mapped) = host.shared_buffer(bytes as u64).unwrap();
        let readback = Readback { buffer, mapped };
        let list = host.new_list().unwrap();
        record(&list);
        let table = heap.strides().expect("a paged heap has a view-page table");
        // SAFETY: list is open. The test keeps the heap, textures, pipeline and
        // mapped result alive through the fence. Every group writes one Sample.
        unsafe {
            std::ptr::write_bytes(mapped, 0xcd, bytes);
            list.list.SetDescriptorHeaps(&[Some(heap.heap.clone())]);
            list.list.SetComputeRootSignature(&self.root);
            list.list
                .SetComputeRootDescriptorTable(0, heap.heap.GetGPUDescriptorHandleForHeapStart());
            list.list
                .SetComputeRootShaderResourceView(1, table.GetGPUVirtualAddress());
            list.list
                .SetComputeRootUnorderedAccessView(2, readback.buffer.GetGPUVirtualAddress());
            list.list
                .SetComputeRoot32BitConstants(3, slots.len() as u32, slots.as_ptr().cast(), 0);
            list.list.SetPipelineState(&self.pipeline);
            list.list.Dispatch(3, 1, 1);
        }
        list.close();
        (list, readback)
    }
}

fn pipeline() -> (ID3D12RootSignature, ID3D12PipelineState) {
    const SOURCE: &str = r#"
#define RS "DescriptorTable(SRV(t0, numDescriptors=unbounded, flags=DESCRIPTORS_VOLATILE)), SRV(t0, space=1, flags=DATA_VOLATILE), UAV(u0), RootConstants(num32BitConstants=3, b0)"
Texture2D<float4> textures[] : register(t0);
ByteAddressBuffer pageTable : register(t0, space1);
RWByteAddressBuffer samples : register(u0);
cbuffer Indices : register(b0) { uint3 slots; }

[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main(uint3 id : SV_GroupID) {
    uint slot = slots[id.x];
    uint page = pageTable.Load(PAGE_TABLE + 4 * (slot / PAGE_SLOTS));
    uint physical = page * PAGE_SLOTS + slot % PAGE_SLOTS;
    uint4 rgba = (uint4)round(255.0 * textures[NonUniformResourceIndex(physical)].Load(int3(0, 0, 0)));
    uint rgbaWord = rgba.r | (rgba.g << 8) | (rgba.b << 16) | (rgba.a << 24);
    samples.Store3(12 * id.x, uint3(page, physical, rgbaWord));
}
"#;
    crate::memory::test_page_table_pipeline(&format!(
        "#define PAGE_SLOTS {VIEW_PAGE_SLOTS}\n{SOURCE}"
    ))
}

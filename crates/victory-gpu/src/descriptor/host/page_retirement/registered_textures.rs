use super::HostHeap;
use crate::descriptor::{self, HEAP_IIDS, HeapDesc, Placement, SHADER_VISIBLE, VIEWS_KIND};
use crate::host::Host;
use crate::object::{S_OK, This, slot};
use crate::resource;
use std::ptr::{from_ref, null};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};

/// Owns one reference returned by a title-side creation call.
struct TitleObject(This);

impl Drop for TitleObject {
    fn drop(&mut self) {
        slot::<extern "system" fn(This) -> u32>(self.0, 0x10)(self.0);
    }
}

pub(super) struct RegisteredTextures {
    heap: TitleObject,
    red: TitleObject,
    blue: TitleObject,
}

impl RegisteredTextures {
    pub(super) fn new(host: &Host, count: u32) -> Self {
        // The title API reads the optional placement immediately after HeapDesc.
        #[repr(C)]
        struct Extended(HeapDesc, Placement);
        let desc = Extended(
            HeapDesc {
                kind: VIEWS_KIND,
                count,
                flags: SHADER_VISIBLE,
                nodes: 1,
            },
            Placement {
                address: 0,
                size: 0,
            },
        );
        let mut heap = null();
        assert_eq!(
            descriptor::create_heap(null(), &desc.0, &HEAP_IIDS[0], &mut heap),
            S_OK
        );
        let heap = TitleObject(heap);
        let list = host.new_list().unwrap();
        let texture = |rgba: [f32; 4]| {
            let properties = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
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
            let mut resource = null();
            assert_eq!(
                resource::create_committed(
                    null(),
                    from_ref(&properties).cast(),
                    0,
                    from_ref(&desc).cast(),
                    D3D12_RESOURCE_STATE_COMMON.0 as u32,
                    null(),
                    &resource::PAGEABLE_IIDS[0],
                    &mut resource,
                ),
                S_OK,
            );
            let resource = TitleObject(resource);
            let copy = resource::host_copy(resource.0).expect("committed RGBA8 texture");
            // The submission prelude handles the first transition to render-target state.
            assert!(
                list.move_texture(&copy, D3D12_RESOURCE_STATE_RENDER_TARGET.0 as u32)
                    .is_none()
            );
            // SAFETY: the prelude prepares this live target, which stays owned through submission.
            unsafe {
                list.list
                    .ClearRenderTargetView(host.rtv(&copy.texture, None), &rgba, None)
            };
            let barrier = list
                .move_texture(
                    &copy,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0 as u32,
                )
                .unwrap();
            // SAFETY: the cleared subresource is in render-target state before this transition.
            unsafe { list.list.ResourceBarrier(&[barrier]) };
            resource
        };
        let red = texture([1.0, 0.0, 0.0, 1.0]);
        let blue = texture([0.0, 0.0, 1.0, 1.0]);
        list.close();
        host.execute(&[&list]);
        Self { heap, red, blue }
    }

    pub(super) fn host_heap(&self) -> &HostHeap {
        descriptor::heap(self.heap.0)
            .host()
            .expect("shader-visible title heap")
    }

    pub(super) fn colors(&self) -> super::Colors {
        let texture = |r: &TitleObject| resource::host_copy(r.0).unwrap().texture.clone();
        super::Colors::from_textures(texture(&self.red), texture(&self.blue))
    }

    pub(super) fn write_red(&self, slot: u32) {
        self.write(slot, self.red.0, D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING);
    }

    pub(super) fn write_green(&self, slot: u32) {
        // Swapping G and B produces green from the committed blue texture.
        let mapping = D3D12_SHADER_COMPONENT_MAPPING_ALWAYS_SET_BIT_AVOIDING_ZEROMEM_MISTAKES
            | (2 << 3)
            | (1 << 6)
            | (3 << 9);
        self.write(slot, self.blue.0, mapping);
    }

    fn write(&self, index: u32, resource: This, mapping: u32) {
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
        let step = descriptor::increment(VIEWS_KIND).unwrap();
        let handle = descriptor::heap(self.heap.0).start + u64::from(index) * u64::from(step);
        descriptor::create_srv(null(), resource, from_ref(&desc).cast(), handle);
    }
}

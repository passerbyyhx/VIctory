use super::{Shader, Translated, library};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};
use victory_shader::{HwStage, emit::HostShader};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{PCSTR, s};

fn shader(stage: &'static str, hw_stage: HwStage, target: PCSTR, source: &str) -> Arc<Shader> {
    let mut blob = None;
    // SAFETY: the source and C strings live for the compilation.
    let code = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            s!("main"),
            target,
            0,
            0,
            &mut blob,
            None,
        )
        .expect("compile synthetic shader");
        let blob = blob.unwrap();
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
            .to_vec()
    };
    translated_shader(stage, hw_stage, code)
}

fn translated_shader(stage: &'static str, hw_stage: HwStage, code: Vec<u8>) -> Arc<Shader> {
    let shader = Shader::new(stage, &code);
    let host = HostShader {
        stage: hw_stage,
        dxil: code,
        user_data: Vec::new(),
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        root_uavs: Vec::new(),
        memory: false,
        memory_writes: false,
        view_addresses: false,
        heap: false,
        heap_writes: false,
        view_marks: false,
    };
    assert!(
        shader
            .translated
            .set(Some(Translated {
                host,
                used: Vec::new()
            }))
            .is_ok()
    );
    Arc::new(shader)
}

fn identify_adapter() {
    let device = &crate::host::host().expect("D3D12 device").device;
    // SAFETY: plain DXGI query for the live device.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(device.GetAdapterLuid()).unwrap();
        let desc = adapter.GetDesc1().unwrap();
        let end = desc
            .Description
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(desc.Description.len());
        eprintln!(
            "pid={} name={:?} vendor={:04x} luid={:08x}:{:08x} flags={:x}",
            std::process::id(),
            String::from_utf16_lossy(&desc.Description[..end]),
            desc.VendorId,
            desc.AdapterLuid.HighPart,
            desc.AdapterLuid.LowPart,
            desc.Flags
        );
        let warp = std::env::var_os("VICTORY_D3D12_WARP").is_some();
        assert_eq!(desc.Flags & 2 != 0, warp, "software adapter flag");
        if !warp { assert_eq!(desc.VendorId, 0x10de); }
    }
}

use super::make;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicUsize;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

static OPEN_WARNINGS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct EventFields(Vec<String>);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }
}

struct WarningRecorder;

impl Subscriber for WarningRecorder {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::WARN
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        if fields.0.iter().any(|f| f.contains("VICTORY_PIPELINE_LOG could not be opened")) {
            OPEN_WARNINGS.fetch_add(1, Relaxed);
        }
        eprintln!("{} {} {}", event.metadata().level(), event.metadata().target(), fields.0.join(" "));
    }
}

fn build(shader: &Arc<Shader>) -> String {
    let pso = Arc::new(make::Pending::new(vec![shader.clone()], None, &[]));
    let started = Instant::now();
    loop {
        if let Some(host) = make::ready(&pso) {
            assert!(host.compute);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "real compute PSO unavailable");
        std::thread::sleep(Duration::from_millis(1));
    }
    while make::waiting() != 0 {
        assert!(started.elapsed() < Duration::from_secs(20), "worker logging did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    pso.key.clone()
}

fn inspect_tsv(text: &str, expected_keys: &[&str]) {
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let kind = fields[0];
        let key = fields[1];
        assert!(seen.insert((kind, key)), "duplicate event {kind} {key}");
        match kind {
            "created" => {
                assert_eq!(fields.len(), 4);
                assert!(!fields[2].is_empty());
            }
            "used" => assert_eq!(&fields[2..], &[""]),
            "built" => {
                assert_eq!(fields.len(), 4);
                assert_eq!(fields[2].len(), 32);
                assert!(matches!(fields[3], "loaded" | "made"));
            }
            _ => panic!("unexpected event {kind}"),
        }
    }
    let expected: BTreeSet<_> = expected_keys.iter().flat_map(|key| {
        ["created", "used", "built"].map(|kind| (kind, *key))
    }).collect();
    assert_eq!(seen, expected);
    eprintln!("tsv_events={seen:?}");
}

#[test]
#[ignore = "uses isolated processes and pipeline-log filesystem state"]
fn pipeline_log_open_and_real_events() {
    tracing::subscriber::set_global_default(WarningRecorder).unwrap();
    identify_adapter();
    let mode = std::env::var("EIGHT_LOG_MODE").unwrap();
    let version = std::env::var("EIGHT_LOG_VERSION").unwrap();
    let destination = PathBuf::from(std::env::var_os("VICTORY_PIPELINE_LOG").unwrap());
    let cache = tempfile::tempdir().unwrap();
    library::use_pipeline_cache(cache.path().to_path_buf());
    let a = shader("compute", HwStage::Cs, s!("cs_5_1"), "[numthreads(1,1,1)] void main() {}");
    let b = shader("compute", HwStage::Cs, s!("cs_5_1"), "[numthreads(2,1,1)] void main() {}");
    let warning_count = || OPEN_WARNINGS.load(Relaxed);
    match mode.as_str() {
        "late" => {
            let parent = destination.parent().unwrap();
            assert!(!parent.exists());
            let a = build(&a);
            assert!(!destination.exists());
            let first_warnings = warning_count();
            assert_eq!(first_warnings, usize::from(version == "head"));
            eprintln!("before_directory key={a} warnings={first_warnings}");
            std::fs::create_dir(parent).unwrap();
            let b_key = build(&b);
            assert_ne!(a, b_key);
            // A fresh Pending repeats all three event paths for the exact same key.
            assert_eq!(build(&b), b_key);
            if version == "head" {
                assert!(!destination.exists());
            } else {
                let text = std::fs::read_to_string(&destination).unwrap();
                inspect_tsv(&text, &[&b_key]);
            }
            assert_eq!(warning_count(), first_warnings);
        }
        "retry" => {
            assert!(destination.parent().unwrap().is_dir());
            let a_key = build(&a);
            let b_key = build(&b);
            assert_ne!(a_key, b_key);
            assert_eq!(build(&a), a_key);
            assert_eq!(build(&b), b_key);
            let text = std::fs::read_to_string(&destination).unwrap();
            inspect_tsv(&text, &[&a_key, &b_key]);
            assert_eq!(warning_count(), 0);
        }
        "empty" => {
            assert!(destination.as_os_str().is_empty());
            let a = build(&a);
            let b_key = build(&b);
            assert_ne!(a, b_key);
            assert_eq!(build(&b), b_key);
            assert_eq!(warning_count(), 0);
        }
        _ => panic!("unexpected mode {mode}"),
    }
    let made = library::MADE.load(Relaxed);
    let loaded = library::LOADED.load(Relaxed);
    assert_eq!(made, 2, "two distinct real D3D12 pipeline creations");
    assert_eq!(loaded, if mode == "retry" { 2 } else { 1 });
    eprintln!("version={version} mode={mode} warnings={} log_exists={} host_made={made} host_loaded={loaded}", warning_count(), destination.is_file());
}

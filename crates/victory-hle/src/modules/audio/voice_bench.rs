use super::*;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SLOTS: [u32; 4] = [0, 63, 64, 511];
const PACKETS: usize = 2;
const CHANNELS: usize = 2;
const PCM_SAMPLES: usize = (PACKETS * xma::FRAME_SAMPLES + xma::FRAME_SAMPLES / 2) * CHANNELS;
const PCM_BYTES: usize = PCM_SAMPLES * 2;
const RING_BYTES: usize = PCM_BYTES + BLOCK_BYTES;
const RING_BLOCKS: u32 = (RING_BYTES / BLOCK_BYTES) as u32;
const SENTINEL: u8 = 0xa5;
const WARMUP_ROUNDS: usize = 128;
static MANUAL_PUMP: AtomicBool = AtomicBool::new(false);

pub(super) fn manual_pump() -> bool {
    MANUAL_PUMP.load(Ordering::Relaxed)
}

#[cfg(feature = "xma-voice-bench-alloc")]
mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct Counter;

    fn tick() {
        let _ = COUNT.try_with(|count| count.set(count.get().map(|n| n + 1)));
    }

    #[global_allocator]
    static ALLOCATOR: Counter = Counter;

    // SAFETY: each operation forwards the pointer and layout unchanged to System.
    unsafe impl GlobalAlloc for Counter {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            tick();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            tick();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            tick();
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    pub(super) fn begin() {
        COUNT.with(|count| count.set(Some(0)));
    }

    pub(super) fn finish() -> usize {
        COUNT.with(|count| count.replace(None).unwrap())
    }
}

#[cfg(not(feature = "xma-voice-bench-alloc"))]
mod allocation {
    pub(super) fn begin() {}

    pub(super) fn finish() -> usize {
        0
    }
}

fn write_bits(data: &mut [u8], at: &mut usize, value: u32, width: u32) {
    for shift in (0..width).rev() {
        let mask = 0x80 >> (*at % 8);
        let byte = &mut data[*at / 8];
        *byte = (*byte & !mask) | if value & (1 << shift) != 0 { mask } else { 0 };
        *at += 1;
    }
}

fn random(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

fn packets(index: u32) -> Vec<u8> {
    // Canonical six-bit VEC4 codes and their following sign counts, adapted
    // from the #364 synthetic stream fixture. The channels use distinct seeds.
    let vectors = [
        (0b100000, 4),
        (0b100110, 1),
        (0b100111, 1),
        (0b101000, 3),
        (0b101001, 3),
        (0b101010, 3),
        (0b101011, 3),
        (0b101101, 2),
    ];
    let mut seeds = [0x3642_6001 ^ index, 0x7163_4002 ^ (index << 16)];
    let mut output = vec![0xff; PACKETS * xma::PACKET_BYTES];
    for packet in output.chunks_exact_mut(xma::PACKET_BYTES) {
        let mut at = 0;
        write_bits(packet, &mut at, 1 << 26, 32); // one frame, zero offset and skip
        let frame_start = at;
        write_bits(packet, &mut at, 0, 15); // frame length filled below
        write_bits(packet, &mut at, 0b10, 2); // fixed, one 512-sample subframe
        write_bits(packet, &mut at, 0, 1 + 8 + 1); // no postprocessing, zero gain, no trim
        write_bits(packet, &mut at, 0b00010, 5); // no fill, reserved zero, no channel transform
        write_bits(packet, &mut at, 0b11, 2); // both channels transmit coefficients
        write_bits(packet, &mut at, 1, 1); // explicit vector counts
        write_bits(packet, &mut at, 0x8080, 16); // 128 four-coefficient vectors per channel
        write_bits(packet, &mut at, 0, 6 + 3 + 2); // neutral shared quantizer, no modifiers
        for _ in 0..CHANNELS {
            write_bits(packet, &mut at, 0, 2); // scale-factor step 1
            write_bits(packet, &mut at, (1 << 23) - 1, 23); // 48 kHz scale deltas all zero
        }
        for seed in &mut seeds {
            write_bits(packet, &mut at, 0, 1); // coefficient table zero
            for _ in 0..128 {
                let (code, signs) = vectors[(random(seed) & 7) as usize];
                write_bits(packet, &mut at, code, 6);
                write_bits(packet, &mut at, random(seed), signs);
            }
        }
        write_bits(packet, &mut at, 0, 2); // trailing bits, including end of this packet
        let mut length_at = frame_start;
        write_bits(packet, &mut length_at, (at - frame_start) as u32, 15);
    }
    output
}

fn reference(data: &[u8]) -> Vec<u8> {
    let mut decoder = xma::Decoder::new(CHANNELS, 48_000);
    let mut pcm = Vec::with_capacity(PCM_SAMPLES);
    let mut at = Some(32);
    let mut frames = 0;
    while let Some(start) = at {
        let frame = xma::read_frame(data, start).expect("complete frame");
        decoder
            .decode(&frame.data, frame.bits, &mut pcm)
            .expect("valid stereo frame");
        at = frame.next;
        frames += 1;
    }
    assert_eq!(frames, PACKETS);
    decoder.flush(&mut pcm);
    assert_eq!(pcm.len(), PCM_SAMPLES);
    for channel in 0..CHANNELS {
        let nonzero = pcm
            .iter()
            .skip(channel)
            .step_by(CHANNELS)
            .filter(|&&s| s != 0)
            .count();
        assert!(nonzero >= PCM_SAMPLES / CHANNELS / 2);
    }
    pcm.into_iter().flat_map(i16::to_le_bytes).collect()
}

struct Buffer {
    host: usize,
    apu: u32,
    size: usize,
}

impl Buffer {
    fn new(size: usize, alignment: u32, tag: u32) -> Self {
        let mut ptr = std::ptr::null_mut();
        let mut apu = 0;
        assert_eq!(
            ApuAlloc(&mut ptr, &mut apu, size as u32, alignment, tag),
            S_OK
        );
        Self {
            host: ptr as usize,
            apu,
            size,
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        ApuFree(self.host as *mut c_void);
    }
}

struct Slot {
    index: u32,
    input: Buffer,
    output: Buffer,
    expected: Vec<u8>,
}

impl Slot {
    fn new(index: u32) -> Self {
        let data = packets(index);
        let expected = reference(&data);
        let input = Buffer::new(data.len(), xma::PACKET_BYTES as u32, 1);
        // SAFETY: ApuAlloc gave this slot its own buffer of exactly data.len() bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), input.host as *mut u8, data.len()) };
        let output = Buffer::new(RING_BYTES, BLOCK_BYTES as u32, 2);
        Self {
            index,
            input,
            output,
            expected,
        }
    }

    fn prepare(&self) {
        let x = xma();
        let context = x.context(self.index).unwrap();
        // Only this worker configures the context, before releasing the pump barrier.
        for word in 0..16 {
            context.set(word, 0);
        }
        context.set(0, PACKETS as u32 | (1 << 20) | (RING_BLOCKS << 22));
        context.set(1, (1 << 31) | (1 << 29) | (3 << 27));
        context.set(2, 32);
        context.set(5, self.input.apu);
        context.set(7, self.output.apu);
        // SAFETY: the slot owns the ring. The scheduler can only write after the ready barrier.
        unsafe { (self.output.host as *mut u8).write_bytes(SENTINEL, self.output.size) };
    }

    fn command(&self, command: u32) {
        assert!(xma_command(command, |word| (word == 0).then_some(self.index)));
    }

    fn snapshot(&self, out: &mut [u8]) {
        // SAFETY: the done barrier follows the pump and this worker owns its slot's output.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.output.host as *const u8,
                out.as_mut_ptr(),
                out.len(),
            )
        };
    }

    fn check_status(&self) {
        let x = xma();
        let context = x.context(self.index).unwrap();
        let guard = lock(&x.voices[self.index as usize]);
        let voice = guard.as_ref().unwrap();
        assert_eq!(context.channels(), CHANNELS);
        assert_eq!(voice.trace.frames, PACKETS as _);
        assert_eq!(voice.trace.bad_frames, 0);
        assert_eq!(voice.trace.produced, PCM_SAMPLES / CHANNELS);
        assert!(voice.ended);
    }
}

struct WorkerResult {
    rings: Vec<u8>,
    latencies: Vec<u64>,
    command_allocations: usize,
}

#[derive(Clone, Copy)]
struct Run {
    rounds: usize,
    full_scan: bool,
    check_status: bool,
}

impl Run {
    fn worker(
        self,
        slot: &Slot,
        ready: &Barrier,
        done: &Barrier,
        finish: &Barrier,
    ) -> WorkerResult {
        let mut rings = vec![0; self.rounds * RING_BYTES];
        let mut latencies = Vec::with_capacity(self.rounds);
        let mut command_allocations = 0;
        finish.wait();
        for ring in rings.chunks_exact_mut(RING_BYTES) {
            slot.prepare();
            let start = Instant::now();
            allocation::begin();
            slot.command(ENABLE_XMA);
            command_allocations += allocation::finish();
            ready.wait();
            done.wait();
            if self.check_status {
                slot.check_status();
            }
            slot.snapshot(ring);
            allocation::begin();
            slot.command(DISABLE_XMA);
            command_allocations += allocation::finish();
            latencies.push(nanos(start.elapsed()));
        }
        finish.wait();
        WorkerResult {
            rings,
            latencies,
            command_allocations,
        }
    }

    fn execute(self, slots: &[Slot]) -> (Duration, Duration, usize, Vec<WorkerResult>) {
        let ready = Barrier::new(slots.len() + 1);
        let done = Barrier::new(slots.len() + 1);
        let finish = Barrier::new(slots.len() + 1);
        std::thread::scope(|scope| {
            let (ready, done, finish) = (&ready, &done, &finish);
            let threads: Vec<_> = slots
                .iter()
                .map(|slot| scope.spawn(move || self.worker(slot, ready, done, finish)))
                .collect();
            let mut pump_elapsed = Duration::ZERO;
            let mut pump_allocations = 0;
            // Workers allocate their capture buffers and enter the launch barrier.
            finish.wait();
            let start = Instant::now();
            for _ in 0..self.rounds {
                ready.wait();
                let pump_start = Instant::now();
                allocation::begin();
                let x = xma();
                if self.full_scan {
                    x.pump();
                } else {
                    for slot in slots {
                        let mut voice = lock(&x.voices[slot.index as usize]);
                        pump(x.context(slot.index).unwrap(), voice.as_mut().unwrap());
                    }
                }
                drop(x);
                pump_allocations += allocation::finish();
                pump_elapsed += pump_start.elapsed();
                done.wait();
            }
            finish.wait();
            let elapsed = start.elapsed();
            let results = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            (elapsed, pump_elapsed, pump_allocations, results)
        })
    }
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap()
}

fn verify(slots: &[Slot], results: &[WorkerResult]) -> (Vec<u8>, u64) {
    let mut ownership_bytes = Vec::new();
    let mut checksum = 0xcbf2_9ce4_8422_2325u64;
    let mut channels: Vec<Vec<u8>> = Vec::new();
    for (slot, result) in slots.iter().zip(results) {
        let mut verified = result.rings.chunks_exact(RING_BYTES);
        for ring in verified.by_ref() {
            assert_eq!(
                &ring[..PCM_BYTES],
                slot.expected,
                "PCM for slot {}",
                slot.index
            );
            assert!(ring[PCM_BYTES..].iter().all(|&b| b == SENTINEL));
        }
        assert!(verified.remainder().is_empty());
        let ring = &result.rings[..PCM_BYTES];
        for channel in 0..CHANNELS {
            let samples: Vec<u8> = ring
                .chunks_exact(2 * CHANNELS)
                .flat_map(|pair| pair[2 * channel..2 * channel + 2].iter().copied())
                .collect();
            assert!(
                channels.iter().all(|other| *other != samples),
                "indistinguishable channel in slot {}",
                slot.index
            );
            ownership_bytes.extend(slot.index.to_le_bytes());
            ownership_bytes.push(channel as u8);
            ownership_bytes.extend(samples.iter().copied());
            channels.push(samples);
        }
    }
    for &byte in &ownership_bytes {
        checksum = (checksum ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (ownership_bytes, checksum)
}

#[test]
#[ignore = "CPU-only release benchmark"]
fn xma_short_voice_cpu_bench() {
    let workers: usize = std::env::var("VI3_AUDIO_WORKERS").unwrap().parse().unwrap();
    let rounds: usize = std::env::var("VI3_AUDIO_ROUNDS").unwrap().parse().unwrap();
    let mode = std::env::var("VI3_AUDIO_MODE").unwrap();
    assert!(matches!(workers, 1 | 2 | 4));
    assert!(rounds > 0);
    assert!(matches!(mode.as_str(), "full" | "slot"));
    let full_scan = mode == "full";
    MANUAL_PUMP.store(true, Ordering::Relaxed);
    let mut request = ShapeContexts {
        counts: [0, 0, 0, 0, 512, 0],
        arrays: [std::ptr::null_mut(); 6],
        apu: [0; 6],
    };
    assert_eq!(AcpHalAllocateShapeContexts(&mut request), S_OK);
    let slots: Vec<_> = SLOTS[..workers].iter().copied().map(Slot::new).collect();
    let preflight = Run {
        rounds: 1,
        full_scan,
        check_status: true,
    };
    let (_, _, _, results) = preflight.execute(&slots);
    let (ownership, expected_checksum) = verify(&slots, &results);
    if let Some(path) = std::env::var_os("VI3_AUDIO_PCM") {
        std::fs::write(path, ownership).unwrap();
    }
    let warmup = Run {
        rounds: WARMUP_ROUNDS,
        full_scan,
        check_status: false,
    };
    let (_, _, _, results) = warmup.execute(&slots);
    assert_eq!(verify(&slots, &results).1, expected_checksum);

    let run = Run {
        rounds,
        full_scan,
        check_status: false,
    };
    let (elapsed, pump_elapsed, pump_allocations, results) = run.execute(&slots);
    let (_, checksum) = verify(&slots, &results);
    assert_eq!(checksum, expected_checksum);
    let command_allocations: usize = results
        .iter()
        .map(|result| result.command_allocations)
        .sum();
    let mut latencies: Vec<_> = results
        .into_iter()
        .flat_map(|result| result.latencies)
        .collect();
    latencies.sort_unstable();
    let voices = workers * rounds;
    let p50 = latencies[(voices - 1) / 2];
    let p95 = latencies[(95 * voices).div_ceil(100) - 1];
    let kind = if cfg!(feature = "xma-voice-bench-alloc") {
        "alloc"
    } else {
        "time"
    };
    println!(
        "VI3_AUDIO,{kind},{mode},{workers},{rounds},{voices},{},{},{},{p50},{p95},{command_allocations},{pump_allocations},{checksum:016x}",
        nanos(elapsed),
        nanos(pump_elapsed),
        voices as f64 / elapsed.as_secs_f64(),
    );
    drop(slots);
    assert_eq!(AcpHalReleaseShapeContexts(), S_OK);
    MANUAL_PUMP.store(false, Ordering::Relaxed);
}

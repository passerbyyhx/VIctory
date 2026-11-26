use std::{env, fs, hint::black_box, time::Instant};
use victory_formats::xma::{self, Decoder, FRAME_SAMPLES, PACKET_BYTES};

const PACKETS: usize = 16;
const STREAMS: usize = 2048;

/// Write the public XMA2 bit order, most significant bit first.
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

fn packets() -> Vec<u8> {
    // Six-bit codes from the canonical VEC4 table, with the number of
    // subsequent sign bits for the nonzero coefficients in each vector.
    let vectors = [
        (0b100000, 4), // 1111
        (0b100110, 1), // 1000
        (0b100111, 1), // 0001
        (0b101000, 3), // 1110
        (0b101001, 3), // 0111
        (0b101010, 3), // 1101
        (0b101011, 3), // 1011
        (0b101101, 2), // 1100
    ];
    let mut output = vec![0xff; PACKETS * PACKET_BYTES];
    let mut seed = 0x3642_6001;
    for packet in output.chunks_exact_mut(PACKET_BYTES) {
        let mut at = 0;
        write_bits(packet, &mut at, 1 << 26, 32); // one frame, offset 0, no skip
        let frame_start = at;
        write_bits(packet, &mut at, 0, 15); // length filled below
        write_bits(packet, &mut at, 0b10, 2); // fixed, one 512-sample subframe
        write_bits(packet, &mut at, 0, 8); // dynamic range compression gain
        write_bits(packet, &mut at, 0, 1); // no trim
        write_bits(packet, &mut at, 0, 2); // no fill, reserved bit zero
        write_bits(packet, &mut at, 1, 1); // transmit monochannel coefficients
        write_bits(packet, &mut at, 1, 1); // explicit vector count
        write_bits(packet, &mut at, 128, 8); // 128 four-coefficient vectors
        write_bits(packet, &mut at, 0, 6); // neutral quantization adjustment
        write_bits(packet, &mut at, 0, 2); // scale-factor step 1
        write_bits(packet, &mut at, (1 << 23) - 1, 23); // 23 bands at 48 kHz, all scale deltas 0
        write_bits(packet, &mut at, 0, 1); // coefficient table 0
        for _ in 0..128 {
            let (code, signs) = vectors[(random(&mut seed) & 7) as usize];
            write_bits(packet, &mut at, code, 6);
            write_bits(packet, &mut at, random(&mut seed), signs);
        }
        write_bits(packet, &mut at, 0, 2); // trailing bits, including the end of this packet
        let mut length_at = frame_start;
        write_bits(packet, &mut length_at, (at - frame_start) as u32, 15);
    }
    output
}

fn decode(data: &[u8], pcm: &mut Vec<i16>) {
    pcm.clear();
    let mut decoder = Decoder::new(1, 48_000);
    let header = xma::packet(data, 0).unwrap();
    let mut position = Some(32 + header.first_frame_bit as usize);
    let mut count = 0;
    while let Some(at) = position {
        let frame = xma::read_frame(data, at).expect("complete frame");
        decoder.decode(&frame.data, frame.bits, pcm).expect("valid XMA2 frame");
        count += 1;
        position = frame.next;
    }
    decoder.flush(pcm);
    assert_eq!(count, PACKETS);
    assert_eq!(pcm.len(), PACKETS * FRAME_SAMPLES + FRAME_SAMPLES / 2);
}

fn main() {
    let mut args = env::args().skip(1);
    let pcm_path = args.next().expect("PCM output path");
    let mode = args.next();
    let data = packets();
    let mut pcm = Vec::with_capacity(PACKETS * FRAME_SAMPLES + FRAME_SAMPLES / 2);
    decode(&data, &mut pcm);
    let nonzero = pcm.iter().filter(|&&x| x != 0).count();
    assert!(nonzero > pcm.len() / 2);
    let expected = pcm.clone();
    let bytes: Vec<_> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    fs::write(pcm_path, &bytes).unwrap();
    println!("packets={PACKETS} samples={} nonzero={nonzero} min={} max={}",
             pcm.len(), pcm.iter().min().unwrap(), pcm.iter().max().unwrap());
    if mode.as_deref() == Some("--verify-only") {
        return;
    }
    assert!(mode.is_none());
    for _ in 0..16 {
        decode(black_box(&data), &mut pcm);
        black_box(&pcm);
    }
    let start = Instant::now();
    for _ in 0..STREAMS {
        decode(black_box(&data), &mut pcm);
        black_box(&pcm);
    }
    let elapsed = start.elapsed();
    assert_eq!(pcm, expected);
    println!("streams={STREAMS} frames={} elapsed_ms={:.3}",
             STREAMS * PACKETS, elapsed.as_secs_f64() * 1000.0);
}

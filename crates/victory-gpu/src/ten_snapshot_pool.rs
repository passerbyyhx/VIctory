use super::*;
use std::hint::black_box;
use std::time::Instant;

fn capacities() -> Vec<usize> {
    POOL.lock().unwrap().iter().map(Vec::capacity).collect()
}

fn reset() {
    POOL.lock().unwrap().clear();
}

#[test]
fn production_pool_semantics() {
    reset();
    let mut small = Vec::with_capacity(3);
    small.extend_from_slice(&[0xaf, 0x11, 0x6e]);
    let small_pointer = small.as_ptr();
    give_back(small);
    give_back(Vec::with_capacity(17));
    give_back(Vec::with_capacity(7));
    let first = pooled(3);
    assert_eq!(first.as_ptr(), small_pointer);
    assert!(first.is_empty());
    assert_eq!(first.capacity(), 3);
    let next = pooled(4);
    assert_eq!(next.capacity(), 7);
    assert_eq!(capacities(), [17]);
    give_back(first);
    give_back(next);
    assert_eq!(capacities(), [17, 7, 3]);

    let miss = pooled(18);
    assert!(miss.is_empty());
    assert_eq!(miss.capacity(), 18);
    assert_eq!(capacities(), [17, 7, 3]);
    eprintln!("TEN_POOL,best_fit,3,7,miss=18,kept={:?}", capacities());

    reset();
    let large = POOL_BYTES / 2;
    let medium = POOL_BYTES / 4;
    give_back(Vec::with_capacity(large));
    give_back(Vec::with_capacity(medium));
    give_back(Vec::with_capacity(medium));
    assert_eq!(capacities(), [large, medium, medium]);
    give_back(Vec::with_capacity(large / 4));
    assert_eq!(capacities(), [large, medium, medium]);
    give_back(Vec::with_capacity(large + 1));
    assert_eq!(capacities(), [large + 1]);
    eprintln!(
        "TEN_POOL,cap,exact={POOL_BYTES},retained={:?}",
        capacities()
    );
    give_back(Vec::with_capacity(POOL_BYTES + 1));
    assert!(capacities().is_empty());
    eprintln!("TEN_POOL,oversize,retained={:?}", capacities());

    give_back(Vec::with_capacity(512));
    give_back(Vec::with_capacity(512));
    let a = pooled(511);
    let b = pooled(512);
    assert_eq!(a.capacity(), 512);
    assert_eq!(b.capacity(), 512);
    assert_ne!(a.as_ptr(), b.as_ptr());
    assert!(capacities().is_empty());
    eprintln!("TEN_POOL,equal_capacity,512,512,distinct_live_buffers=true");
    reset();
}

#[test]
#[ignore = "run separately in release with one test thread"]
fn production_pool_timing() {
    assert!(!cfg!(debug_assertions), "timing requires --release");
    for n in [8usize, 128, 2000] {
        reset();
        // Touch each synthetic buffer before timing, like a returned snapshot.
        for i in 0..n {
            let bytes = (i % 64 + 1) * 1024;
            give_back(vec![0xa7; bytes]);
        }
        let initial = capacities();
        assert_eq!(initial.len(), n);
        let max = initial[0];
        for (workload, iters) in [("fixed", 40_000usize), ("mixed", 40_000)] {
            let requests: Vec<_> = (0..iters)
                .map(|i| {
                    if workload == "fixed" {
                        max / 2
                    } else {
                        (i.wrapping_mul(1_987) % max) + 1
                    }
                })
                .collect();
            let cycle = |requests: &[usize]| {
                for &request in requests {
                    let snapshot = pooled(black_box(request));
                    give_back(black_box(snapshot));
                }
            };
            cycle(&requests[..2000]);
            let start = Instant::now();
            cycle(&requests);
            let elapsed = start.elapsed().as_nanos();
            assert_eq!(capacities(), initial);
            eprintln!("TEN_POOL_TIME,{n},{workload},{iters},{elapsed}");
        }
    }
    reset();
}

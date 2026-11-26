use super::*;
use std::sync::mpsc::{Receiver, Sender, channel};

pub(super) static WORKER: OnceLock<Gate> = OnceLock::new();

pub(super) struct Gate {
    taken: Sender<Arc<Pending>>,
    resume: Mutex<Receiver<()>>,
}

pub(super) fn pause(p: &Arc<Pending>) {
    if let Some(gate) = WORKER.get() {
        if gate.taken.send(p.clone()).is_ok() {
            let _ = gate.resume.lock().unwrap().recv();
        }
    }
}

#[test]
#[ignore = "run alone with the worker gate"]
fn presents_promote_at_most_eight_available_feeders_nearest_first() {
    use crate::{feeds, queue};

    const NEAR: usize = 12;
    const NORMAL_A: usize = 12;
    const NORMAL_B: usize = 13;
    const NULL_WRITER: usize = 14;
    const UNRELATED: usize = 15;
    let candidates: [Arc<Pending>; 16] = std::array::from_fn(|_| {
        Arc::new(Pending::new(
            Vec::new(),
            None,
            #[cfg(feature = "diagnostics")]
            &[],
        ))
    });
    // Empty shader lists intentionally return a refusal after scheduling.
    // Pipeline identities in the queue and graph are distinct Arc addresses.
    let tag = |p: &Arc<Pending>| {
        candidates
            .iter()
            .position(|q| Arc::ptr_eq(p, q))
            .expect("fixture pipeline")
    };
    let (taken, claims) = channel();
    let (resume, paused) = channel();
    assert!(
        WORKER
            .set(Gate {
                taken,
                resume: Mutex::new(paused)
            })
            .is_ok()
    );
    let claim = |want| {
        let p = claims
            .recv_timeout(Duration::from_secs(10))
            .expect("worker claim");
        let got = tag(&p);
        eprintln!("worker took {got}");
        assert_eq!(got, want);
        assert!(p.taken.load(Ordering::Relaxed));
        assert!(!may_hurry(&p));
    };
    let pending = || {
        let queues = QUEUES.0.lock().unwrap();
        queues[0].iter().map(tag).collect::<Vec<_>>()
    };
    let pass = |reads: &[usize], write| {
        feeds::begin();
        for &read in reads {
            feeds::view(read, false);
        }
        feeds::ran(&[write], 0);
    };
    let dropped = |i, write| feeds::dropped(100 + i, &candidates[i], &[write]);

    // Hold the worker immediately after it takes the nearest pipeline.
    start(&candidates[0], true);
    claim(0);
    let normal: Vec<usize> = [NORMAL_A, NORMAL_B]
        .into_iter()
        .chain(1..NEAR)
        .chain([NULL_WRITER, UNRELATED])
        .collect();
    for &i in &normal {
        start(&candidates[i], true);
    }
    assert_eq!(pending(), normal);
    for i in 0..NEAR {
        dropped(i, 2_000 + i);
        pass(&[2_000 + i, 3_001 + i], 3_000 + i);
    }
    dropped(UNRELATED, 9_999);
    pass(&[9_999], 3_012);
    assert!(!queue::present_for_pipeline_probe(1, 3_000));
    let first: Vec<_> = (1..=8).chain(normal.iter().copied()).collect();
    let got = pending();
    eprintln!("frame=1 queue_after_present={got:?} expected={first:?}");
    assert_eq!(got, first);
    for i in 1..=8 {
        resume.send(()).unwrap();
        claim(i);
    }
    assert_eq!(pending(), normal);

    // The worker has 8. Also note 1 (finished), and let the three deferred
    // feeders arrive at different distances in the opposite order.
    dropped(8, 4_000);
    dropped(1, 4_001);
    dropped(11, 4_002);
    dropped(10, 4_003);
    dropped(9, 4_004);
    dropped(NULL_WRITER, 0);
    pass(&[4_000, 4_001, 0, 4_002, 3_012, 5_001], 5_000);
    pass(&[4_003, 5_002], 5_001);
    pass(&[4_004], 5_002);
    // Frame 1 connected 3_012 to UNRELATED. Frame 2 records no such writer.
    assert!(!queue::present_for_pipeline_probe(2, 5_000));
    let second: Vec<_> = [11, 10, 9]
        .into_iter()
        .chain(normal.iter().copied())
        .collect();
    let got = pending();
    eprintln!("frame=2 queue_after_present={got:?} expected={second:?}");
    assert_eq!(got, second);

    for i in [11, 10, 9, NORMAL_A, NORMAL_B, NULL_WRITER, UNRELATED] {
        resume.send(()).unwrap();
        claim(i);
    }
    resume.send(()).unwrap();
    let start = Instant::now();
    while waiting() != 0 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "worker did not drain"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(pending().is_empty());
    assert!(claims.try_recv().is_err(), "a duplicate reached the worker");
    eprintln!("completed=16 unique claims; pending=0");
}

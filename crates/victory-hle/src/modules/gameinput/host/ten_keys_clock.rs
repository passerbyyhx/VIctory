use super::*;

#[test]
fn controlled_clock() {
    let schedule: Vec<_> = parse_script(
        " 1.000 : Enter ,1.100:w:600,1.200:Right:100,2:Tab:bogus,3:Back:0,\
         4:A: 250 ,bad:7,-1:8,5:NoSuchKey,6",
    )
    .collect();
    let expect_hold = std::env::var("TEN_EXPECT_HOLD").as_deref() != Ok("0");
    let rows: &[(u64, &[u8], &[u8])] = &[
        (999, b"", b""),
        (1000, b"\r", b"\r"),
        (1099, b"\r", b"\r"),
        (1100, b"\rW", b"\r"),
        (1199, b"\rW", b"\r"),
        (1200, b"W\x27", b""),
        (1299, b"W\x27", b""),
        (1300, b"W", b""),
        (1699, b"W", b""),
        (1700, b"", b""),
        (1999, b"", b""),
        (2000, b"\t", b""),
        (2199, b"\t", b""),
        (2200, b"", b""),
        (3000, b"", b""),
        (4000, b"A", b""),
        (4249, b"A", b""),
        (4250, b"", b""),
        (6000, b"", b""),
    ];
    for &(ms, head, parent) in rows {
        let active = active_script(&schedule, Duration::from_millis(ms));
        let keys: Vec<_> = active.iter().copied().map(key_state).collect();
        let pad = fallback(&keys);
        eprintln!(
            "TEN_KEYS,{ms},{active:02x?},{},{:.1}",
            pad.buttons, pad.left_y
        );
        let expected = if expect_hold { head } else { parent };
        assert_eq!(active, expected, "elapsed {ms} ms");
        assert_eq!(pad.buttons & A != 0, expected.contains(&0x0d), "{ms} ms");
        assert_eq!(
            pad.left_y,
            if expected.contains(&b'W') { 1.0 } else { 0.0 },
            "{ms} ms"
        );
    }
}

#[test]
fn valid_duration_fields_can_overflow_end() {
    let schedule: Vec<_> = parse_script("18446744073709549568:W:18446744073709551615").collect();
    // A representable start near u64::MAX seconds, plus a representable hold,
    // exceeds Duration::MAX. The actual query is at time zero.
    let result = std::panic::catch_unwind(|| active_script(&schedule, Duration::ZERO));
    let expect_hold = std::env::var("TEN_EXPECT_HOLD").as_deref() != Ok("0");
    eprintln!(
        "TEN_KEYS_OVERFLOW,entries={},panicked={}",
        schedule.len(),
        result.is_err()
    );
    assert_eq!(schedule.len(), usize::from(expect_hold));
    assert_eq!(result.is_err(), expect_hold);
}

# Windows x86-64 measurements for VIctory #364

[PR #364](https://github.com/Marvisak/VIctory/pull/364) introduces an eight-bit prefix lookup in the XMA variable-length code decoder. All inputs here were generated locally. On the synthetic stream below, the median paired speedup over the parent was **1.244×**. The isolated benchmark measured **6.966×** for short codes and **1.033×** across all code lengths.

## Revisions and machine

| Role | Commit |
| --- | --- |
| Parent | `d2a4bae1f49533ef5cb512fd1b1ba1513f2e1989` |
| PR | `e503756597cafb3ac6c259ec82458fa5c61215aa` |

The machine ran Windows build 26300 on an Intel Core i9-14900HX. Both revisions used their pinned Rust 1.98.1 toolchain (`48a229cea`, LLVM 22.1.8), `x86_64-pc-windows-msvc`, Cargo's release profile, and the same external benchmark source.

## Complete decoder run

The fixture generates 16 XMA2 packets containing one 512-sample mono frame each at 48 kHz. Each frame transmits 23 scale factors and 128 four-coefficient vectors. A fixed seed selects among eight six-bit vector codes and varies the coefficient signs. Each timed stream constructs a decoder, traverses every packet with `packet` and `read_frame`, decodes all 16 frames and flushes the final overlap. The output buffer is reused. Stream generation, PCM file I/O, hashes and instrumentation occur outside timing.

Both binaries produced byte-identical little-endian PCM. `fc /b` compared all 16,896 bytes. The 8,448 samples include 8,446 nonzero values, spanning −6,334 to 5,660. SHA-256 is `a312404245407fa9b20d942c92158b61992e9b66768be45811b6419a672f5b27`. A separate untimed copy of the PR library counted **368 one-bit** scale symbols and **2,048 six-bit** vector symbols. All 2,416 calls to `Vlc::read` used the single-candidate lookup.

Each arm of each pair ran as a fresh process on logical CPU 4, with 16 warm-up streams and 2,048 timed streams (32,768 frames). The order alternated in 12 pairs, starting with the parent. Both processes inherited the same affinity before startup. Frequency, temperature, and background OS activity were unmeasured.

| Measurement | Result |
| --- | ---: |
| Parent median | 470.526 ms (14.36 µs/frame) |
| PR median | 380.160 ms (11.60 µs/frame) |
| Median of paired parent/PR times | 1.244× |
| Range across the 12 pairs | 1.061–1.523× |
| Pairs where the PR was faster | 12 of 12 |
| Median when parent ran first / PR ran first | 1.296× / 1.197× |

[stream-pairs.csv](./stream-pairs.csv) contains all 24 timed measurements. An earlier pilot used 256 timed streams per arm. Its 12 paired ratios had a median of 1.214× and a range of 0.557–1.591×, which prompted the longer run. All pilot rows are in [stream-pairs-pilot.csv](./stream-pairs-pilot.csv). This fixture exercises short VLC codes exclusively, so its speedup applies to this workload. Audio with a different code-length distribution requires a separate measurement.

### Reproduce

Use PowerShell 7 and a Windows x64 MSVC toolchain. In the directory containing this README, create two worktrees at the tested revisions, then run both scripts. Change `-Cpu` to an available logical processor.

```powershell
$parentTree = Join-Path $env:TEMP 'victory-xma-364-parent'
$prTree = Join-Path $env:TEMP 'victory-xma-364-pr'
git worktree add --detach $parentTree d2a4bae1f49533ef5cb512fd1b1ba1513f2e1989
git worktree add --detach $prTree e503756597cafb3ac6c259ec82458fa5c61215aa
./measure.ps1 -Base $parentTree -Head $prTree -Cpu 4 -Results ./my-pairs.csv
./audit.ps1 -Head $prTree
```

`measure.ps1` builds both original libraries, compiles the fixture, checks the output hashes and writes a row per timed arm. `audit.ps1` copies the library sources under the PR worktree's `target`, records calls from the instrumented copy and verifies their counts. Both commands leave checkout sources untouched.

## Author's isolated VLC benchmark

The PR's original harness draws 16,384 seeded samples from seven codebooks and visits each sample 64 times per measurement, for 1,048,576 symbols. It compares the production decoder with the original binary search. Each process warms up both paths, then collects five timing pairs in alternating order. Assertions check the sum of decoded symbols and consumed bit counts on every pair.

We ran three processes per revision, serially in PR, parent, parent, PR, PR, parent order with default process affinity. The parent control reused the PR's test functions from `fn vlc_books()` through `fn vlc_prefix_benchmark()`, inserted immediately before the existing `put_bits` helper in the parent test module. Its production lookup remained the parent's original binary search. All equality checks passed. The PR's full release formats suite also passed 21 tests with 4 ignored.

| Corpus | PR process speedups | Parent control process ratios |
| --- | --- | --- |
| All codes | 1.060×, 1.033×, 1.003× | 0.985×, 1.021×, 0.983× |
| Codes of at most eight bits | 6.966×, 7.028×, 6.951× | 0.941×, 0.948×, 0.975× |

The raw reference/candidate timings for all 60 rounds are in [symbol-rounds.csv](./symbol-rounds.csv). The following command runs the author's benchmark once in either prepared worktree.

```powershell
cargo test --locked --release -p victory-formats --lib xma::tests::vlc_prefix_benchmark -- --exact --ignored --nocapture --test-threads=1
```

"""Join CSwitch/ReadyThread records for the scoped four-worker benchmark and its pump."""
from collections import Counter, defaultdict
import csv
import json
from pathlib import Path
import re
import sys

here = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent


def write_table(name, columns, rows):
    with (here / name).open("w", newline="", encoding="utf-8") as out:
        writer = csv.writer(out)
        writer.writerow(columns)
        writer.writerows(rows)


def analyze(rev):
    def local(suffix):
        return here / (rev + "-" + suffix)

    lifetimes = {
        int(t): (float(start), float(stop))
        for t, start, stop in re.findall(r"thread (\d+) start=([\d.]+) stop=([\d.]+)",
                                         local("analysis.log").read_text())
    }
    workers = sorted(lifetimes, key=lambda tid: lifetimes[tid][0])[-4:]
    begin = max(lifetimes[t][0] for t in workers)
    end = min(lifetimes[t][1] for t in workers)
    symbols = json.loads(local("symbols.json").read_text())
    stacks = {}
    for line in local("stacks.tsv").read_text().splitlines():
        key, *tokens = line.split("\t")
        stacks[key] = [symbols[t] for t in tokens if t.startswith(rev + "-time.exe+") and t in symbols]
    timed_pump = {k for k, stack in stacks.items() if any(
        "xma_short_voice_cpu_bench" in name and source == "voice_bench.rs:442"
        for name, _, source in stack)}
    worker_stack = {k for k, stack in stacks.items() if any(
        "::Run::worker" in name or ("WorkerResult" in name and "__rust_begin_short_backtrace" in name)
        for name, _, _ in stack)}
    verified_workers, pumps = set(), set()
    with local("events.tsv").open() as events:
        for line in events:
            e = line.rstrip().split("\t")
            if e[0] == "cs" and begin <= float(e[1]) <= end and e[4] == "Wait":
                tid = int(e[2])
                if e[6] in timed_pump:
                    pumps.add(tid)
                if tid in workers and e[6] in worker_stack:
                    verified_workers.add(tid)
    assert verified_workers == set(workers) and len(pumps) == 1 and not pumps.intersection(workers)
    roles = {**dict.fromkeys(workers, "workers"), **dict.fromkeys(pumps, "pump")}
    write_table(rev + "-roles.csv", ["tid", "role", "start_ms", "end_ms"],
                [[tid, role, *lifetimes[tid]] for tid, role in roles.items()])

    waits = {}
    counts, blocked, queue, quality = Counter(), Counter(), Counter(), Counter()
    by_stack, by_waker = defaultdict(lambda: [0, 0.0]), Counter()
    with local("events.tsv").open() as events:
        for line in events:
            e = line.rstrip().split("\t")
            time = float(e[1])
            if time < begin or time > end:
                continue
            old, new = int(e[2]), int(e[3])
            if e[0] == "ready":
                if old in waits and waits[old][2] is None:
                    waits[old][2:] = [time, roles.get(new, "other target thread" if new in lifetimes else "external")]
                continue
            if new in waits:
                out, key, ready, waker = waits.pop(new)
                role = roles[new]
                if ready is None:
                    quality[role, "switch-in with no ReadyThread"] += 1
                else:
                    assert out <= ready <= time
                    own = stacks.get(key, [])
                    if any("std::sync::barrier::Barrier::wait" in f for f, _, _ in own):
                        bucket = "fixture barrier"
                    elif not own:
                        bucket = "no resolved benchmark frame"
                    else:
                        bucket = "other benchmark stack"
                    group = (role, bucket)
                    counts[group] += 1
                    blocked[group] += ready - out
                    queue[group] += time - ready
                    by_waker[role, waker] += 1
                    row = by_stack[role, key, bucket]
                    row[0] += 1
                    row[1] += ready - out
            if old in roles:
                if e[4] == "Wait":
                    assert old not in waits
                    waits[old] = [time, e[6], None, None]
                else:
                    quality[roles[old], "non-Wait switch-out"] += 1
    for tid in waits:
        quality[roles[tid], "open at window end"] += 1
    write_table(rev + "-waits.csv", ["role", "bucket", "count", "blocked_thread_ms", "ready_thread_ms"],
                [[*g, counts[g], round(blocked[g], 4), round(queue[g], 4)] for g in sorted(counts)])
    write_table(rev + "-quality.csv", ["role", "type", "count"], [[*g, n] for g, n in sorted(quality.items())])
    write_table(rev + "-wakers.csv", ["role", "waker", "count"], [[*g, n] for g, n in sorted(by_waker.items())])
    write_table(rev + "-wait-sites.csv", ["role", "stack", "bucket", "count", "blocked_ms", "own_call_sites"],
                [[role, key, bucket, n, round(ms, 4),
                  " | ".join(f.split("<")[0] + "@" + s for f, _, s in stacks.get(key, [])[:6])]
                 for (role, key, bucket), (n, ms) in sorted(by_stack.items(), key=lambda kv: -kv[1][1])])
    print(rev, "common worker lifetime", begin, end, "ms")


for revision in ("base", "head"):
    analyze(revision)


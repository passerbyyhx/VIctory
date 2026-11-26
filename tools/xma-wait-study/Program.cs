using Microsoft.Diagnostics.Tracing.Etlx;
using Microsoft.Diagnostics.Tracing.Parsers.Kernel;
using System.Globalization;

CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
using var log = TraceLog.OpenOrConvert(args[0], null);
int pid = int.Parse(args[1]);
var proc = log.Processes.Single(p => p.ProcessID == pid && p.Name.EndsWith("-time", StringComparison.OrdinalIgnoreCase));
var tids = proc.Threads.Select(t => t.ThreadID).ToHashSet();
Console.WriteLine($"EventsLost={log.EventsLost}, truncated={log.Truncated}, threads={tids.Count}, process={proc.Name}");
foreach (var t in proc.Threads) Console.WriteLine($"thread {t.ThreadID} start={t.StartTimeRelativeMSec:F3} stop={t.EndTimeRelativeMSec:F3} cpu={t.CPUMSec:F3}");
var directory = Path.GetDirectoryName(Path.GetFullPath(args[0]))!;
var stem = Path.Combine(directory, Path.GetFileNameWithoutExtension(args[0]));
using var output = new StreamWriter(stem + "-events.tsv");
using var stacks = new StreamWriter(stem + "-stacks.tsv");
var seen = new HashSet<CallStackIndex>();
void Save(CallStackIndex idx) {
    if (idx == CallStackIndex.Invalid || !seen.Add(idx)) return;
    stacks.Write((int)idx);
    for (var s = log.CallStacks[idx]; s != null; s = s.Caller) {
        var a = s.CodeAddress;
        var m = a.ModuleFile;
        string module = m == null ? "unknown" : Path.GetFileName(m.FilePath);
        ulong offset = m == null ? a.Address : a.Address - m.ImageBase;
        stacks.Write($"\t{module}+{offset:x}");
    }
    stacks.WriteLine();
}
foreach (var e in proc.EventsDuringProcess) {
    if (e is CSwitchTraceData cs) {
        bool old = tids.Contains(cs.OldThreadID), next = tids.Contains(cs.NewThreadID);
        if (!old && !next) continue;
        var block = cs.BlockingStack();
        var normal = cs.CallStackIndex();
        Save(block); Save(normal);
        output.WriteLine($"cs\t{cs.TimeStampRelativeMSec:F6}\t{cs.OldThreadID}\t{cs.NewThreadID}\t{cs.OldThreadState}\t{cs.OldThreadWaitReason}\t{(int)block}\t{(int)normal}");
    } else if (e is DispatcherReadyThreadTraceData w && tids.Contains(w.AwakenedThreadID)) {
        var stack = w.CallStackIndex();
        Save(stack);
        output.WriteLine($"ready\t{w.TimeStampRelativeMSec:F6}\t{w.AwakenedThreadID}\t{w.ThreadID}\t{w.ProcessID}\t{(int)stack}");
    }
}


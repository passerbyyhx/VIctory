from pathlib import Path
import sys
src = Path(sys.argv[1]) / "crates/victory-gpu/src"
def insert(file, anchor, text):
    path = src / file
    current = path.read_text(encoding="utf8")
    if text+anchor in current: return
    assert current.count(anchor)==1,(file,anchor)
    path.write_text(current.replace(anchor,text+anchor),encoding="utf8")
insert("bindless.rs","    /// The same with the buffer and the texture",Path(__file__).with_name("fixture.rs").read_text())
insert("copystat.rs","pub(crate) fn add(c: C",'''#[cfg(test)]
pub(crate) fn probe_slots() -> u64 { COUNT[C::SyncSlots as usize].load(Relaxed) }

''')
insert("descriptor/host.rs","impl HostHeap {\n    pub(crate) fn new",'''#[cfg(test)]
pub(super) static PROBE_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
pub(super) static PROBE_WARN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

''')
insert("descriptor/host.rs","        let records: Vec<[u32; 3]> =",'''        #[cfg(test)]
        PROBE_READS.fetch_add(u64::from(last-first),std::sync::atomic::Ordering::Relaxed);
''')
insert("descriptor/host.rs","                tracing::warn!(\n                    index,\n                    count = self.count,",'''                #[cfg(test)]
                PROBE_WARN.store(index,std::sync::atomic::Ordering::Relaxed);
''')
insert("descriptor.rs","impl Heap {",'''#[cfg(all(test,windows,feature="diagnostics"))]
pub(crate) fn probe_counts() -> (u64,u64,Option<u32>) {
    use std::sync::atomic::Ordering::Relaxed;
    let w = host::PROBE_WARN.load(Relaxed);
    (crate::copystat::probe_slots(),host::PROBE_READS.load(Relaxed),(w!=u32::MAX).then_some(w))
}
#[cfg(all(test,windows,feature="diagnostics"))]
impl Heap {
    pub(crate) fn probe_cap(&self,count:u32) { assert!(self.host.set(host::HostHeap::new(false,count)).is_ok()); }
    pub(crate) fn probe_drain_initial(&self) { assert!(!self.written.as_ref().unwrap().take(self.start as usize,usize::MAX).is_empty()); }
}

''')

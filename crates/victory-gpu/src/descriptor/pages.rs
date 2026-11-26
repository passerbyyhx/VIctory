//! A paged view heap's pages ([`super::host::view_pages`]): where each page
//! of `PAGE` slots is in the host heap, entered in the page table shaders
//! read. A page is placed when a slot of it is first written, and taken
//! back when room runs low and no shader or command used it lately.
//!
//! Grand Theft Auto VI writes fresh descriptors over most of its 1,245,184
//! slots in a run: pages placed on first write and never given back filled
//! the 2,880 host pages NVIDIA leaves the title within minutes (RTX 4090,
//! #2121: 1,024 pages refused with ~231,000 slots written, ~226 of 256 a
//! page; smaller pages would not fit more). Pages are taken back oldest use
//! first, between submissions; one read again is placed again and its
//! slots made from the title's records, so a wrong guess costs the
//! submission that read it null.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Slots per page of a paged view heap.
pub(super) const PAGE: u32 = victory_shader::emit::VIEW_PAGE_SLOTS;
/// Host pages every paged heap keeps: all null (where unplaced pages
/// read), and a scratch page (what is written to slots of pages there was
/// no room for, read by nothing).
pub(super) const NULL_PAGE: u32 = 0;
pub(super) const SCRATCH_PAGE: u32 = 1;

/// Pages are taken back once fewer than 1/`LOW_WATER` of the host pages
/// are free, until 1/`HIGH_WATER` are.
const LOW_WATER: u32 = 32;
const HIGH_WATER: u32 = 16;
/// Only pages no shader read, no command bound and no write reached for
/// this many submissions and this long are taken back (the strict ages):
/// the title's world streams in over seconds, and a frame takes a few
/// submissions.
const MIN_AGE_SUBMISSIONS: u32 = 300;
const MIN_AGE: Duration = Duration::from_secs(10);
/// Where a write found no host page since the last pass, pages unused for
/// this many submissions are taken back too, however recently in time:
/// more than a frame's submissions, so pages read every frame stay.
const PRESSED_AGE_SUBMISSIONS: u32 = 32;

/// A paged view heap's pages: the host page of each page of slots (0 for
/// none: [`NULL_PAGE`]), entered in the page table shaders read, and when
/// each was last used.
pub(super) struct Pages {
    placed: Mutex<Placed>,
    /// Host pages in all.
    end: u32,
    /// The page table, a dword per page, in the view-stride table's buffer.
    table: usize,
    /// Per page of slots, the submissions run before its last use: a slot
    /// written, a page read mark (`crate::shadow::take_page_reads`), a
    /// shader-visible handle given out or a draw binding a slot of it.
    used: Box<[AtomicU32]>,
    /// Submissions run so far.
    now: AtomicU32,
}

/// [`Pages`] as they are placed, written and taken back.
struct Placed {
    /// Per page of slots, its host page.
    host: Vec<u32>,
    /// The next host page never given out.
    next: u32,
    /// The first host page given out on a write (those before it hold the
    /// dynamic slots, placed at once, and are never taken back).
    first: u32,
    /// Pages of slots below this hold title slots only, which are made
    /// again from the title's records: those placed on a write may be
    /// taken back (pages past it hold the host's own pairs and null views).
    title: u32,
    /// Per page of slots, a bit per slot written: how full the pages placed
    /// on a write are tells whether smaller pages would fit more of a title.
    written: Vec<u64>,
    /// Slots written in the pages placed on a write that hold them now.
    slots: u64,
    /// Pages written when no host page was left, and their slots written.
    refused: u32,
    refused_slots: u64,
    /// Whether a write found no host page since the last pass of
    /// [`Pages::take_back`].
    pressed: bool,
    /// Whether the last pass took back pages younger than the strict ages,
    /// and pages taken back so by all passes.
    relaxed: bool,
    taken_relaxed: u64,
    /// Host pages taken back, given out before `next`.
    free: Vec<u32>,
    /// Per page of slots, whether the title has records there that its
    /// host page lacks, and why: made from them when it is placed.
    lost: Vec<Lost>,
    /// Pages placed that are to be made from the title's records.
    remake: Vec<u32>,
    /// Pages taken back so far.
    taken_back: u64,
    /// Pages placed after they were taken back, and after they were
    /// written with no room, so far.
    placed_again: u64,
    placed_late: u64,
    /// Submissions (as counted in `Pages::now`) and when they ended: the
    /// first is the last that ended at least `MIN_AGE` ago, if any did.
    ended: VecDeque<(u32, Instant)>,
    /// Free host pages below which pages are taken back, and up to which.
    low: u32,
    high: u32,
}

/// Why a page of slots lacks records the title has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lost {
    No,
    /// Taken back ([`Pages::take_back`]).
    TakenBack,
    /// Written when no host page was free.
    Refused,
}

// A page's bits are whole words.
const _: () = assert!(PAGE.is_multiple_of(64));

impl Placed {
    /// Notes slot `index` written: whether it is the slot's first write,
    /// and its page's.
    fn note(&mut self, index: u32) -> (bool, bool) {
        let bits = self.bits(index / PAGE);
        let page_new = bits.iter().all(|&w| w == 0);
        let k = (index % PAGE) as usize;
        let bit = 1u64 << (k % 64);
        let new = bits[k / 64] & bit == 0;
        bits[k / 64] |= bit;
        (new, new && page_new)
    }

    /// Page `page`'s written bits.
    fn bits(&mut self, page: u32) -> &mut [u64] {
        let words = (PAGE / 64) as usize;
        let at = page as usize * words;
        &mut self.written[at..at + words]
    }

    /// Slots written in page `page`.
    fn written_in(&mut self, page: u32) -> u64 {
        self.bits(page).iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// Host pages given out on a write and not taken back.
    fn in_use(&self) -> u32 {
        self.next - self.first - self.free.len() as u32
    }

    /// Slots in the pages placed on a write.
    fn placed_slots(&self) -> u64 {
        u64::from(self.in_use()) * u64::from(PAGE)
    }
}

impl Pages {
    /// `host` per page of slots (placed ones entered in the table already),
    /// `next` the first host page to give out on a write, of `end`; pages
    /// below `title` hold title slots only.
    pub(super) fn new(host: Vec<u32>, next: u32, end: u32, table: usize, title: u32) -> Self {
        let pages = host.len();
        let free = end.saturating_sub(next);
        Self {
            placed: Mutex::new(Placed {
                written: vec![0; pages * (PAGE / 64) as usize],
                lost: vec![Lost::No; pages],
                host,
                next,
                first: next,
                title,
                slots: 0,
                refused: 0,
                refused_slots: 0,
                pressed: false,
                relaxed: false,
                taken_relaxed: 0,
                free: Vec::new(),
                remake: Vec::new(),
                taken_back: 0,
                placed_again: 0,
                placed_late: 0,
                ended: VecDeque::new(),
                low: end / LOW_WATER,
                high: (end / HIGH_WATER).min(free),
            }),
            end,
            table,
            used: (0..pages).map(|_| AtomicU32::new(0)).collect(),
            now: AtomicU32::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Placed> {
        self.placed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Enters host page `host` for page `page` in the page table.
    fn enter(&self, page: usize, host: u32) {
        // SAFETY: the mapped page table has a dword per page.
        unsafe { (self.table as *mut u32).add(page).write_volatile(host) };
    }

    /// Page `page` was used now.
    fn stamp_page(&self, page: usize) {
        if let Some(used) = self.used.get(page) {
            used.store(self.now.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    }

    /// Slot `index` was used now (a draw binds it).
    pub(super) fn stamp(&self, index: u32) {
        self.stamp_page((index / PAGE) as usize);
    }

    /// The host slot of slot `index`, placing its page if it has none and
    /// `place` (a write); None where it has none (none left, or not to
    /// place). Its page is used now.
    pub(super) fn slot(&self, index: u32, place: bool) -> Option<u32> {
        let page = (index / PAGE) as usize;
        let mut guard = self.lock();
        let p = &mut *guard;
        let host = match *p.host.get(page)? {
            NULL_PAGE if !place => return None,
            NULL_PAGE => {
                let Some(host) = self.place_at(p, page) else {
                    p.pressed = true;
                    let (new, page_new) = p.note(index);
                    p.refused_slots += u64::from(new);
                    if p.lost[page] == Lost::No {
                        p.lost[page] = Lost::Refused;
                    }
                    if page_new {
                        p.refused += 1;
                        if p.refused == 1 {
                            tracing::warn!(
                                pages = self.end,
                                written = p.slots,
                                of = p.placed_slots(),
                                "no room left in the host view heap: slots of further pages read null"
                            );
                        } else if p.refused.is_power_of_two() {
                            tracing::info!(
                                pages = p.refused,
                                written = p.refused_slots,
                                placed_written = p.slots,
                                of = p.placed_slots(),
                                "view heap pages written with no room left"
                            );
                        }
                    }
                    return None;
                };
                p.slots += u64::from(p.note(index).0);
                host
            }
            host => {
                if place && host >= p.first {
                    p.slots += u64::from(p.note(index).0);
                }
                host
            }
        };
        self.stamp_page(page);
        Some(host * PAGE + index % PAGE)
    }

    /// Places page `page` (which has none) in a host page taken back, else
    /// one never given out: None where none is left.
    fn place_at(&self, p: &mut Placed, page: usize) -> Option<u32> {
        let reused = p.free.pop();
        let host = match reused {
            Some(host) => host,
            None if p.next < self.end => {
                p.next += 1;
                p.next - 1
            }
            None => return None,
        };
        p.host[page] = host;
        self.enter(page, host);
        self.stamp_page(page);
        // Slots written while it had no place count from now.
        p.slots += p.written_in(page as u32);
        let lost = std::mem::replace(&mut p.lost[page], Lost::No);
        if lost != Lost::No {
            p.remake.push(page as u32);
            let count = match lost {
                Lost::TakenBack => &mut p.placed_again,
                _ => &mut p.placed_late,
            };
            *count += 1;
            // Logged where either total reaches a power of two.
            if count.is_power_of_two() {
                tracing::info!(
                    pages = p.placed_again + p.placed_late,
                    taken_back = p.placed_again,
                    refused = p.placed_late,
                    in_use = p.in_use(),
                    "view heap pages placed again: their slots are made from the title's records"
                );
            }
        }
        if reused.is_none() && (host.is_power_of_two() || p.next == self.end) {
            tracing::info!(
                pages = host + 1,
                of = self.end,
                written = p.slots,
                of_slots = p.placed_slots(),
                "host view heap pages in use"
            );
        }
        Some(host)
    }

    /// Places page `page` if it has no place and the title has records
    /// there (a page read after it was taken back): whether it has one now.
    pub(super) fn place(&self, page: u32) -> bool {
        let mut p = self.lock();
        let page = page as usize;
        match p.host.get(page) {
            None => false,
            Some(&NULL_PAGE) => p.lost[page] != Lost::No && self.place_at(&mut p, page).is_some(),
            Some(_) => true,
        }
    }

    /// After a submission: pages `read` (their read marks) were used now;
    /// those of them without a place whose slots the title has records for
    /// are returned (to be placed again).
    pub(super) fn read(&self, read: &[u32]) -> Vec<u32> {
        let p = self.lock();
        let mut again = Vec::new();
        for &page in read {
            let page = page as usize;
            self.stamp_page(page);
            if p.host.get(page) == Some(&NULL_PAGE) && p.lost[page] != Lost::No {
                again.push(page as u32);
            }
        }
        again
    }

    /// A submission ended at `at`. Where fewer host pages than the low-water
    /// mark are free, pages placed on a write that hold title slots only
    /// and were last used `MIN_AGE_SUBMISSIONS` submissions and `MIN_AGE`
    /// ago or more are taken back, least recently used first, up to the
    /// high-water mark. Where a write found no host page since the last
    /// pass, pages last used `PRESSED_AGE_SUBMISSIONS` submissions ago or
    /// more are taken back too, after the older ones. Each reads the null
    /// page from now, its slots to be made again when it is placed again.
    /// Returns them and their host pages, which go back to [`Pages::give`]
    /// once nothing they held is left there.
    pub(super) fn take_back(&self, at: Instant) -> Vec<(u32, u32)> {
        let now = self.now.fetch_add(1, Ordering::Relaxed) + 1;
        let mut guard = self.lock();
        let p = &mut *guard;
        let pressed = std::mem::take(&mut p.pressed);
        p.ended.push_back((now, at));
        while p.ended.len() > 1 && p.ended[1].1 + MIN_AGE <= at {
            p.ended.pop_front();
        }
        let free = p.free.len() as u32 + (self.end - p.next);
        if free >= p.low {
            return Vec::new();
        }
        // A page stamped `s` was last used before submission `s + 1` ended.
        let strict = p
            .ended
            .front()
            .filter(|&&(_, ended)| ended + MIN_AGE <= at)
            .map(|&(old, _)| old.min(now.saturating_sub(MIN_AGE_SUBMISSIONS)));
        // Pages old by the strict ages are older still, so taken first.
        let before = match strict {
            _ if pressed => now.saturating_sub(PRESSED_AGE_SUBMISSIONS),
            Some(before) => before,
            None => return Vec::new(),
        };
        let mut stale: Vec<(u32, u32)> = (0..p.title.min(p.host.len() as u32))
            .filter(|&page| p.host[page as usize] >= p.first)
            .map(|page| (self.used[page as usize].load(Ordering::Relaxed), page))
            .filter(|&(used, _)| used < before)
            .collect();
        stale.sort_unstable();
        stale.truncate(p.high.saturating_sub(free) as usize);
        let strict = strict.unwrap_or(0);
        let young = stale.iter().filter(|&&(used, _)| used >= strict).count();
        p.relaxed = young > 0;
        p.taken_relaxed += young as u64;
        stale
            .into_iter()
            .map(|(_, page)| {
                let host = std::mem::replace(&mut p.host[page as usize], NULL_PAGE);
                self.enter(page as usize, NULL_PAGE);
                p.lost[page as usize] = Lost::TakenBack;
                p.slots -= p.written_in(page);
                p.bits(page).fill(0);
                p.taken_back += 1;
                (page, host)
            })
            .collect()
    }

    /// Host pages taken back ([`Pages::take_back`]) are free to give out.
    pub(super) fn give(&self, hosts: &[u32]) {
        if hosts.is_empty() {
            return;
        }
        let mut p = self.lock();
        p.free.extend_from_slice(hosts);
        let n = hosts.len() as u64;
        // Logged where the total passes a power of two.
        if (p.taken_back - n + 1..=p.taken_back).any(u64::is_power_of_two) {
            tracing::info!(
                pages = n,
                total = p.taken_back,
                free = p.free.len() as u32 + (self.end - p.next),
                in_use = p.in_use(),
                relaxed = p.relaxed,
                relaxed_total = p.taken_relaxed,
                "view heap pages unused lately taken back: they read null until placed again"
            );
        }
    }

    /// The pages placed since the last call that are to be made from the
    /// title's records.
    pub(super) fn remakes(&self) -> Vec<u32> {
        std::mem::take(&mut self.lock().remake)
    }

    /// The host page of page `page` (tests).
    #[cfg(test)]
    pub(super) fn host_page(&self, page: u32) -> u32 {
        self.lock().host[page as usize]
    }

    #[cfg(test)]
    pub(super) fn limit_unused_pages(&mut self, n: u32) {
        let next = self.lock().next;
        assert!(n <= self.end - next);
        self.end = next + n;
    }

    #[cfg(test)]
    pub(super) fn counts(&self) -> (u32, u32, u64, u64, u64) {
        let p = self.lock();
        (
            self.end - p.next + p.free.len() as u32,
            p.in_use(),
            p.taken_back,
            p.placed_again,
            p.placed_late,
        )
    }

    /// Takes pages back below `low` free host pages, up to `high` (tests).
    #[cfg(test)]
    pub(super) fn set_water(&self, low: u32, high: u32) {
        let mut p = self.lock();
        (p.low, p.high) = (low, high);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGES: usize = 8;

    /// Pages over `table`, all unplaced, host pages from the first past the
    /// scratch page up to `end`; all title pages.
    fn pages(table: &mut [u32], end: u32) -> Pages {
        let (n, data) = (table.len(), table.as_mut_ptr() as usize);
        Pages::new(vec![NULL_PAGE; n], SCRATCH_PAGE + 1, end, data, n as u32)
    }

    fn at(page: u32, k: u32) -> u32 {
        page * PAGE + k
    }

    /// `n` submissions ending `step` apart from `*t` on, none taking back.
    fn run(pages: &Pages, t: &mut Instant, n: u32, step: Duration) {
        for _ in 0..n {
            *t += step;
            assert!(pages.take_back(*t).is_empty());
        }
    }

    const MS: Duration = Duration::from_millis(1);
    const SECOND: Duration = Duration::from_secs(1);

    /// A paged heap's pages are placed on first write, in turn, each once,
    /// and entered in the page table; reads place none; past the host's
    /// pages, and past the virtual ones, a slot has no place.
    #[test]
    fn pages_are_placed_once_on_first_write() {
        let mut table = vec![0u32; PAGES];
        let pages = pages(&mut table, 4);
        assert_eq!(pages.slot(at(3, 5), false), None, "a read places nothing");
        assert_eq!(pages.slot(at(3, 5), true), Some(at(2, 5)));
        assert_eq!(pages.slot(at(3, 200), false), Some(at(2, 200)), "the same page");
        assert_eq!(pages.slot(at(6, 0), true), Some(at(3, 0)));
        assert_eq!(pages.slot(at(7, 1), true), None, "no host page left");
        assert_eq!(pages.slot(at(8, 0), true), None, "past the virtual pages");
        assert_eq!(pages.lock().next, 4);
        drop(pages);
        assert_eq!(table, [0, 0, 0, 2, 0, 0, 3, 0]);
    }

    /// Slots written are counted once each, in the pages placed on a write
    /// (not the ones placed at once) and in the pages with no room left;
    /// reads count none.
    #[test]
    fn written_slots_are_counted_per_page() {
        let mut table = vec![0u32; PAGES];
        let mut host = vec![NULL_PAGE; PAGES];
        host[0] = SCRATCH_PAGE + 1; // placed at once, as the dynamic slots
        let data = table.as_mut_ptr() as usize;
        let pages = Pages::new(host, SCRATCH_PAGE + 2, 5, data, PAGES as u32);
        for k in [0, 1, 1, 63, 64, PAGE - 1] {
            pages.slot(at(2, k), true);
        }
        pages.slot(at(2, 7), false);
        pages.slot(at(0, 3), true);
        pages.slot(at(4, 9), true);
        {
            let p = pages.lock();
            assert_eq!(p.next, 5);
            assert_eq!((p.slots, p.placed_slots()), (6, 2 * u64::from(PAGE)));
            assert_eq!((p.refused, p.refused_slots), (0, 0));
        }
        pages.slot(at(5, 1), true);
        pages.slot(at(5, 1), true);
        pages.slot(at(5, 2), true);
        pages.slot(at(6, 0), true);
        let p = pages.lock();
        assert_eq!((p.refused, p.refused_slots), (2, 3));
        assert_eq!(p.slots, 6);
        drop(p);
        drop(pages);
        assert_eq!(table, [0, 0, 3, 0, 4, 0, 0, 0]);
    }

    /// Where room runs low, pages unused for `MIN_AGE_SUBMISSIONS`
    /// submissions and `MIN_AGE` are taken back, least recently used first,
    /// up to the high-water mark; their table entries read the null page and
    /// their host pages go to the next pages written, before any never
    /// given out.
    #[test]
    fn pages_unused_lately_are_taken_back_oldest_first() {
        let mut table = vec![0u32; PAGES];
        // Host pages 2-8.
        let pages = pages(&mut table, 9);
        pages.set_water(2, 3);
        let mut t = Instant::now();
        // Pages 0-5 written in turn, a submission apart: oldest first.
        for page in 0..6 {
            pages.slot(at(page, 1), true);
            run(&pages, &mut t, 1, MS);
        }
        // Room runs low: no host page left.
        pages.slot(at(6, 0), true);
        // Unused for as many submissions, not as long: none.
        run(&pages, &mut t, MIN_AGE_SUBMISSIONS, MS);
        // As long too: the three oldest, to reach three free.
        t += MIN_AGE;
        let taken = pages.take_back(t);
        assert_eq!(taken, [(0, 2), (1, 3), (2, 4)]);
        assert_eq!(pages.slot(at(0, 1), false), None, "taken back: no place");
        pages.give(&[2, 3, 4]);
        assert_eq!(pages.lock().in_use(), 4);
        run(&pages, &mut t, 1, MS);
        // The next page written takes a host page taken back.
        assert_eq!(pages.slot(at(7, 0), true), Some(at(4, 0)));
        drop(pages);
        assert_eq!(table, [0, 0, 0, 5, 6, 7, 8, 4]);
    }

    /// A page is taken back only once unused for both
    /// `MIN_AGE_SUBMISSIONS` submissions and `MIN_AGE`.
    #[test]
    fn pages_are_taken_back_only_once_old_by_both_counts() {
        let mut table = vec![0u32; PAGES];
        let pages = pages(&mut table, 3);
        pages.set_water(1, 1);
        pages.slot(at(0, 0), true);
        let mut t = Instant::now();
        run(&pages, &mut t, 1, MS);
        // As long, not as many submissions.
        t += 2 * MIN_AGE;
        assert!(pages.take_back(t).is_empty());
        run(&pages, &mut t, MIN_AGE_SUBMISSIONS - 2, MS);
        t += MS;
        assert_eq!(pages.take_back(t), [(0, 2)]);
    }

    /// Pages placed at once (the dynamic slots) and pages past the title's
    /// are never taken back, however old.
    #[test]
    fn fixed_and_host_pages_are_never_taken_back() {
        let mut table = vec![0u32; PAGES];
        let mut host = vec![NULL_PAGE; PAGES];
        host[4] = SCRATCH_PAGE + 1; // placed at once, as the dynamic slots
        let pages = Pages::new(host, SCRATCH_PAGE + 2, 6, table.as_mut_ptr() as usize, 6);
        pages.set_water(10, 10);
        pages.slot(at(4, 0), true);
        pages.slot(at(6, 0), true); // past the title's pages (a pair)
        pages.slot(at(1, 0), true);
        let mut t = Instant::now();
        let mut taken = Vec::new();
        for _ in 0..MIN_AGE_SUBMISSIONS + 20 {
            t += SECOND;
            taken.extend(pages.take_back(t));
        }
        assert_eq!(taken, [(1, 4)], "the title's page placed on a write only");
        assert_eq!(pages.host_page(4), SCRATCH_PAGE + 1);
        assert_eq!(pages.host_page(6), 3);
    }

    /// A page written, read (its read mark) or used (a handle given out, a
    /// draw binding it) lately stays; one read after it was taken back is
    /// placed again ([`Pages::read`], [`Pages::place`]) and to be made from
    /// the title's records, as is one written.
    #[test]
    fn pages_used_lately_stay_and_taken_back_ones_are_placed_again() {
        let mut table = vec![0u32; PAGES];
        let pages = pages(&mut table, 7);
        pages.set_water(0, 0);
        for page in 0..5 {
            pages.slot(at(page, 0), true);
        }
        let mut t = Instant::now();
        run(&pages, &mut t, MIN_AGE_SUBMISSIONS + 20, SECOND);
        pages.set_water(PAGES as u32, PAGES as u32);
        // Used now: written, read, a handle given out, bound by a draw.
        pages.slot(at(0, 9), true);
        assert!(pages.read(&[1]).is_empty(), "placed: nothing to place again");
        pages.slot(at(2, 3), false);
        pages.stamp(at(3, 4));
        t += SECOND;
        let taken = pages.take_back(t);
        assert_eq!(taken, [(4, 6)], "the page unused lately only");
        pages.give(&[6]);
        assert!(pages.remakes().is_empty());
        // Read again: placed again, in its old host page.
        assert_eq!(pages.read(&[4]), [4]);
        assert!(pages.place(4));
        assert_eq!(pages.host_page(4), 6);
        assert_eq!(pages.remakes(), [4], "its slots to be made again");
        // All taken back, then one written: placed again too.
        pages.set_water(0, 0);
        run(&pages, &mut t, MIN_AGE_SUBMISSIONS + 20, SECOND);
        pages.set_water(PAGES as u32, PAGES as u32);
        t += SECOND;
        let taken = pages.take_back(t);
        assert_eq!(taken.len(), 5, "all unused lately");
        pages.give(&taken.iter().map(|&(_, h)| h).collect::<Vec<_>>());
        assert!(pages.read(&[5]).is_empty(), "never written: nothing to place");
        assert!(pages.slot(at(2, 1), true).is_some());
        assert_eq!(pages.remakes(), [2]);
        let p = pages.lock();
        assert_eq!((p.taken_back, p.placed_again, p.placed_late), (6, 2, 0));
        assert_eq!(p.slots, 1, "the slot written since");
    }

    /// A page written when no host page was left is placed, and made from
    /// the title's records, once one is free.
    #[test]
    fn pages_refused_room_are_placed_again_when_there_is() {
        let mut table = vec![0u32; PAGES];
        let pages = pages(&mut table, 3);
        pages.set_water(1, 1);
        pages.slot(at(0, 0), true);
        assert_eq!(pages.slot(at(1, 5), true), None, "no room");
        let mut t = Instant::now();
        for _ in 0..MIN_AGE_SUBMISSIONS + 20 {
            t += SECOND;
            for (_, host) in pages.take_back(t) {
                pages.give(&[host]);
            }
        }
        assert_eq!(pages.host_page(0), NULL_PAGE, "taken back");
        assert_eq!(pages.read(&[1]), [1]);
        assert!(pages.place(1));
        assert_eq!(pages.remakes(), [1]);
        let p = pages.lock();
        assert_eq!(p.slots, 1, "the slot written with no room");
        assert_eq!((p.taken_back, p.placed_again, p.placed_late), (1, 0, 1));
    }

    /// A page taken back and then written with no room is counted placed
    /// again, not placed late, once it has room.
    #[test]
    fn pages_taken_back_then_refused_room_count_as_placed_again() {
        let mut table = vec![0u32; PAGES];
        let pages = pages(&mut table, 3);
        pages.set_water(1, 1);
        pages.slot(at(0, 0), true);
        let mut t = Instant::now();
        let mut taken = Vec::new();
        for _ in 0..MIN_AGE_SUBMISSIONS + 20 {
            t += SECOND;
            taken.extend(pages.take_back(t));
        }
        assert_eq!(taken, [(0, 2)]);
        // Its host page not yet free: no room for it, nor a new page.
        assert_eq!(pages.slot(at(0, 4), true), None);
        assert_eq!(pages.slot(at(1, 0), true), None);
        pages.give(&[2]);
        assert!(pages.place(0));
        assert_eq!(pages.remakes(), [0]);
        let p = pages.lock();
        assert_eq!((p.placed_again, p.placed_late), (1, 0));
        assert_eq!((p.lost[0], p.lost[1]), (Lost::No, Lost::Refused));
    }

    /// With no write refused room, pages younger than the strict ages stay
    /// however few host pages are free.
    #[test]
    fn young_pages_stay_while_no_write_is_refused() {
        let mut table = vec![0u32; PAGES];
        // Host pages 2-8, all placed.
        let pages = pages(&mut table, 9);
        for page in 0..7 {
            pages.slot(at(page, 0), true);
        }
        pages.set_water(PAGES as u32, PAGES as u32);
        let mut t = Instant::now();
        run(&pages, &mut t, 4 * PRESSED_AGE_SUBMISSIONS, MS);
        assert!(!pages.lock().pressed);
    }

    /// After a write found no host page, pages unused for more than
    /// `PRESSED_AGE_SUBMISSIONS` submissions are taken back, least recently
    /// used first, up to the high-water mark, though younger than
    /// `MIN_AGE`; pages used since stay.
    #[test]
    fn pages_unused_for_a_few_submissions_are_taken_back_once_a_write_is_refused() {
        let mut table = vec![0u32; PAGES];
        // Host pages 2-8.
        let pages = pages(&mut table, 9);
        let mut t = Instant::now();
        // Pages 0-5 written in turn, a submission apart: oldest first.
        for page in 0..6 {
            pages.slot(at(page, 1), true);
            run(&pages, &mut t, 1, MS);
        }
        run(&pages, &mut t, 1, MS);
        // Page 2 used `PRESSED_AGE_SUBMISSIONS` submissions before the pass.
        pages.slot(at(2, 1), false);
        run(&pages, &mut t, PRESSED_AGE_SUBMISSIONS - 1, MS);
        // Page 1 used now; page 6 takes the last host page, page 7 none.
        pages.stamp(at(1, 0));
        assert_eq!(pages.slot(at(6, 0), true), Some(at(8, 0)));
        assert_eq!(pages.slot(at(7, 0), true), None, "no room");
        pages.set_water(1, 3);
        t += MS;
        assert_eq!(pages.take_back(t), [(0, 2), (3, 5), (4, 6)]);
        let p = pages.lock();
        assert!(p.relaxed && !p.pressed);
        assert_eq!((p.taken_back, p.taken_relaxed), (3, 3));
    }

    /// A write refused room relaxes the next pass only.
    #[test]
    fn a_refused_write_relaxes_one_pass() {
        let mut table = vec![0u32; PAGES];
        // Host pages 2-8, all placed.
        let pages = pages(&mut table, 9);
        for page in 0..7 {
            pages.slot(at(page, 0), true);
        }
        let mut t = Instant::now();
        run(&pages, &mut t, PRESSED_AGE_SUBMISSIONS + 1, MS);
        // A pass with room enough still ends the pressure.
        assert_eq!(pages.slot(at(7, 0), true), None, "no room");
        run(&pages, &mut t, 1, MS);
        pages.set_water(1, 3);
        run(&pages, &mut t, 1, MS);
        // Refused again: one pass takes back up to the high-water mark.
        assert_eq!(pages.slot(at(7, 0), true), None, "no room");
        t += MS;
        assert_eq!(pages.take_back(t), [(0, 2), (1, 3), (2, 4)]);
        // Their host pages not yet given back: room still short, pressure gone.
        run(&pages, &mut t, 1, MS);
        assert_eq!(pages.slot(at(7, 0), true), None, "no room");
        t += MS;
        assert_eq!(pages.take_back(t), [(3, 5), (4, 6), (5, 7)]);
    }
}

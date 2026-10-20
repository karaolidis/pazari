use std::{ffi::c_void, io, num::NonZeroU64, ptr, str::FromStr};

use rustix::{
    mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap},
    param::page_size,
};
use serde::Serialize;
use thiserror::Error;
use tracing::warn;

/// Decorrelates the fills of neighbouring pages.
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// What a touched page is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// One word taken from the page's own ordinal, so no two pages fold together.
    #[default]
    Index,

    /// A whole page of noise, so zram cannot hold the page for less than it costs.
    Random,
}

impl FromStr for Fill {
    type Err = FillError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "index" => Ok(Self::Index),
            "random" => Ok(Self::Random),
            _ => Err(FillError { value: text.into() }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{value:?} is not a fill: expected index or random")]
pub struct FillError {
    value: String,
}

/// Whether the arena is charged to the commit limit when it is mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// Charged up front.
    Reserve,

    /// Mapped `MAP_NORESERVE`, so never charged up front.
    NoReserve,
}

/// Whether the arena may be backed by transparent huge pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HugePages {
    /// Left to the system's policy.
    Allowed,

    /// Advised `MADV_NOHUGEPAGE`.
    Excluded,
}

/// One anonymous private mapping, touched from the bottom up.
#[derive(Debug)]
pub struct Region {
    base: *mut c_void,
    len: usize,
    page: usize,
    fill: Fill,
}

impl Region {
    /// Maps an arena of at least `bytes`, rounded up to a whole page.
    pub fn map(
        bytes: NonZeroU64,
        commit: Commit,
        huge_pages: HugePages,
        fill: Fill,
    ) -> Result<Self, RegionError> {
        let page = page_size();

        let len = usize::try_from(bytes.get())
            .ok()
            .and_then(|len| len.checked_next_multiple_of(page))
            .ok_or_else(|| RegionError::Vast { bytes: bytes.get() })?;

        let mut flags = MapFlags::PRIVATE;

        if commit == Commit::NoReserve {
            flags |= MapFlags::NORESERVE;
        }

        // SAFETY: a null hint leaves the address to the kernel, and the mapping is ours alone.
        let base = unsafe {
            mmap_anonymous(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                flags,
            )
        }
        .map_err(|source| RegionError::Map {
            bytes: widen(len),
            source: source.into(),
        })?;

        let region = Self {
            base,
            len,
            page,
            fill,
        };

        if huge_pages == HugePages::Excluded {
            // SAFETY: the whole mapping is ours, and advice neither reads nor writes it.
            unsafe { madvise(base, len, Advice::LinuxNoHugepage) }.map_err(|source| {
                RegionError::NoThp {
                    bytes: widen(len),
                    source: source.into(),
                }
            })?;
        }

        Ok(region)
    }

    /// The bytes mapped.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        widen(self.len)
    }

    #[must_use]
    pub fn page(&self) -> u64 {
        widen(self.page)
    }

    /// Writes one fill into every page the range reaches, faulting it in.
    pub fn touch(&mut self, from: u64, to: u64) -> Result<(), RegionError> {
        let (start, end) = self.bounds(from, to)?;

        for index in start / self.page..end.div_ceil(self.page) {
            self.write(index);
        }

        Ok(())
    }

    /// Drops every page from `from` up to the frontier, handing the memory straight back.
    ///
    /// A page that also holds bytes below `from` stays.
    /// Nothing past `frontier` may be in use, so the page it ends in goes too.
    pub fn release(&mut self, from: u64, frontier: u64) -> Result<(), RegionError> {
        let (start, end) = self.bounds(from, frontier)?;

        let first = start.next_multiple_of(self.page);
        let last = end.next_multiple_of(self.page);

        if first >= last {
            return Ok(());
        }

        let span = last - first;

        // SAFETY: `first..last` is a whole number of pages inside our own mapping.
        unsafe { madvise(self.at(first), span, Advice::LinuxDontNeed) }.map_err(|source| {
            RegionError::Release {
                offset: widen(first),
                bytes: widen(span),
                source: source.into(),
            }
        })
    }

    /// The bytes of the range that are resident right now.
    #[cfg(test)]
    fn resident(&self, from: u64, to: u64) -> Result<u64, RegionError> {
        let (start, end) = self.bounds(from, to)?;

        let first = start - start % self.page;
        let last = end.next_multiple_of(self.page);

        if first >= last {
            return Ok(0);
        }

        let span = last - first;
        let mut pages = vec![0_u8; span / self.page];

        // SAFETY: `first` is page-aligned inside our own mapping, `span` covers whole pages of
        // it, and `pages` holds one byte for each of them.
        let answer = unsafe { libc::mincore(self.at(first), span, pages.as_mut_ptr()) };

        if answer != 0 {
            return Err(RegionError::Residency {
                offset: widen(first),
                bytes: widen(span),
                source: io::Error::last_os_error(),
            });
        }

        let resident = pages.iter().filter(|page| *page & 1 == 1).count();

        Ok(widen(resident.saturating_mul(self.page)))
    }

    /// The byte offsets the range spans.
    fn bounds(&self, from: u64, to: u64) -> Result<(usize, usize), RegionError> {
        let outside = RegionError::Outside {
            from,
            to,
            len: widen(self.len),
        };

        if from > to {
            return Err(outside);
        }

        let (Ok(start), Ok(end)) = (usize::try_from(from), usize::try_from(to)) else {
            return Err(outside);
        };

        if end > self.len {
            return Err(outside);
        }

        Ok((start, end))
    }

    const fn at(&self, offset: usize) -> *mut c_void {
        // SAFETY: every caller has had `offset` checked against the mapping's length first.
        unsafe { self.base.cast::<u8>().add(offset) }.cast()
    }

    /// Faults one page in and leaves something in it that will not fold away.
    fn write(&self, index: usize) {
        let start = self.at(index * self.page).cast::<u64>();
        let seed = widen(index).wrapping_add(1).wrapping_mul(GOLDEN);

        match self.fill {
            // SAFETY: the page is mapped, writable, and aligned, so one word fits at its start.
            Fill::Index => unsafe { start.write_volatile(seed) },

            Fill::Random => {
                let mut state = seed;

                for word in 0..self.page / size_of::<u64>() {
                    state = xorshift(state);

                    // SAFETY: `word` stays inside the page, which is mapped, writable, and aligned.
                    unsafe { start.add(word).write_volatile(state) };
                }
            }
        }
    }

    /// Reads back a word of a page.
    #[cfg(test)]
    fn read(&self, index: usize, word: usize) -> u64 {
        let start = self.at(index * self.page).cast::<u64>();

        // SAFETY: `index` and `word` stay inside the mapping, which is readable and aligned.
        unsafe { start.add(word).read_volatile() }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: the mapping is ours, nothing borrows it, and this is the only place it is undone.
        if let Err(error) = unsafe { munmap(self.base, self.len) } {
            warn!(%error, "failed to unmap the arena");
        }
    }
}

#[derive(Debug, Error)]
pub enum RegionError {
    #[error("an arena of {bytes} bytes does not fit in this address space")]
    Vast { bytes: u64 },

    #[error("failed to map a {bytes} byte arena: {source}")]
    Map { bytes: u64, source: io::Error },

    #[error("failed to keep huge pages out of a {bytes} byte arena: {source}")]
    NoThp { bytes: u64, source: io::Error },

    #[error("failed to release {bytes} bytes at offset {offset} of the arena: {source}")]
    Release {
        offset: u64,
        bytes: u64,
        source: io::Error,
    },

    #[cfg(test)]
    #[error("failed to read what is resident in {bytes} bytes at offset {offset}: {source}")]
    Residency {
        offset: u64,
        bytes: u64,
        source: io::Error,
    },

    #[error("{from}..{to} is not inside the {len} byte arena")]
    Outside { from: u64, to: u64, len: u64 },
}

const fn xorshift(mut state: u64) -> u64 {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    state
}

fn widen(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;

    fn bytes(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive byte count")
    }

    fn arena(wanted: u64, fill: Fill) -> Region {
        Region::map(bytes(wanted), Commit::Reserve, HugePages::Allowed, fill)
            .expect("a mappable arena")
    }

    fn resident(region: &Region, from: u64, to: u64) -> u64 {
        region
            .resident(from, to)
            .expect("the arena answers mincore")
    }

    #[test]
    fn a_fill_is_named_by_what_it_puts_in_a_page() {
        assert_eq!("index".parse(), Ok(Fill::Index));
        assert_eq!("random".parse(), Ok(Fill::Random));
        assert_eq!(Fill::default(), Fill::Index);
        assert_eq!(
            "sequential"
                .parse::<Fill>()
                .expect_err("that is not a fill")
                .to_string(),
            "\"sequential\" is not a fill: expected index or random"
        );
    }

    #[test]
    fn an_arena_is_rounded_up_to_a_whole_page() {
        let region = arena(1, Fill::Index);

        assert_eq!(region.bytes(), region.page());
        assert_eq!(arena(4 * MIB, Fill::Index).bytes(), 4 * MIB);
    }

    #[test]
    fn an_untouched_arena_is_not_resident() {
        let region = arena(4 * MIB, Fill::Index);

        assert_eq!(resident(&region, 0, 4 * MIB), 0);
    }

    #[test]
    fn touching_a_range_makes_it_resident_and_leaves_the_rest_alone() {
        let mut region = arena(4 * MIB, Fill::Index);

        region.touch(0, MIB).expect("the frontier advances");

        assert_eq!(resident(&region, 0, MIB), MIB);
        assert_eq!(resident(&region, MIB, 4 * MIB), 0);

        region.touch(MIB, 2 * MIB).expect("the frontier advances");

        assert_eq!(resident(&region, 0, 2 * MIB), 2 * MIB);
        assert_eq!(resident(&region, 2 * MIB, 4 * MIB), 0);
    }

    #[test]
    fn releasing_a_range_takes_it_out_of_residency() {
        let mut region = arena(4 * MIB, Fill::Index);

        region.touch(0, 4 * MIB).expect("the frontier advances");
        assert_eq!(resident(&region, 0, 4 * MIB), 4 * MIB);

        region.release(2 * MIB, 4 * MIB).expect("the pages go back");

        assert_eq!(resident(&region, 0, 2 * MIB), 2 * MIB);
        assert_eq!(resident(&region, 2 * MIB, 4 * MIB), 0);
    }

    #[test]
    fn a_released_range_comes_back_when_it_is_touched_again() {
        let mut region = arena(4 * MIB, Fill::Index);

        region.touch(0, 4 * MIB).expect("the frontier advances");
        region.release(MIB, 4 * MIB).expect("the pages go back");

        assert_eq!(resident(&region, 0, 4 * MIB), MIB);

        region
            .touch(MIB, 3 * MIB)
            .expect("the frontier advances again");

        assert_eq!(resident(&region, 0, 3 * MIB), 3 * MIB);
        assert_eq!(resident(&region, 3 * MIB, 4 * MIB), 0);
    }

    #[test]
    fn a_release_keeps_the_page_that_still_holds_bytes_below_it() {
        let mut region = arena(4 * MIB, Fill::Index);
        let page = region.page();

        region.touch(0, page - 1).expect("the frontier advances");
        region.release(1, page - 1).expect("nothing to drop");

        assert_eq!(resident(&region, 0, 4 * MIB), page);
    }

    #[test]
    fn a_release_drops_the_partial_page_a_grow_faulted_in() {
        let mut region = arena(4 * MIB, Fill::Index);
        let page = region.page();

        region.touch(0, page + 1).expect("the frontier advances");

        region.release(1, page + 1).expect("the pages go back");

        assert_eq!(resident(&region, 0, 4 * MIB), page);
    }

    #[test]
    fn a_release_in_chunks_smaller_than_a_page_drops_every_page_it_passes() {
        let mut region = arena(4 * MIB, Fill::Index);
        let page = region.page();
        let floor = page / 2;
        let mut frontier = 3 * page;

        region.touch(0, frontier).expect("the frontier advances");

        while frontier > floor {
            let next = frontier.saturating_sub(1000).max(floor);

            region.release(next, frontier).expect("the pages go back");

            frontier = next;
        }

        assert_eq!(
            resident(&region, 0, 4 * MIB),
            page,
            "only the page the frontier ends in stays"
        );
    }

    #[test]
    fn a_range_that_ends_inside_a_page_still_faults_that_page_in() {
        let mut region = arena(4 * MIB, Fill::Index);
        let page = region.page();

        region.touch(0, 1).expect("the frontier advances");

        assert_eq!(resident(&region, 0, page), page);
        assert_eq!(resident(&region, page, 2 * page), 0);
    }

    #[test]
    fn a_range_outside_the_arena_is_refused() {
        let mut region = arena(4 * KIB, Fill::Index);
        let len = region.bytes();

        assert_eq!(
            region
                .touch(0, len + 1)
                .expect_err("the arena stops at its length")
                .to_string(),
            format!("0..{} is not inside the {len} byte arena", len + 1)
        );
        assert!(
            region.release(2 * KIB, KIB).is_err(),
            "backwards is not a range"
        );
        assert!(region.resident(0, u64::MAX).is_err());
    }

    #[test]
    fn an_index_fill_leaves_every_page_holding_something_of_its_own() {
        let mut region = arena(16 * KIB, Fill::Index);

        region.touch(0, region.bytes()).expect("every page");

        let words: Vec<u64> = (0..4).map(|index| region.read(index, 0)).collect();

        assert!(words.iter().all(|word| *word != 0), "a page folds to zero");
        assert_eq!(
            words.iter().collect::<std::collections::HashSet<_>>().len(),
            words.len(),
            "two pages hold the same thing"
        );
    }

    #[test]
    fn a_random_fill_leaves_no_two_words_alike() {
        let mut region = arena(16 * KIB, Fill::Random);

        region.touch(0, region.bytes()).expect("every page");

        let words: Vec<u64> = (0..64).map(|word| region.read(0, word)).collect();

        assert!(words.iter().all(|word| *word != 0), "a word folds to zero");
        assert_eq!(
            words.iter().collect::<std::collections::HashSet<_>>().len(),
            words.len(),
            "the page repeats itself"
        );
        assert_ne!(
            region.read(0, 0),
            region.read(1, 0),
            "two pages start the same"
        );
    }

    #[test]
    fn an_arena_can_be_mapped_without_a_commitment_or_huge_pages() {
        let mut region = Region::map(
            bytes(4 * MIB),
            Commit::NoReserve,
            HugePages::Excluded,
            Fill::Random,
        )
        .expect("a mappable arena");

        region.touch(0, MIB).expect("the frontier advances");

        assert_eq!(resident(&region, 0, MIB), MIB);
    }
}

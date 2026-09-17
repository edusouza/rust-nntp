//! A compact set of article numbers, in the `.newsrc` range syntax.

use std::fmt;

/// The set of articles read in one group, stored as sorted, disjoint, non-adjacent ranges.
///
/// # Why ranges
///
/// A busy group holds hundreds of thousands of articles and a reader that has followed it
/// for a year has read almost all of them. Storing one number per article would be both
/// large and, worse, unbounded in a way the user cannot see. Ranges make the common case —
/// "I have read everything up to here" — a single pair of numbers, which is why every
/// newsreader since the 1980s has used the same representation:
///
/// ```text
/// comp.lang.c: 1-4237,4240,4242-4250
/// ```
///
/// Keeping that exact syntax is deliberate: it is the one thing in this project that
/// another program is likely to read. `slrn`, `tin` and `nn` all understand it, so a user
/// migrating does not lose a year of reading history.
///
/// # Invariants
///
/// The internal ranges are always sorted, disjoint, and separated by at least one unread
/// number — `1-5` and `6-9` are stored as `1-9`. Every stored bound is at least 1, since
/// article numbering starts at 1 and `0` means "no article" in RFC 3977 §6. Nothing
/// outside this module can construct a value that breaks those invariants, so
/// [`Self::contains`] can binary-search and the [`fmt::Display`] impl can print the ranges
/// directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadSet {
    /// Inclusive `(low, high)` pairs. Sorted by `low`, disjoint, non-adjacent.
    ranges: Vec<(u64, u64)>,
}

impl ReadSet {
    /// An empty set: nothing in the group has been read.
    pub const fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    /// Whether nothing has been read.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// How many articles are marked read.
    ///
    /// This counts *numbers*, not articles the server still holds: numbers left by
    /// cancelled and expired articles are counted too, exactly as they are by the
    /// watermarks a group listing reports. Callers that show a number to the user should
    /// treat it the same way the group list treats its counts — as an upper bound.
    pub fn count(&self) -> u64 {
        self.ranges.iter().map(|&(low, high)| high - low + 1).sum()
    }

    /// How many ranges the set occupies. Useful for tests and for deciding whether a
    /// stored line has grown pathological.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// The highest article number marked read, or `None` for an empty set.
    pub fn highest(&self) -> Option<u64> {
        self.ranges.last().map(|&(_, high)| high)
    }

    /// Whether `number` is marked read.
    ///
    /// Article `0` is never read, because there is no article `0`.
    pub fn contains(&self, number: u64) -> bool {
        if number == 0 {
            return false;
        }

        // Ranges are sorted and disjoint, so the candidate is the last range starting at
        // or below `number`.
        match self.ranges.binary_search_by(|&(low, _)| low.cmp(&number)) {
            Ok(_) => true,
            Err(0) => false,
            Err(index) => self
                .ranges
                .get(index - 1)
                .is_some_and(|&(_, high)| number <= high),
        }
    }

    /// Marks a single article read.
    pub fn insert(&mut self, number: u64) {
        self.insert_range(number, number);
    }

    /// Marks an inclusive range of article numbers read.
    ///
    /// A reversed pair is normalised rather than ignored, and `0` bounds are clamped to 1,
    /// so a caller working from server watermarks cannot corrupt the set by passing them
    /// through unexamined. An all-zero range marks nothing.
    pub fn insert_range(&mut self, low: u64, high: u64) {
        let (low, high) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if high == 0 {
            return;
        }
        let low = low.max(1);

        // Everything that touches or abuts [low, high] is replaced by one merged range.
        // `low - 1` and `high + 1` make adjacency count as overlap, so `1-5` plus `6-9`
        // becomes `1-9` rather than two ranges that would grow without bound over a year
        // of reading one article at a time.
        let merge_from = low.saturating_sub(1);
        let merge_to = high.saturating_add(1);

        let first = self.ranges.partition_point(|&(_, h)| h < merge_from);
        let last = self.ranges.partition_point(|&(l, _)| l <= merge_to);

        if first >= last {
            self.ranges.insert(first, (low, high));
            return;
        }

        let mut merged_low = low;
        let mut merged_high = high;
        for &(l, h) in self.ranges.get(first..last).unwrap_or_default() {
            merged_low = merged_low.min(l);
            merged_high = merged_high.max(h);
        }

        self.ranges.splice(first..last, [(merged_low, merged_high)]);
    }

    /// Marks a single article unread.
    ///
    /// Splitting a range in two is the point: a user who marks one article unread in the
    /// middle of a read run expects exactly that article to come back, not the run to be
    /// forgotten.
    pub fn remove(&mut self, number: u64) {
        if number == 0 {
            return;
        }

        let index = match self.ranges.binary_search_by(|&(low, _)| low.cmp(&number)) {
            Ok(index) => index,
            Err(0) => return,
            Err(index) => index - 1,
        };

        let Some(&(low, high)) = self.ranges.get(index) else {
            return;
        };
        if number < low || number > high {
            return;
        }

        match (number == low, number == high) {
            (true, true) => {
                self.ranges.remove(index);
            }
            (true, false) => {
                self.ranges.splice(index..=index, [(low + 1, high)]);
            }
            (false, true) => {
                self.ranges.splice(index..=index, [(low, high - 1)]);
            }
            (false, false) => {
                self.ranges
                    .splice(index..=index, [(low, number - 1), (number + 1, high)]);
            }
        }
    }

    /// Marks every number from `low` to `high` unread, splitting ranges as needed.
    pub fn remove_range(&mut self, low: u64, high: u64) {
        let (low, high) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if high == 0 {
            return;
        }
        let low = low.max(1);

        let mut kept = Vec::with_capacity(self.ranges.len() + 1);
        for &(l, h) in &self.ranges {
            if h < low || l > high {
                kept.push((l, h));
                continue;
            }
            if l < low {
                kept.push((l, low - 1));
            }
            if h > high {
                kept.push((high + 1, h));
            }
        }
        self.ranges = kept;
    }

    /// How many numbers in `low..=high` are *not* marked read.
    ///
    /// This is the unread count the interface shows. It is derived from the group's
    /// watermarks, so like every other count built from watermarks it is an upper bound:
    /// numbers whose articles have been cancelled or expired are counted as unread because
    /// nothing short of asking the server can tell them apart from articles nobody has
    /// read yet.
    pub fn unread_in(&self, low: u64, high: u64) -> u64 {
        if high == 0 || high < low {
            return 0;
        }
        let low = low.max(1);
        let span = high - low + 1;
        span.saturating_sub(self.read_in(low, high))
    }

    /// How many numbers in `low..=high` are marked read.
    pub fn read_in(&self, low: u64, high: u64) -> u64 {
        if high == 0 || high < low {
            return 0;
        }
        let low = low.max(1);

        self.ranges
            .iter()
            .filter_map(|&(l, h)| {
                let start = l.max(low);
                let end = h.min(high);
                (start <= end).then(|| end - start + 1)
            })
            .sum()
    }

    /// The ranges, for a caller that wants to write them somewhere other than
    /// [`fmt::Display`].
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    /// Parses the `.newsrc` range list on the right of a `group:` line.
    ///
    /// Tolerant on purpose. This text comes from a file the user may have edited by hand,
    /// or from another newsreader, so a single unreadable piece must not cost the rest of
    /// a year's reading history: pieces that parse are kept, pieces that do not are
    /// returned alongside so the caller can log them. Overlapping, reversed, duplicated
    /// and out-of-order ranges are all accepted and normalised, because every one of them
    /// has appeared in a real `.newsrc`.
    ///
    /// `0` bounds are clamped to 1 rather than rejected: `0-5` in the wild means "the
    /// first five articles", written by something that did not know article numbering
    /// starts at 1.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut set = Self::new();
        let mut malformed = Vec::new();

        for piece in text.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                // A trailing comma, or `group: ` with nothing after it. Not worth
                // reporting: it is what an empty set looks like when written by hand.
                continue;
            }

            match parse_piece(piece) {
                Some((low, high)) => set.insert_range(low, high),
                None => malformed.push(piece.to_owned()),
            }
        }

        (set, malformed)
    }
}

/// Parses one comma-separated piece: `4240` or `4242-4250`.
fn parse_piece(piece: &str) -> Option<(u64, u64)> {
    match piece.split_once('-') {
        None => {
            let number = piece.trim().parse::<u64>().ok()?;
            Some((number, number))
        }
        Some((low, high)) => {
            let low = low.trim().parse::<u64>().ok()?;
            let high = high.trim();
            if high.is_empty() {
                // `4240-` is not valid `.newsrc`, and guessing "to the end of the group"
                // would mark articles read that the user has never seen. The rest of the
                // line is still usable, so this is reported rather than fatal.
                return None;
            }
            Some((low, high.parse::<u64>().ok()?))
        }
    }
}

impl fmt::Display for ReadSet {
    /// Writes the set in `.newsrc` syntax: `1-4237,4240,4242-4250`.
    ///
    /// A single-number range is written as one number rather than `4240-4240`, which is
    /// what other readers write and what a person editing the file expects to see.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        for (index, &(low, high)) in self.ranges.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            if low == high {
                out.push_str(&low.to_string());
            } else {
                out.push_str(&format!("{low}-{high}"));
            }
        }
        // `pad` rather than `write_str`, so `{:>20}` in a caller's format string works.
        // Learned the hard way in v0.1.0, where three `Display` impls ignored width.
        f.pad(&out)
    }
}

impl FromIterator<u64> for ReadSet {
    fn from_iter<I: IntoIterator<Item = u64>>(iter: I) -> Self {
        let mut set = Self::new();
        for number in iter {
            set.insert(number);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random generator, so the randomised tests below fail the
    /// same way twice. An LCG is more than enough to shuffle insertion order, and it
    /// avoids a dependency for the sake of one test module.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    fn set_of(numbers: &[u64]) -> ReadSet {
        numbers.iter().copied().collect()
    }

    #[test]
    fn an_empty_set_reads_and_prints_as_nothing() {
        let set = ReadSet::new();

        assert!(set.is_empty());
        assert_eq!(set.count(), 0);
        assert_eq!(set.highest(), None);
        assert_eq!(set.to_string(), "");
        assert!(!set.contains(1));
    }

    #[test]
    fn adjacent_insertions_merge_into_one_range() {
        let mut set = ReadSet::new();
        for number in 1..=1_000 {
            set.insert(number);
        }

        // The whole point of the representation: a year of reading one article at a time
        // is one range, not a thousand.
        assert_eq!(set.range_count(), 1);
        assert_eq!(set.to_string(), "1-1000");
        assert_eq!(set.count(), 1_000);
    }

    #[test]
    fn insertion_order_does_not_change_the_result() {
        let forwards = set_of(&[1, 2, 3, 7, 8, 20]);
        let backwards = set_of(&[20, 8, 7, 3, 2, 1]);
        let jumbled = set_of(&[7, 20, 1, 8, 3, 2]);

        assert_eq!(forwards, backwards);
        assert_eq!(forwards, jumbled);
        assert_eq!(forwards.to_string(), "1-3,7-8,20");
    }

    #[test]
    fn inserting_a_gap_filler_joins_the_ranges_on_both_sides() {
        let mut set = set_of(&[1, 2, 4, 5]);
        assert_eq!(set.to_string(), "1-2,4-5");

        set.insert(3);

        assert_eq!(set.to_string(), "1-5");
        assert_eq!(set.range_count(), 1);
    }

    #[test]
    fn an_inserted_range_swallows_everything_it_covers() {
        let mut set = set_of(&[2, 5, 9, 40]);
        set.insert_range(1, 10);

        assert_eq!(set.to_string(), "1-10,40");
    }

    #[test]
    fn a_reversed_range_is_normalised_rather_than_ignored() {
        let mut set = ReadSet::new();
        set.insert_range(10, 4);

        assert_eq!(set.to_string(), "4-10");
    }

    #[test]
    fn article_zero_does_not_exist() {
        let mut set = ReadSet::new();

        set.insert(0);
        assert!(set.is_empty(), "there is no article 0 (RFC 3977 §6)");

        // A caller passing watermarks straight through gets the sensible reading rather
        // than a corrupt set.
        set.insert_range(0, 5);
        assert_eq!(set.to_string(), "1-5");
        assert!(!set.contains(0));

        set.insert_range(0, 0);
        assert_eq!(set.to_string(), "1-5");
    }

    #[test]
    fn contains_finds_numbers_inside_and_outside_every_range() {
        let set = set_of(&[1, 2, 3, 10, 20, 21]);

        for read in [1, 2, 3, 10, 20, 21] {
            assert!(set.contains(read), "{read} should be read");
        }
        for unread in [0, 4, 9, 11, 19, 22, u64::MAX] {
            assert!(!set.contains(unread), "{unread} should be unread");
        }
    }

    #[test]
    fn removing_from_the_middle_splits_the_range() {
        let mut set = ReadSet::new();
        set.insert_range(1, 10);

        set.remove(5);

        assert_eq!(set.to_string(), "1-4,6-10");
        assert!(!set.contains(5));
        assert_eq!(set.count(), 9);
    }

    #[test]
    fn removing_an_endpoint_shrinks_the_range_instead_of_splitting_it() {
        let mut set = ReadSet::new();
        set.insert_range(1, 10);

        set.remove(1);
        assert_eq!(set.to_string(), "2-10");

        set.remove(10);
        assert_eq!(set.to_string(), "2-9");
    }

    #[test]
    fn removing_a_single_number_range_drops_it() {
        let mut set = set_of(&[5, 9]);

        set.remove(5);

        assert_eq!(set.to_string(), "9");
    }

    #[test]
    fn removing_something_unread_changes_nothing() {
        let before = set_of(&[1, 2, 3, 10]);
        let mut after = before.clone();

        after.remove(0);
        after.remove(4);
        after.remove(9);
        after.remove(u64::MAX);

        assert_eq!(before, after);
    }

    #[test]
    fn removing_a_range_cuts_every_range_it_crosses() {
        let mut set = ReadSet::new();
        set.insert_range(1, 10);
        set.insert_range(20, 30);
        set.insert_range(40, 50);

        set.remove_range(5, 45);

        assert_eq!(set.to_string(), "1-4,46-50");
    }

    #[test]
    fn unread_counts_come_from_the_watermarks_and_the_read_set() {
        let mut set = ReadSet::new();
        set.insert_range(1, 90);

        // 100 numbers in the group, 90 of them read.
        assert_eq!(set.unread_in(1, 100), 10);
        assert_eq!(set.read_in(1, 100), 90);

        // A window entirely inside the read run.
        assert_eq!(set.unread_in(10, 20), 0);

        // A window entirely outside it.
        assert_eq!(set.unread_in(200, 300), 101);

        // An empty or nonsensical window is zero rather than a panic or an underflow.
        assert_eq!(set.unread_in(0, 0), 0);
        assert_eq!(set.unread_in(50, 40), 0);
    }

    #[test]
    fn unread_counts_do_not_underflow_when_the_set_reaches_past_the_watermarks() {
        // A group that has been renumbered, or a `.newsrc` copied from a machine that saw
        // more articles than this server admits to. The answer must be 0, not a number
        // near u64::MAX.
        let mut set = ReadSet::new();
        set.insert_range(1, 10_000);

        assert_eq!(set.unread_in(1, 100), 0);
    }

    #[test]
    fn parses_the_newsrc_syntax() {
        let (set, malformed) = ReadSet::parse("1-4237,4240,4242-4250");

        assert!(malformed.is_empty());
        assert_eq!(set.to_string(), "1-4237,4240,4242-4250");
        assert!(set.contains(4240));
        assert!(!set.contains(4241));
    }

    #[test]
    fn parses_what_other_readers_and_hand_edits_actually_produce() {
        // Whitespace, out-of-order pieces, overlap, duplication, a trailing comma, and a
        // `0-` lower bound. All of these appear in real `.newsrc` files, and none of them
        // is a reason to throw away a reading history.
        let (set, malformed) = ReadSet::parse(" 20-30 , 1-10, 5-7 ,0-3, 20-25, 31,");

        assert!(malformed.is_empty(), "{malformed:?}");
        assert_eq!(set.to_string(), "1-10,20-31");
    }

    #[test]
    fn keeps_what_parses_and_reports_what_does_not() {
        let (set, malformed) = ReadSet::parse("1-10,oops,20,4240-,,30-40");

        // The readable pieces survive, which is the whole point.
        assert_eq!(set.to_string(), "1-10,20,30-40");
        // The unreadable ones are handed back for logging rather than silently dropped.
        assert_eq!(malformed, vec!["oops".to_owned(), "4240-".to_owned()]);
    }

    #[test]
    fn an_empty_line_parses_to_an_empty_set() {
        for text in ["", "   ", ",", ", ,"] {
            let (set, malformed) = ReadSet::parse(text);
            assert!(set.is_empty(), "{text:?} should parse to nothing");
            assert!(malformed.is_empty(), "{text:?} should not be reported");
        }
    }

    #[test]
    fn a_number_too_large_for_u64_is_reported_not_wrapped() {
        let (set, malformed) = ReadSet::parse("1-10,99999999999999999999999");

        assert_eq!(set.to_string(), "1-10");
        assert_eq!(malformed.len(), 1);
    }

    #[test]
    fn display_honours_field_width() {
        let set = set_of(&[1, 2, 3]);

        assert_eq!(format!("[{:>8}]", set), "[     1-3]");
        assert_eq!(format!("[{:<8}]", set), "[1-3     ]");
    }

    #[test]
    fn a_set_round_trips_through_parsing() {
        let mut set = ReadSet::new();
        set.insert_range(1, 4_237);
        set.insert(4_240);
        set.insert_range(4_242, 4_250);

        let (parsed, malformed) = ReadSet::parse(&set.to_string());

        assert!(malformed.is_empty());
        assert_eq!(parsed, set);
    }

    #[test]
    fn behaves_like_a_set_of_numbers_under_random_operations() {
        // The invariants — sorted, disjoint, non-adjacent — are easy to state and easy to
        // break in an edge case that hand-written tests miss. This checks every operation
        // against a plain sorted Vec oracle, which is obviously correct and obviously too
        // slow to ship.
        let mut set = ReadSet::new();
        let mut oracle: Vec<bool> = vec![false; 201];
        let mut rng = Lcg(0x5EED);

        for step in 0..4_000 {
            let low = rng.below(200) + 1;
            let high = (low + rng.below(20)).min(200);

            match rng.below(4) {
                0 => {
                    set.insert(low);
                    if let Some(slot) = oracle.get_mut(low as usize) {
                        *slot = true;
                    }
                }
                1 => {
                    set.insert_range(low, high);
                    for number in low..=high {
                        if let Some(slot) = oracle.get_mut(number as usize) {
                            *slot = true;
                        }
                    }
                }
                2 => {
                    set.remove(low);
                    if let Some(slot) = oracle.get_mut(low as usize) {
                        *slot = false;
                    }
                }
                _ => {
                    set.remove_range(low, high);
                    for number in low..=high {
                        if let Some(slot) = oracle.get_mut(number as usize) {
                            *slot = false;
                        }
                    }
                }
            }

            for number in 1..=200u64 {
                let expected = oracle.get(number as usize).copied().unwrap_or(false);
                assert_eq!(
                    set.contains(number),
                    expected,
                    "step {step}: article {number}"
                );
            }

            let expected_count = oracle.iter().filter(|&&read| read).count() as u64;
            assert_eq!(set.count(), expected_count, "step {step}: count");
            assert_eq!(set.read_in(1, 200), expected_count, "step {step}: read_in");
            assert_eq!(
                set.unread_in(1, 200),
                200 - expected_count,
                "step {step}: unread_in"
            );

            // The invariants themselves, not just the behaviour they enable.
            let mut previous: Option<(u64, u64)> = None;
            for &(l, h) in set.ranges() {
                assert!(l >= 1 && l <= h, "step {step}: bad range {l}-{h}");
                if let Some((_, previous_high)) = previous {
                    assert!(
                        l > previous_high + 1,
                        "step {step}: {l}-{h} touches or overlaps the range before it"
                    );
                }
                previous = Some((l, h));
            }

            // And the round trip, on every shape the loop produces.
            let (parsed, malformed) = ReadSet::parse(&set.to_string());
            assert!(malformed.is_empty(), "step {step}: {malformed:?}");
            assert_eq!(parsed, set, "step {step}: round trip");
        }
    }
}

// Which entries are selected, as a bitset.
//
// Indexed by *entry*, not by visible row, so a selection survives sorting and
// filtering without being rebuilt — sort a 100k directory with fifty files
// selected and the same fifty are still selected afterwards.
//
// A `HashSet<u32>` would do the same job at roughly 48 bytes per selected
// entry plus hashing on every draw. This is one bit per entry, 12 KB for a
// 100k-entry directory whether nothing or everything is selected, and
// membership is a shift and a mask.

#[derive(Debug, Default, Clone)]
pub struct Selection {
    bits: Vec<u64>,
    count: usize,
}

const BITS: usize = u64::BITS as usize;

impl Selection {
    pub fn clear(&mut self) {
        self.bits.clear();
        self.count = 0;
    }

    pub fn contains(&self, entry: usize) -> bool {
        self.bits
            .get(entry / BITS)
            .is_some_and(|word| word & (1 << (entry % BITS)) != 0)
    }

    pub fn set(&mut self, entry: usize, selected: bool) {
        let (word, mask) = (entry / BITS, 1u64 << (entry % BITS));
        if selected {
            if self.bits.len() <= word {
                self.bits.resize(word + 1, 0);
            }
            if self.bits[word] & mask == 0 {
                self.bits[word] |= mask;
                self.count += 1;
            }
        } else if let Some(slot) = self.bits.get_mut(word)
            && *slot & mask != 0
        {
            *slot &= !mask;
            self.count -= 1;
        }
    }

    pub fn toggle(&mut self, entry: usize) {
        self.set(entry, !self.contains(entry));
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Selected entries in index order. Walks whole words, so an empty or
    /// sparse selection skips 64 entries at a time.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.bits
            .iter()
            .enumerate()
            .filter(|(_, word)| **word != 0)
            .flat_map(|(index, word)| {
                let base = index * BITS;
                (0..BITS).filter_map(move |bit| (word & (1 << bit) != 0).then_some(base + bit))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_and_clearing_tracks_the_count() {
        let mut selection = Selection::default();
        assert!(selection.is_empty());

        selection.set(5, true);
        selection.set(200, true);
        assert_eq!(selection.len(), 2);
        assert!(selection.contains(5));
        assert!(selection.contains(200));
        assert!(!selection.contains(6));

        // Setting the same entry twice must not double-count.
        selection.set(5, true);
        assert_eq!(selection.len(), 2);

        selection.set(5, false);
        assert_eq!(selection.len(), 1);
        // Clearing something that was never set must not underflow.
        selection.set(9999, false);
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn toggle_flips() {
        let mut selection = Selection::default();
        selection.toggle(3);
        assert!(selection.contains(3));
        selection.toggle(3);
        assert!(!selection.contains(3));
        assert!(selection.is_empty());
    }

    #[test]
    fn iteration_is_in_index_order() {
        let mut selection = Selection::default();
        for entry in [300, 1, 64, 63, 65, 0] {
            selection.set(entry, true);
        }
        assert_eq!(
            selection.iter().collect::<Vec<_>>(),
            [0, 1, 63, 64, 65, 300]
        );
    }

    #[test]
    fn word_boundaries_do_not_leak() {
        let mut selection = Selection::default();
        selection.set(63, true);
        assert!(selection.contains(63));
        assert!(!selection.contains(64));
        assert!(!selection.contains(62));

        selection.set(64, true);
        assert_eq!(selection.iter().collect::<Vec<_>>(), [63, 64]);
    }

    #[test]
    fn clear_resets_everything() {
        let mut selection = Selection::default();
        for entry in 0..1000 {
            selection.set(entry, true);
        }
        assert_eq!(selection.len(), 1000);
        selection.clear();
        assert!(selection.is_empty());
        assert!(!selection.contains(0));
        assert_eq!(selection.iter().count(), 0);
    }

    #[test]
    fn a_hundred_thousand_entries_cost_one_bit_each() {
        let mut selection = Selection::default();
        selection.set(99_999, true);
        // 100k bits, rounded up to whole words.
        assert!(selection.bits.len() * 8 <= 13 * 1024);
        assert_eq!(selection.len(), 1);
    }
}

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use bone_app::{HistoryCursor, HistoryEntry, HistoryPage, RecentHistoryPage, SessionSeq};

use crate::layout::{ContentAnchor, TranscriptMetrics};

pub(crate) const HISTORY_CACHE_ITEMS: usize = 512;
// Reserve the other half of the 32 MiB cache budget for editor history and reader layout.
pub(crate) const HISTORY_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct TranscriptState {
    entries: VecDeque<HistoryEntry>,
    bytes: usize,
    copy_cache: RefCell<BTreeMap<SessionSeq, Arc<str>>>,
    cursor: SessionSeq,
    older_cursor: Option<HistoryCursor>,
    older_loading: bool,
    older_metrics: Option<Arc<TranscriptMetrics>>,
    history_loading: bool,
    recent_loading: bool,
    newer_missing: bool,
    scroll_from_tail: usize,
    read_anchor: Option<ContentAnchor>,
    metrics: Option<Arc<TranscriptMetrics>>,
}

impl TranscriptState {
    pub(crate) fn copy_text(
        &self,
        sequence: SessionSeq,
        make: impl FnOnce() -> Arc<str>,
    ) -> Arc<str> {
        if let Some(text) = self.copy_cache.borrow().get(&sequence) {
            return Arc::clone(text);
        }
        let text = make();
        let mut cache = self.copy_cache.borrow_mut();
        let used: usize = cache.values().map(|text| text.len() + 64).sum();
        if self.bytes + used + text.len() + 64 <= HISTORY_CACHE_BYTES {
            cache.insert(sequence, Arc::clone(&text));
        }
        text
    }

    pub(crate) fn entries(&self) -> impl DoubleEndedIterator<Item = &HistoryEntry> {
        self.entries.iter()
    }

    pub(crate) fn find(&self, sequence: SessionSeq) -> Option<&HistoryEntry> {
        self.entries.iter().find(|entry| entry.sequence == sequence)
    }

    pub(crate) const fn scroll_from_tail(&self) -> usize {
        self.scroll_from_tail
    }

    pub(crate) const fn read_anchor(&self) -> Option<ContentAnchor> {
        self.read_anchor
    }

    pub(crate) const fn reading(&self) -> bool {
        self.read_anchor.is_some() || self.scroll_from_tail > 0
    }

    pub(crate) fn at_tail(&self) -> bool {
        !self.reading()
    }

    pub(crate) fn open(&mut self, page: RecentHistoryPage) {
        self.replace(page);
        self.start_generation();
        self.newer_missing = false;
        self.scroll_from_tail = 0;
        self.read_anchor = None;
        self.metrics = None;
        self.trim_front();
    }

    pub(crate) fn start_generation(&mut self) {
        self.history_loading = false;
        self.older_loading = false;
        self.older_metrics = None;
        self.recent_loading = false;
    }

    /// Records an authoritative session change and returns the cursor for the
    /// next forward-history request, if one should be started.
    pub(crate) fn session_changed(&mut self, through: SessionSeq) -> Option<SessionSeq> {
        if through <= self.cursor || self.history_loading || self.newer_missing {
            return None;
        }
        if self.reading() || self.older_loading {
            self.newer_missing = true;
            return None;
        }
        self.history_loading = true;
        Some(self.cursor)
    }

    /// Applies a forward page and returns the cursor for another page when the
    /// durable history snapshot has not been exhausted.
    pub(crate) fn history_loaded(&mut self, page: HistoryPage) -> Option<SessionSeq> {
        self.history_loading = false;
        if self.reading() || self.older_loading {
            self.newer_missing = true;
            return None;
        }
        for entry in page.items {
            if self
                .entries
                .back()
                .is_none_or(|old| old.sequence < entry.sequence)
            {
                self.push_back(entry);
            }
        }
        self.cursor = self.cursor.max(page.next_cursor);
        self.trim_front();
        if page.has_more {
            self.history_loading = true;
            Some(self.cursor)
        } else {
            None
        }
    }

    pub(crate) fn older_history_loaded(&mut self, page: RecentHistoryPage) {
        self.older_loading = false;
        let metrics = self.older_metrics.take();
        self.older_cursor = page.older_cursor;
        for entry in page.items.into_iter().rev() {
            if self
                .entries
                .front()
                .is_none_or(|old| entry.sequence < old.sequence)
            {
                self.push_front(entry);
            }
        }
        let evicted = self.trim_back();
        if let Some(metrics) = metrics {
            let evicted_rows = evicted
                .into_iter()
                .map(|sequence| metrics.anchors.row_count(sequence))
                .sum::<usize>();
            self.scroll_from_tail = self.scroll_from_tail.saturating_sub(evicted_rows);
        }
    }

    pub(crate) fn recent_history_reloaded(&mut self, page: RecentHistoryPage) {
        self.recent_loading = false;
        if self.reading() || self.older_loading {
            self.newer_missing = true;
            return;
        }
        self.replace(page);
        self.scroll_from_tail = 0;
        self.read_anchor = None;
        self.metrics = None;
        self.newer_missing = false;
        self.trim_front();
    }

    pub(crate) fn history_failed(&mut self) {
        self.history_loading = false;
    }

    pub(crate) fn older_history_failed(&mut self) {
        self.older_loading = false;
        self.older_metrics = None;
    }

    pub(crate) fn recent_history_failed(&mut self) {
        self.recent_loading = false;
    }

    /// Moves to the live tail and reports whether it must be reloaded because
    /// newer entries were deliberately kept out of the reading window.
    pub(crate) fn follow_tail(&mut self) -> bool {
        self.read_anchor = None;
        self.scroll_from_tail = 0;
        if self.newer_missing && !self.recent_loading {
            self.recent_loading = true;
            true
        } else {
            false
        }
    }

    /// Scrolls toward older content and returns a stable pagination cursor when
    /// the retained viewport has reached its prefetch threshold.
    pub(crate) fn scroll_up(&mut self, amount: usize) -> Option<HistoryCursor> {
        let metrics = self.metrics.clone();
        if let Some(metrics) = &metrics {
            let start = self
                .read_anchor
                .and_then(|anchor| metrics.row_for_anchor(anchor))
                .unwrap_or(metrics.start_row)
                .saturating_sub(amount);
            self.read_anchor = metrics.anchor_at_start(start);
            self.scroll_from_tail = metrics
                .total_rows
                .saturating_sub(start + metrics.viewport_rows);
        } else {
            self.scroll_from_tail = self.scroll_from_tail.saturating_add(amount);
        }
        if metrics
            .as_ref()
            .is_some_and(|metrics| metrics.near_start(self.scroll_from_tail))
            && !self.older_loading
            && let Some(cursor) = self.older_cursor
        {
            self.older_loading = true;
            self.older_metrics = metrics;
            Some(cursor)
        } else {
            None
        }
    }

    /// Scrolls toward newer content and reports whether following the tail
    /// requires a recent-history reload.
    pub(crate) fn scroll_down(&mut self, amount: usize) -> bool {
        let target = self.metrics.as_ref().map(|metrics| {
            self.read_anchor
                .and_then(|anchor| metrics.row_for_anchor(anchor))
                .unwrap_or(metrics.start_row)
                .saturating_add(amount)
        });
        let reaches_tail = self
            .metrics
            .as_ref()
            .zip(target)
            .is_some_and(|(metrics, target)| target >= metrics.max_scroll());
        if reaches_tail || self.scroll_from_tail <= amount && self.metrics.is_none() {
            return self.follow_tail();
        }
        self.scroll_from_tail = self.scroll_from_tail.saturating_sub(amount);
        self.read_anchor = self
            .metrics
            .as_ref()
            .zip(target)
            .and_then(|(metrics, target)| metrics.anchor_at_start(target));
        false
    }

    pub(crate) fn pin_reading(&mut self) {
        if self.read_anchor.is_none()
            && let Some(metrics) = &self.metrics
            && let Some(anchor) = metrics.anchor_at_start(metrics.start_row)
        {
            self.begin_reading_at(anchor);
        }
    }

    pub(crate) fn begin_reading_at(&mut self, anchor: ContentAnchor) {
        self.read_anchor = Some(anchor);
    }

    pub(crate) fn retain_metrics(&mut self, metrics: Arc<TranscriptMetrics>, limit: usize) -> bool {
        if self.read_anchor.is_some() {
            self.scroll_from_tail = metrics.scroll_from_tail;
        }
        self.metrics = (metrics.allocated_bytes() <= limit).then_some(metrics);
        self.metrics.is_some()
    }

    pub(crate) fn allocated_bytes(&self) -> usize {
        self.bytes
            + self
                .copy_cache
                .borrow()
                .values()
                .map(|text| text.len() + 64)
                .sum::<usize>()
            + self
                .metrics
                .as_ref()
                .map_or(0, |metrics| metrics.allocated_bytes())
            + self
                .older_metrics
                .as_ref()
                .filter(|older| {
                    self.metrics
                        .as_ref()
                        .is_none_or(|current| !Arc::ptr_eq(current, older))
                })
                .map_or(0, |metrics| metrics.allocated_bytes())
    }

    pub(crate) fn has_metrics(&self) -> bool {
        self.metrics.is_some()
    }

    pub(crate) fn clear_layouts(&mut self) {
        self.copy_cache.get_mut().clear();
        self.metrics = None;
        self.older_metrics = None;
    }

    pub(crate) fn clear_older_layout(&mut self) {
        self.older_metrics = None;
    }

    pub(crate) fn clear_current_layout(&mut self) {
        self.metrics = None;
    }

    pub(crate) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Evicts one source entry for the global cross-session budget and returns
    /// its measured bytes. The visible anchor remains loaded whenever possible.
    pub(crate) fn evict_one(&mut self) -> Option<usize> {
        let preserve_front = self.entries.len() > 1
            && self.read_anchor.is_some_and(|anchor| {
                self.entries
                    .front()
                    .is_some_and(|entry| entry.sequence == anchor.sequence)
            });
        let entry = if preserve_front {
            self.newer_missing = true;
            self.entries.pop_back()
        } else {
            self.entries.pop_front()
        }?;
        let bytes = history_entry_bytes(&entry);
        self.bytes = self.bytes.saturating_sub(bytes);
        let copied = self
            .copy_cache
            .get_mut()
            .remove(&entry.sequence)
            .map_or(0, |text| text.len() + 64);
        Some(bytes + copied)
    }

    fn replace(&mut self, page: RecentHistoryPage) {
        self.entries.clear();
        self.copy_cache.get_mut().clear();
        self.bytes = 0;
        for entry in page.items {
            self.push_back(entry);
        }
        self.cursor = page.snapshot_through;
        self.older_cursor = page.older_cursor;
    }

    fn push_back(&mut self, entry: HistoryEntry) {
        self.bytes = self.bytes.saturating_add(history_entry_bytes(&entry));
        self.entries.push_back(entry);
        self.trim_copy_cache();
    }

    fn push_front(&mut self, entry: HistoryEntry) {
        self.bytes = self.bytes.saturating_add(history_entry_bytes(&entry));
        self.entries.push_front(entry);
        self.trim_copy_cache();
    }

    fn trim_copy_cache(&mut self) {
        let cache = self.copy_cache.get_mut();
        let mut bytes = self.bytes + cache.values().map(|text| text.len() + 64).sum::<usize>();
        while bytes > HISTORY_CACHE_BYTES {
            let Some((_, text)) = cache.pop_first() else {
                break;
            };
            bytes -= text.len() + 64;
        }
    }

    fn trim_front(&mut self) {
        while self.entries.len() > HISTORY_CACHE_ITEMS || self.bytes > HISTORY_CACHE_BYTES {
            let preserve_front = self.entries.len() > 1
                && self.read_anchor.is_some_and(|anchor| {
                    self.entries
                        .front()
                        .is_some_and(|entry| entry.sequence == anchor.sequence)
                });
            let Some(entry) = (if preserve_front {
                self.newer_missing = true;
                self.entries.pop_back()
            } else {
                self.entries.pop_front()
            }) else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(history_entry_bytes(&entry));
            self.copy_cache.get_mut().remove(&entry.sequence);
        }
    }

    fn trim_back(&mut self) -> Vec<SessionSeq> {
        let mut evicted = Vec::new();
        while self.entries.len() > HISTORY_CACHE_ITEMS || self.bytes > HISTORY_CACHE_BYTES {
            let preserve_back = self.entries.len() > 1
                && self.read_anchor.is_some_and(|anchor| {
                    self.entries
                        .back()
                        .is_some_and(|entry| entry.sequence == anchor.sequence)
                });
            let Some(entry) = (if preserve_back {
                self.entries.pop_front()
            } else {
                self.entries.pop_back()
            }) else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(history_entry_bytes(&entry));
            self.copy_cache.get_mut().remove(&entry.sequence);
            evicted.push(entry.sequence);
            self.newer_missing = true;
        }
        evicted
    }
}

fn history_entry_bytes(entry: &HistoryEntry) -> usize {
    serde_json::to_vec(entry).map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::AnchorPart;
    use bone_app::{InputId, SessionEvent};

    fn entry(sequence: u64) -> HistoryEntry {
        HistoryEntry {
            sequence: SessionSeq(sequence),
            occurred_at: sequence as i64,
            event: SessionEvent::InputCancelled {
                input: InputId(sequence),
            },
        }
    }

    fn recent(sequences: impl IntoIterator<Item = u64>) -> RecentHistoryPage {
        let items = sequences.into_iter().map(entry).collect::<Vec<_>>();
        let snapshot_through = items.last().map_or(SessionSeq(0), |entry| entry.sequence);
        RecentHistoryPage {
            items,
            older_cursor: None,
            snapshot_through,
        }
    }

    fn anchor(sequence: u64) -> ContentAnchor {
        ContentAnchor {
            sequence: SessionSeq(sequence),
            byte: 0,
            part: AnchorPart::Text,
        }
    }

    #[test]
    fn forward_pages_deduplicate_entries_and_never_move_the_cursor_backwards() {
        let mut transcript = TranscriptState::default();
        assert_eq!(
            transcript.session_changed(SessionSeq(10)),
            Some(SessionSeq(0))
        );
        assert_eq!(transcript.session_changed(SessionSeq(10)), None);

        assert_eq!(
            transcript.history_loaded(HistoryPage {
                items: vec![entry(5)],
                next_cursor: SessionSeq(5),
                has_more: false,
            }),
            None
        );
        transcript.history_loaded(HistoryPage {
            items: vec![entry(5), entry(4)],
            next_cursor: SessionSeq(3),
            has_more: false,
        });

        assert_eq!(
            transcript
                .entries()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![SessionSeq(5)]
        );
        assert_eq!(
            transcript.session_changed(SessionSeq(6)),
            Some(SessionSeq(5))
        );
    }

    #[test]
    fn starting_a_generation_releases_every_previous_loading_latch() {
        let mut transcript = TranscriptState {
            history_loading: true,
            older_loading: true,
            recent_loading: true,
            newer_missing: true,
            older_metrics: Some(Arc::new(TranscriptMetrics::default())),
            metrics: Some(Arc::new(TranscriptMetrics::default())),
            read_anchor: Some(anchor(9)),
            scroll_from_tail: 12,
            ..TranscriptState::default()
        };

        transcript.start_generation();

        assert!(!transcript.history_loading);
        assert!(!transcript.older_loading);
        assert!(!transcript.recent_loading);
        assert!(transcript.older_metrics.is_none());
        assert!(transcript.newer_missing);
        assert!(transcript.metrics.is_some());
        assert_eq!(transcript.read_anchor, Some(anchor(9)));
        assert_eq!(transcript.scroll_from_tail, 12);

        transcript.open(recent([1, 2]));
        assert!(!transcript.newer_missing);
        assert!(transcript.metrics.is_none());
        assert!(transcript.read_anchor.is_none());
        assert_eq!(transcript.scroll_from_tail, 0);
    }

    #[test]
    fn recent_reload_does_not_override_a_new_scroll_without_layout_metrics() {
        let mut transcript = TranscriptState::default();
        transcript.open(recent([1]));
        transcript.newer_missing = true;
        assert!(transcript.follow_tail());
        assert_eq!(transcript.scroll_up(1), None);

        transcript.recent_history_reloaded(recent([8, 9]));

        assert_eq!(
            transcript
                .entries()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![SessionSeq(1)]
        );
        assert_eq!(transcript.scroll_from_tail, 1);
        assert!(transcript.newer_missing);
        assert!(!transcript.recent_loading);
    }

    #[test]
    fn older_page_trim_compensates_for_the_evicted_visual_rows() {
        let mut transcript = TranscriptState::default();
        transcript.open(recent(1_000..1_512));
        let anchors = (1_000..1_512)
            .flat_map(|sequence| [anchor(sequence), anchor(sequence), anchor(sequence)])
            .collect::<Vec<_>>();
        transcript.scroll_from_tail = 32 * 3 + 7;
        transcript.older_loading = true;
        transcript.older_metrics = Some(Arc::new(TranscriptMetrics {
            total_rows: anchors.len(),
            viewport_rows: 24,
            anchors: anchors.into(),
            start_row: 0,
            scroll_from_tail: transcript.scroll_from_tail,
        }));

        transcript.older_history_loaded(recent(1..=32));

        assert_eq!(transcript.entries.len(), HISTORY_CACHE_ITEMS);
        assert_eq!(transcript.scroll_from_tail, 7);
        assert!(transcript.newer_missing);
        assert!(!transcript.older_loading);
        assert!(transcript.older_metrics.is_none());
    }

    #[test]
    fn failures_release_only_the_corresponding_loading_latch() {
        let mut transcript = TranscriptState {
            history_loading: true,
            older_loading: true,
            recent_loading: true,
            older_metrics: Some(Arc::new(TranscriptMetrics::default())),
            ..TranscriptState::default()
        };

        transcript.history_failed();
        assert!(!transcript.history_loading);
        assert!(transcript.older_loading);
        assert!(transcript.recent_loading);

        transcript.older_history_failed();
        assert!(!transcript.older_loading);
        assert!(transcript.older_metrics.is_none());
        assert!(transcript.recent_loading);

        transcript.recent_history_failed();
        assert!(!transcript.recent_loading);
    }
    #[test]
    fn copy_projection_is_reused_accounted_and_invalidated_with_history() {
        let mut transcript = TranscriptState::default();
        transcript.open(recent([1, 2]));
        let baseline = transcript.allocated_bytes();
        let original = transcript.copy_text(SessionSeq(1), || Arc::from("original"));
        let again = transcript.copy_text(SessionSeq(1), || panic!("recomputed cached projection"));
        assert!(Arc::ptr_eq(&original, &again));
        assert_eq!(
            transcript.allocated_bytes(),
            baseline + "original".len() + 64
        );
        let before = transcript.allocated_bytes();
        let removed = transcript.evict_one().unwrap();
        assert_eq!(transcript.allocated_bytes(), before - removed);
        assert!(transcript.copy_cache.borrow().is_empty());
        transcript.open(recent([1]));
        assert_eq!(
            &*transcript.copy_text(SessionSeq(1), || Arc::from("updated")),
            "updated"
        );
    }
}

//! Bounded presentation text cache shared by all Hermes conversation tabs.
//!
//! History and execution remain native Hermes data. Keys address immutable
//! source fragments; this cache may evict anything and read it through again.
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

pub const TEXT_CACHE_BYTES: usize = 64 * 1024 * 1024;
pub const BLOCK_BYTES: usize = 4096;
pub const MAX_VISIBLE_BLOCKS: usize = 120;
pub const UI_INTERVAL: std::time::Duration = std::time::Duration::from_millis(34);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockKey {
    pub session: Arc<str>,
    pub message: u64,
    pub offset: u64,
}

struct CachedBlock {
    text: Arc<str>,
    touched: u64,
}

pub struct TextCache {
    capacity: usize,
    bytes: usize,
    clock: u64,
    blocks: HashMap<BlockKey, CachedBlock>,
    lru: BTreeSet<(u64, BlockKey)>,
}

impl Default for TextCache {
    fn default() -> Self {
        Self::new(TEXT_CACHE_BYTES)
    }
}

impl TextCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            bytes: 0,
            clock: 0,
            blocks: HashMap::new(),
            lru: BTreeSet::new(),
        }
    }

    pub fn get(&mut self, key: &BlockKey) -> Option<Arc<str>> {
        let block = self.blocks.get_mut(key)?;
        self.lru.remove(&(block.touched, key.clone()));
        self.clock += 1;
        block.touched = self.clock;
        self.lru.insert((block.touched, key.clone()));
        Some(block.text.clone())
    }

    pub fn insert(&mut self, key: BlockKey, text: Arc<str>) {
        if let Some(old) = self.blocks.remove(&key) {
            self.bytes -= old.text.len();
            self.lru.remove(&(old.touched, key.clone()));
        }
        // Never make a single oversized block exempt from the global budget.
        // The caller can present a bounded source fragment without caching it.
        if text.len() > self.capacity {
            return;
        }
        while self.bytes + text.len() > self.capacity {
            let Some((_, victim)) = self.lru.pop_first() else {
                break;
            };
            if let Some(old) = self.blocks.remove(&victim) {
                self.bytes -= old.text.len();
            }
        }
        self.clock += 1;
        self.bytes += text.len();
        self.lru.insert((self.clock, key.clone()));
        self.blocks.insert(
            key,
            CachedBlock {
                text,
                touched: self.clock,
            },
        );
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Slice at a UTF-8 boundary even for a huge paragraph with no whitespace.
/// The consumed byte count is also the stable native source cursor.
pub fn fragment(text: &str, max_bytes: usize) -> (&str, usize) {
    let mut end = text.len().min(max_bytes);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end)
}

/// Only the unfinished bounded tail is mutable. Completed blocks never need
/// re-parsing when more tokens arrive. Callbacks persist the frozen blocks off
/// the UI thread, so streams cannot grow an unbounded in-memory transcript.
#[derive(Default)]
pub struct StreamingTail {
    text: String,
    offset: u64,
}

impl StreamingTail {
    pub fn push(&mut self, mut delta: &str, mut freeze: impl FnMut(u64, &str)) {
        while !delta.is_empty() {
            let available = BLOCK_BYTES - self.text.len();
            let (part, consumed) = fragment(delta, available);
            if consumed == 0 {
                freeze(self.offset, &self.text);
                self.offset += self.text.len() as u64;
                self.text.clear();
                continue;
            }
            self.text.push_str(part);
            delta = &delta[consumed..];
            if self.text.len() == BLOCK_BYTES {
                freeze(self.offset, &self.text);
                self.offset += self.text.len() as u64;
                self.text.clear();
            }
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn finish(&mut self, mut freeze: impl FnMut(u64, &str)) {
        if !self.text.is_empty() {
            freeze(self.offset, &self.text);
            self.offset += self.text.len() as u64;
            self.text.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(session: &str, message: u64) -> BlockKey {
        BlockKey {
            session: session.into(),
            message,
            offset: 0,
        }
    }

    #[test]
    fn global_budget_evicts_cold_tabs_and_replacement_is_accounted() {
        let mut cache = TextCache::new(12);
        cache.insert(key("background", 1), "aaaaaa".into());
        cache.insert(key("selected", 1), "bbbbbb".into());
        cache.get(&key("selected", 1));
        cache.insert(key("new", 1), "cccccc".into());
        assert!(cache.get(&key("background", 1)).is_none());
        assert!(cache.get(&key("selected", 1)).is_some());
        cache.insert(key("selected", 1), "bb".into());
        assert_eq!(cache.bytes(), 8);
        cache.insert(key("oversized", 1), "x".repeat(100).into());
        assert_eq!(cache.bytes(), 8);
    }

    #[test]
    fn giant_unicode_paragraph_is_lossless_with_bounded_mutable_tail() {
        let input = "👩🏽‍💻漢字é".repeat(100_000);
        let mut stream = StreamingTail::default();
        let mut rebuilt = String::new();
        let mut expected_offset = 0;
        let mut freeze = |offset, text: &str| {
            assert_eq!(offset, expected_offset);
            assert!(text.len() <= BLOCK_BYTES);
            expected_offset += text.len() as u64;
            rebuilt.push_str(text);
        };
        stream.push(&input, &mut freeze);
        assert!(stream.text().len() <= BLOCK_BYTES);
        stream.finish(&mut freeze);
        assert_eq!(input, rebuilt);
    }

    #[test]
    fn split_code_and_incomplete_markdown_keep_every_byte() {
        let input = format!("```rust\n{}\n```\n[unfinished](", "x".repeat(50_000));
        let mut tail = StreamingTail::default();
        let mut result = String::new();
        for part in input.as_bytes().chunks(37) {
            tail.push(std::str::from_utf8(part).unwrap(), |_, block| {
                result.push_str(block)
            });
        }
        tail.finish(|_, block| result.push_str(block));
        assert_eq!(result, input);
    }
}

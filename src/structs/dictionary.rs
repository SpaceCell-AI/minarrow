// Copyright 2025 Peter Garfield Bower
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # **Dictionary Module** - *Append-only Shared String Dictionary for Categorical Arrays*
//!
//! Gated at the module level under the `shared_dict` feature. Without
//! the feature `CategoricalArray<T>` carries `unique_values: Vec64<String>`
//! directly and there is no dictionary type.
//!
//! ## Shape
//! `Dictionary<T>` is a handle around `Arc<DictionaryInner<T>>`. The
//! inner holds two pieces:
//!
//! - `values: AppendOnlyVec<String>` - the code-indexed value array.
//!   Lock-free multi-reader and multi-writer; elements live at stable
//!   heap addresses for the vector's lifetime, so `&String` borrows
//!   survive concurrent appends.
//! - `index: ShardedIndex<T>` - the reverse string-to-code lookup,
//!   sharded across 64 `Mutex<HashMap>` slots. Distinct strings hash to
//!   distinct shards and never contend; same-shard collisions briefly
//!   serialise on that shard's mutex.
//!
//! Cloning the dictionary is an Arc bump; every clone observes the same
//! updates with no fork on write.
//!
//! ## Intern flow
//! 1. Hash the candidate string, pick a shard, take its mutex.
//! 2. If already present, return existing code.
//! 3. Otherwise, atomically reserve a slot in `AppendOnlyVec` via a
//!    CAS-bounded push capped at `T::MAX + 1` (so narrow widths like
//!    `u8` honour their 256-entry cap exactly, even under concurrent
//!    writes from many threads).
//! 4. Write the string into the slot, publish, insert the code into the
//!    shard's hashmap, release the mutex.
//!
//! ## Append-only invariant
//! Once a string is interned and assigned a code, the mapping is
//! permanent. Entries are never reordered, replaced, or removed.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use ::vec64::Vec64;

use crate::structs::append_only_vec::AppendOnlyVec;
use crate::traits::type_unions::Integer;

#[cfg(feature = "fast_hash")]
type IndexMap<T> = ahash::AHashMap<String, T>;
#[cfg(not(feature = "fast_hash"))]
type IndexMap<T> = std::collections::HashMap<String, T>;

/// Number of shards in the reverse string-to-code index. 64 means
/// distinct novel strings spread cheaply across the index without
/// serialising on a single mutex.
const N_INDEX_SHARDS: usize = 64;

/// Errors arising from mutating a `Dictionary<T>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictionaryError {
    /// The new cardinality would exceed the capacity of the index type `T`
    /// (e.g. 256 entries for `u8`). The dictionary is left unchanged and
    /// no slot is reserved.
    Overflow,
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => write!(
                f,
                "dictionary cardinality would exceed the capacity of the index type"
            ),
        }
    }
}

impl std::error::Error for DictionaryError {}

/// Sharded reverse index. Each shard holds its slice of strings under a
/// `Mutex`, hashed-out via the high bits of `DefaultHasher`. Distinct
/// novel strings hit distinct shards under uniform hashing and never
/// serialise against each other.
struct ShardedIndex<T: Integer> {
    shards: Box<[Mutex<IndexMap<T>>; N_INDEX_SHARDS]>,
}

impl<T: Integer> Default for ShardedIndex<T> {
    fn default() -> Self {
        let shards: [Mutex<IndexMap<T>>; N_INDEX_SHARDS] =
            std::array::from_fn(|_| Mutex::new(IndexMap::default()));
        Self {
            shards: Box::new(shards),
        }
    }
}

impl<T: Integer> ShardedIndex<T> {
    #[inline]
    fn shard_for(s: &str) -> usize {
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        (h.finish() as usize) % N_INDEX_SHARDS
    }

    fn lookup(&self, s: &str) -> Option<T> {
        let shard = &self.shards[Self::shard_for(s)];
        shard.lock().expect("dictionary shard poisoned").get(s).copied()
    }
}

/// Backing storage for a `Dictionary<T>`. Held behind the dictionary's
/// `Arc`. Reads of the value array via `values()` are lock-free; intern
/// briefly takes a per-shard mutex for the check-and-insert step.
pub struct DictionaryInner<T: Integer> {
    /// Code-indexed string array. Lock-free reads; lock-free multi-writer
    /// `push_bounded` under the categorical's width cap.
    pub values: AppendOnlyVec<String>,
    /// Reverse lookup, sharded.
    index: ShardedIndex<T>,
}

impl<T: Integer> Default for DictionaryInner<T> {
    fn default() -> Self {
        // Pre-allocate the value array to the type's natural cap.
        // The cap is fixed for the dictionary's lifetime (the
        // `AppendOnlyVec` never reallocates); `push` returns `None`
        // when the cap is reached, which `intern` surfaces as
        // `DictionaryError::Overflow`.
        Self {
            values: AppendOnlyVec::with_capacity(max_cap::<T>()),
            index: ShardedIndex::default(),
        }
    }
}

/// Append-only string dictionary backing `CategoricalArray<T>` under
/// `shared_dict`. Cloning a `Dictionary` is an Arc bump on the underlying
/// inner; both clones observe the same updates immediately. `intern`
/// takes `&self` and is concurrent-safe across many writers without
/// global serialisation.
#[derive(Clone)]
pub struct Dictionary<T: Integer> {
    inner: Arc<DictionaryInner<T>>,
}

impl<T: Integer> Default for Dictionary<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(DictionaryInner::default()),
        }
    }
}

impl<T: Integer> Dictionary<T> {
    /// Empty dictionary in a fresh sharing group.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a dictionary from an ordered list of values. Input is
    /// preserved verbatim and the reverse index is built from it. Panics
    /// if the input length exceeds the capacity of `T`.
    pub fn from_values(values: impl Into<Vec64<String>>) -> Self {
        let values: Vec64<String> = values.into();
        let d = Self::default();
        let cap = d.inner.values.capacity();
        for (i, s) in values.into_iter().enumerate() {
            assert!(
                i < cap,
                "Dictionary input length {} exceeds capacity of index type {}",
                i + 1,
                std::any::type_name::<T>()
            );
            // Inline the intern logic since we know each push is a novel
            // value (de-duped in the construction loop below).
            let shard = &d.inner.index.shards[ShardedIndex::<T>::shard_for(&s)];
            let mut g = shard.lock().expect("dictionary shard poisoned");
            if g.get(&s).is_some() {
                continue;
            }
            let idx = d
                .inner
                .values
                .push(s.clone())
                .expect("checked cap above");
            g.insert(s, T::from_usize(idx));
        }
        d
    }

    /// Borrow the published prefix of the value array as a slice.
    /// Lock-free; indexing returns `&String`, which derefs to `&str`
    /// for the lifetime of `&self`. Concurrent pushes may publish
    /// additional slots after this call returns; those are visible to
    /// subsequent invocations.
    #[inline]
    pub fn values(&self) -> &[String] {
        self.inner.values.as_slice()
    }

    /// Number of entries currently visible to readers.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.values.count()
    }

    /// True if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Code for `s` if interned at the moment of the call, otherwise `None`.
    #[inline]
    pub fn lookup(&self, s: &str) -> Option<T> {
        self.inner.index.lookup(s)
    }

    /// Interns `s` atomically. Concurrent-safe via `&self`; distinct
    /// novel strings spread across the index's shards and never block
    /// each other. Returns `Err(DictionaryError::Overflow)` if the new
    /// cardinality would exceed the capacity of `T`, leaving the
    /// dictionary unchanged and no slot reserved.
    pub fn intern(&self, value: &str) -> Result<T, DictionaryError> {
        let shard = &self.inner.index.shards[ShardedIndex::<T>::shard_for(value)];
        let mut g = shard.lock().expect("dictionary shard poisoned");
        if let Some(&code) = g.get(value) {
            return Ok(code);
        }
        let idx = self
            .inner
            .values
            .push(value.to_owned())
            .ok_or(DictionaryError::Overflow)?;
        let code = T::from_usize(idx);
        g.insert(value.to_owned(), code);
        Ok(code)
    }

    /// Absorb `cat` into this sharing group. Interns every entry of
    /// `cat`'s current dictionary into `self`, remaps `cat`'s data buffer
    /// to the resulting codes if any code shifted, and rebinds `cat`'s
    /// dictionary to a clone of `self` so the chunk joins the sharing
    /// group.
    pub fn absorb(&self, cat: &mut crate::CategoricalArray<T>) {
        let incoming = &cat.dictionary.inner.values;
        let mut shifted = false;
        let mut remap: Vec<T> = Vec::with_capacity(incoming.count());
        for (incoming_code, s) in incoming.iter() {
            let Ok(new_code) = self.intern(s) else { return };
            if new_code.to_usize() != incoming_code {
                shifted = true;
            }
            remap.push(new_code);
        }
        if shifted {
            for code in cat.data.iter_mut() {
                *code = remap[code.to_usize()];
            }
        }
        cat.dictionary = self.clone();
    }

    /// True if `self`'s values at the moment of the call are a prefix of
    /// `other`'s. Every code valid against `self` decodes to the same
    /// string in `other`.
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        let a = &self.inner.values;
        let b = &other.inner.values;
        if a.count() > b.count() {
            return false;
        }
        for (i, s) in a.iter() {
            match b.get(i) {
                Some(t) if t.as_str() == s.as_str() => {}
                _ => return false,
            }
        }
        true
    }

    /// True if `self` and `other` are clones of the same dictionary, so
    /// updates through one are visible through the other.
    #[inline]
    pub fn shares_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Detach this dictionary from its sharing group. Future mutations are
    /// independent of the original group; the data is preserved.
    ///
    /// User-only; no internal call sites. Use when you want to keep the
    /// current entries but stop receiving updates from the group.
    pub fn detach_to_owned(&mut self) {
        let fresh = Dictionary::<T>::default();
        for (_, s) in self.inner.values.iter() {
            let shard = &fresh.inner.index.shards[ShardedIndex::<T>::shard_for(s)];
            let mut g = shard.lock().expect("dictionary shard poisoned");
            let idx = fresh
                .inner
                .values
                .push(s.clone())
                .expect("source dict already within cap");
            g.insert(s.clone(), T::from_usize(idx));
        }
        self.inner = fresh.inner;
    }
}

impl<T: Integer> PartialEq for Dictionary<T> {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return true;
        }
        let a = &self.inner.values;
        let b = &other.inner.values;
        if a.count() != b.count() {
            return false;
        }
        for (i, s) in a.iter() {
            match b.get(i) {
                Some(t) if t.as_str() == s.as_str() => {}
                _ => return false,
            }
        }
        true
    }
}

impl<T: Integer> std::fmt::Debug for Dictionary<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("len", &self.inner.values.count())
            .finish()
    }
}

impl<T: Integer> From<Vec64<String>> for Dictionary<T> {
    fn from(values: Vec64<String>) -> Self {
        Self::from_values(values)
    }
}

impl<T: Integer> From<Vec<String>> for Dictionary<T> {
    fn from(values: Vec<String>) -> Self {
        Self::from_values(Vec64::from(values))
    }
}

impl<T: Integer, S: Into<String>> FromIterator<S> for Dictionary<T> {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let owned: Vec64<String> = Vec64::from(iter.into_iter().map(Into::into).collect::<Vec<_>>());
        Self::from_values(owned)
    }
}

/// Practical entry cap used by `Dictionary::default()`. Narrow widths
/// receive their natural cap; `u32`/`u64` receive a soft cap of
/// `1 << 20` (1 048 576) entries because preallocating their natural
/// cap would reserve ~100 GB+ of virtual address space per dictionary
/// (allocators reject the request even with overcommit). Users who
/// genuinely need a larger cap can pass it via `with_capacity`.
///
/// `u8 -> 256`, `u16 -> 65 536`, `u32/u64 -> 1 048 576`.
const DEFAULT_WIDE_CAP: usize = 1 << 20;

#[inline]
fn max_cap<T: Integer>() -> usize {
    use num_traits::Bounded;
    let type_max = T::max_value().to_usize().saturating_add(1);
    if type_max > DEFAULT_WIDE_CAP {
        DEFAULT_WIDE_CAP
    } else {
        type_max
    }
}

// =============================================================================
// CategoryDispatch - width-erased holder used by parent containers.
// =============================================================================

/// Width-erased `Dictionary` so a parent container (`SuperTable`,
/// `SuperArray`) can hold one entry per categorical column without being
/// generic over each column's width. Each variant carries the column's
/// typed `Dictionary`; cloning a `CategoryDispatch` is an Arc bump on the
/// underlying inner.
#[derive(Debug, Clone)]
pub enum CategoryDispatch {
    #[cfg(feature = "default_categorical_8")]
    U8(Dictionary<u8>),
    #[cfg(feature = "extended_categorical")]
    U16(Dictionary<u16>),
    #[cfg(any(not(feature = "default_categorical_8"), feature = "extended_categorical"))]
    U32(Dictionary<u32>),
    #[cfg(feature = "extended_categorical")]
    U64(Dictionary<u64>),
}

impl CategoryDispatch {
    /// Install a fresh dispatch from a batch's categorical column by
    /// cloning the chunk's dictionary (Arc bump). Subsequent absorbs from
    /// other chunks intern through this dispatch's dictionary and rebind
    /// those chunks to share the same Arc.
    ///
    /// Returns `None` if the array is not categorical at any enabled width.
    pub fn install_from(array: &mut crate::Array) -> Option<Self> {
        use crate::{Array, TextArray};
        match array {
            #[cfg(any(not(feature = "default_categorical_8"), feature = "extended_categorical"))]
            Array::TextArray(TextArray::Categorical32(arc)) => {
                let cat = Arc::make_mut(arc);
                Some(CategoryDispatch::U32(cat.dictionary.clone()))
            }
            #[cfg(feature = "default_categorical_8")]
            Array::TextArray(TextArray::Categorical8(arc)) => {
                let cat = Arc::make_mut(arc);
                Some(CategoryDispatch::U8(cat.dictionary.clone()))
            }
            #[cfg(feature = "extended_categorical")]
            Array::TextArray(TextArray::Categorical16(arc)) => {
                let cat = Arc::make_mut(arc);
                Some(CategoryDispatch::U16(cat.dictionary.clone()))
            }
            #[cfg(feature = "extended_categorical")]
            Array::TextArray(TextArray::Categorical64(arc)) => {
                let cat = Arc::make_mut(arc);
                Some(CategoryDispatch::U64(cat.dictionary.clone()))
            }
            _ => None,
        }
    }

    /// Dispatches on the dispatch variant and the array's categorical
    /// width, calling `Dictionary::absorb` on the matching pair. Width
    /// mismatch is a schema error upstream and is treated as a no-op here.
    pub fn absorb(&self, array: &mut crate::Array) {
        use crate::{Array, TextArray};
        match (self, array) {
            #[cfg(any(not(feature = "default_categorical_8"), feature = "extended_categorical"))]
            (CategoryDispatch::U32(d), Array::TextArray(TextArray::Categorical32(arc))) => {
                d.absorb(Arc::make_mut(arc));
            }
            #[cfg(feature = "default_categorical_8")]
            (CategoryDispatch::U8(d), Array::TextArray(TextArray::Categorical8(arc))) => {
                d.absorb(Arc::make_mut(arc));
            }
            #[cfg(feature = "extended_categorical")]
            (CategoryDispatch::U16(d), Array::TextArray(TextArray::Categorical16(arc))) => {
                d.absorb(Arc::make_mut(arc));
            }
            #[cfg(feature = "extended_categorical")]
            (CategoryDispatch::U64(d), Array::TextArray(TextArray::Categorical64(arc))) => {
                d.absorb(Arc::make_mut(arc));
            }
            _ => {}
        }
    }

    /// Number of entries currently in the dispatch's dictionary.
    pub fn len(&self) -> usize {
        match self {
            #[cfg(feature = "default_categorical_8")]
            CategoryDispatch::U8(d) => d.len(),
            #[cfg(feature = "extended_categorical")]
            CategoryDispatch::U16(d) => d.len(),
            #[cfg(any(not(feature = "default_categorical_8"), feature = "extended_categorical"))]
            CategoryDispatch::U32(d) => d.len(),
            #[cfg(feature = "extended_categorical")]
            CategoryDispatch::U64(d) => d.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dictionary_starts_empty() {
        let d: Dictionary<u32> = Dictionary::new();
        assert_eq!(d.len(), 0);
        assert!(d.is_empty());
        assert_eq!(d.lookup("anything"), None);
    }

    #[test]
    fn intern_assigns_dense_sequential_codes() {
        let d: Dictionary<u32> = Dictionary::new();
        assert_eq!(d.intern("a"), Ok(0));
        assert_eq!(d.intern("b"), Ok(1));
        assert_eq!(d.intern("c"), Ok(2));
        assert_eq!(d.intern("a"), Ok(0));
        assert_eq!(d.len(), 3);
        let values: Vec<&str> = d.values().iter().map(|s| s.as_str()).collect();
        assert_eq!(values, vec!["a", "b", "c"]);
    }

    #[test]
    fn clones_share_state() {
        let d: Dictionary<u32> = Dictionary::new();
        let cloned = d.clone();
        assert!(d.shares_with(&cloned));
        // Update through one is visible through the other.
        assert_eq!(d.intern("a"), Ok(0));
        let values: Vec<&str> = cloned.values().iter().map(|s| s.as_str()).collect();
        assert_eq!(values, vec!["a"]);
    }

    #[test]
    fn detach_breaks_sharing() {
        let a: Dictionary<u32> = Dictionary::new();
        let _ = a.intern("x").unwrap();
        let mut b = a.clone();
        b.detach_to_owned();
        assert_eq!(a.values().get(0).map(|s| s.as_str()), Some("x"));
        assert_eq!(b.values().get(0).map(|s| s.as_str()), Some("x"));
        assert!(!a.shares_with(&b));
        let _ = b.intern("y").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn is_prefix_of_recognises_prefix() {
        let a: Dictionary<u32> = Dictionary::from_iter(["x", "y"]);
        let b: Dictionary<u32> = Dictionary::from_iter(["x", "y", "z"]);
        assert!(a.is_prefix_of(&b));
        assert!(!b.is_prefix_of(&a));
        let c: Dictionary<u32> = Dictionary::from_iter(["x", "z"]);
        assert!(!a.is_prefix_of(&c));
    }

    /// Narrow-width cap is honoured exactly: 256 successful interns,
    /// the 257th returns Overflow with no leaked slot.
    #[test]
    fn intern_returns_overflow_at_u8_cap() {
        let d: Dictionary<u8> = Dictionary::new();
        for i in 0..256u32 {
            d.intern(&format!("v{i}")).unwrap();
        }
        assert_eq!(d.intern("overflow"), Err(DictionaryError::Overflow));
        assert_eq!(d.len(), 256);
    }

    /// Many threads concurrently interning into the same u8 dictionary
    /// must collectively succeed exactly 256 times and never exceed the
    /// cap. No leaked slots, no double-assigned codes.
    #[test]
    fn concurrent_intern_under_u8_cap_no_leaks() {
        use std::sync::Arc;
        use std::thread;

        let d: Arc<Dictionary<u8>> = Arc::new(Dictionary::new());
        let mut handles = Vec::new();
        for t in 0..16 {
            let d = Arc::clone(&d);
            handles.push(thread::spawn(move || {
                let mut successes = 0u32;
                let mut overflows = 0u32;
                for i in 0..100 {
                    let s = format!("t{t}_v{i}");
                    match d.intern(&s) {
                        Ok(_) => successes += 1,
                        Err(DictionaryError::Overflow) => overflows += 1,
                    }
                }
                (successes, overflows)
            }));
        }
        let (mut total_succ, mut total_ovf) = (0u32, 0u32);
        for h in handles {
            let (s, o) = h.join().unwrap();
            total_succ += s;
            total_ovf += o;
        }
        assert_eq!(d.len(), 256);
        assert_eq!(total_succ, 256);
        assert_eq!(total_ovf, 16 * 100 - 256);
    }

    /// Many threads interning distinct novel strings into a wide dict;
    /// every string ends up represented exactly once.
    #[test]
    fn concurrent_intern_distinct_strings_no_duplicates() {
        use std::sync::Arc;
        use std::thread;

        let d: Arc<Dictionary<u32>> = Arc::new(Dictionary::new());
        let mut handles = Vec::new();
        for t in 0..8 {
            let d = Arc::clone(&d);
            handles.push(thread::spawn(move || {
                for i in 0..500 {
                    let _ = d.intern(&format!("t{t}_v{i}")).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(d.len(), 8 * 500);
    }
}

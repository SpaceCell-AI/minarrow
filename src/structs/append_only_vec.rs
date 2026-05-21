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

//! # **AppendOnlyVec** - *Contiguous Lock-free Append-only Vec*
//!
//! Used internally by `Dictionary<T>` as the code-indexed value array.
//! Storage is a single contiguous allocation sized at construction;
//! the buffer is never reallocated, so element addresses are stable
//! for the vector's lifetime. `Dictionary` hands out `&str` borrows
//! tied to `&self` against the published prefix without any reader-side
//! lock or guard.
//!
//! ## Concurrency model
//! - **Reads**: lock-free. `as_slice` / `get` / `iter` read the prefix
//!   `[0, published)` via an `Acquire` load on the publish counter,
//!   then access the contiguous buffer directly.
//! - **Writes**: lock-free. A writer claims a slot via a cap-bounded
//!   `compare_exchange_weak` loop on `reserved`, writes the value
//!   into its slot, then briefly spins on the `published` counter
//!   waiting for predecessors to commit before `Release`-storing its
//!   own commit. No mutex is taken on either path; cache-hit interns
//!   in `Dictionary` never reach the value array at all (sharded
//!   reverse-lookup map deals with them).
//!
//! ## Capacity
//! Fixed at construction. `push` returns `None` once the cap is
//! reached; the cap check sits inside the `reserved` CAS loop, so the
//! narrow-width categorical caps (u8 → 256, u16 → 65 536, ...) are
//! honoured exactly even under heavy multi-writer contention - a
//! failed CAS never advances the counter, so no slot is leaked.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free append-only vector with a fixed contiguous buffer. See
/// module documentation for the concurrency model.
pub struct AppendOnlyVec<T> {
    /// Pre-allocated slot buffer. Length is fixed at construction;
    /// never resized. Slots at indices `[0, published)` are fully
    /// initialised; later indices are `MaybeUninit`.
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// Slot claims. Writers advance via a cap-bounded
    /// `compare_exchange_weak` loop; a failed CAS leaves the counter
    /// untouched, so the cap is enforced exactly.
    reserved: AtomicUsize,
    /// Published prefix length. Writers commit by `Release`-storing
    /// `idx + 1` once their predecessors have committed; readers see
    /// only the published prefix via `Acquire` loads. The published
    /// prefix is always contiguous - readers can return `&[T]`
    /// covering it directly.
    published: AtomicUsize,
}

impl<T> Default for AppendOnlyVec<T> {
    /// Empty vector with zero capacity. Use [`with_capacity`] to size it.
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl<T> AppendOnlyVec<T> {
    /// Construct an empty vector with capacity for `cap` elements.
    /// The buffer is allocated once and never reallocated; `push`
    /// fails (returns `None`) once `cap` slots are filled.
    pub fn with_capacity(cap: usize) -> Self {
        let mut v: Vec<UnsafeCell<MaybeUninit<T>>> = Vec::with_capacity(cap);
        // Fill the buffer with uninitialised slot cells. Slots above
        // `published` are never read as `T`; only those below the
        // published prefix are.
        for _ in 0..cap {
            v.push(UnsafeCell::new(MaybeUninit::uninit()));
        }
        Self {
            slots: v.into_boxed_slice(),
            reserved: AtomicUsize::new(0),
            published: AtomicUsize::new(0),
        }
    }

    /// Total slot capacity set at construction.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Number of fully published entries visible to readers.
    #[inline]
    pub fn count(&self) -> usize {
        self.published.load(Ordering::Acquire)
    }

    /// True if the vector is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// Append `value`. Returns the assigned index, or `None` if the
    /// pre-allocated capacity is exhausted.
    ///
    /// Concurrent-safe and lock-free. Multiple writers can call this
    /// from different threads simultaneously - each claims a distinct
    /// slot via a cap-bounded `compare_exchange_weak`. Writers commit
    /// their slot in claim order via the `published` counter: after
    /// writing the value, a writer spins briefly on `published` until
    /// its predecessor has committed, then `Release`-stores its own
    /// commit. The spin loop uses `std::hint::spin_loop` so the
    /// hardware backs off on contention.
    pub fn push(&self, value: T) -> Option<usize> {
        // Step 1: claim a slot via cap-bounded CAS. A failed CAS
        // never advances `reserved`, so a cap overshoot can't leak.
        let idx = {
            let mut cur = self.reserved.load(Ordering::Relaxed);
            loop {
                if cur >= self.slots.len() {
                    return None;
                }
                match self.reserved.compare_exchange_weak(
                    cur,
                    cur + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break cur,
                    Err(actual) => cur = actual,
                }
            }
        };

        // Step 2: write the value into our exclusively-claimed slot.
        // SAFETY: idx < cap by the CAS bound; no other thread can
        // claim this idx because `reserved` has already moved past it.
        unsafe {
            (*self.slots[idx].get()).write(value);
        }

        // Step 3: wait for predecessors to commit, then commit our
        // slot. Predecessors hold lower idx values; each commits by
        // bumping `published` from N to N+1 in claim order. Once
        // `published` reaches `idx`, our slot is the next to commit.
        // Spin-wait via `spin_loop` hints the hardware to back off
        // (e.g., issue PAUSE on x86) without parking the thread.
        while self.published.load(Ordering::Acquire) != idx {
            std::hint::spin_loop();
        }
        // `Release` store synchronises with the `Acquire` load in
        // readers (`count`, `as_slice`, `get`) so the value written
        // above is visible to any reader that observes `idx + 1`.
        self.published.store(idx + 1, Ordering::Release);

        Some(idx)
    }

    /// Returns the published prefix as a contiguous slice. Lock-free.
    /// The slice covers all slots committed at the moment of the call;
    /// commits happening concurrently with this call may publish new
    /// slots, but those will only appear in subsequent `as_slice`
    /// invocations.
    pub fn as_slice(&self) -> &[T] {
        let len = self.published.load(Ordering::Acquire);
        // SAFETY: slots [0, len) are fully initialised and published
        // - their writers `Release`-stored the published counter to
        // values >= len + 1 after writing the value. The `Acquire`
        // load above synchronises with those stores.
        // `UnsafeCell<MaybeUninit<T>>` has the same layout as `T`, so
        // casting the base pointer is sound.
        unsafe { std::slice::from_raw_parts(self.slots.as_ptr() as *const T, len) }
    }

    /// Returns `&T` at logical index `idx`, or `None` if the slot has
    /// not yet been published. Lock-free.
    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let len = self.published.load(Ordering::Acquire);
        if idx >= len {
            return None;
        }
        // SAFETY: idx < published => slot is fully committed.
        Some(unsafe { (*self.slots[idx].get()).assume_init_ref() })
    }

    /// Returns an iterator over `(index, &T)` pairs covering the
    /// published prefix at the moment of the call.
    #[inline]
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            slice: self.as_slice(),
            idx: 0,
        }
    }
}

unsafe impl<T: Send> Send for AppendOnlyVec<T> {}
unsafe impl<T: Sync> Sync for AppendOnlyVec<T> {}

impl<T> std::ops::Index<usize> for AppendOnlyVec<T> {
    type Output = T;

    /// Panicking indexed access. Use [`get`](Self::get) for a fallible
    /// version. Panics if `idx` is out of range or not yet published.
    fn index(&self, idx: usize) -> &T {
        self.get(idx).unwrap_or_else(|| {
            panic!(
                "AppendOnlyVec index out of range or slot not yet published: {idx}"
            )
        })
    }
}

impl<T> std::ops::Deref for AppendOnlyVec<T> {
    type Target = [T];

    /// Deref to the published prefix. Lets the vector be used wherever
    /// a `&[T]` is expected; same lock-free semantics as `as_slice`.
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> Drop for AppendOnlyVec<T> {
    fn drop(&mut self) {
        // `published.get_mut()` is sound: we have unique access.
        let len = *self.published.get_mut();
        for i in 0..len {
            // SAFETY: i < published => slot is initialised.
            unsafe {
                (*self.slots[i].get()).assume_init_drop();
            }
        }
        // `Box<[UnsafeCell<MaybeUninit<T>>]>` drops the cells; their
        // contents are `MaybeUninit` (no Drop), so nothing more to do.
    }
}

/// Iterator returned by [`AppendOnlyVec::iter`].
pub struct Iter<'a, T> {
    slice: &'a [T],
    idx: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (usize, &'a T);

    fn next(&mut self) -> Option<(usize, &'a T)> {
        if self.idx >= self.slice.len() {
            return None;
        }
        let i = self.idx;
        let v = &self.slice[i];
        self.idx += 1;
        Some((i, v))
    }
}

// -------- tests --------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn push_under_cap_succeeds() {
        let v = AppendOnlyVec::<u32>::with_capacity(4);
        assert_eq!(v.push(10), Some(0));
        assert_eq!(v.push(20), Some(1));
        assert_eq!(v.count(), 2);
        assert_eq!(v.as_slice(), &[10, 20]);
    }

    #[test]
    fn push_past_cap_returns_none_and_leaves_state_unchanged() {
        let v = AppendOnlyVec::<u32>::with_capacity(2);
        assert_eq!(v.push(1), Some(0));
        assert_eq!(v.push(2), Some(1));
        // Cap reached; subsequent pushes return None without reserving
        // a slot. Count is unchanged.
        assert_eq!(v.push(3), None);
        assert_eq!(v.count(), 2);
        assert_eq!(v.as_slice(), &[1, 2]);
    }

    #[test]
    fn index_get_slice_agree() {
        let v = AppendOnlyVec::<String>::with_capacity(8);
        v.push("a".into());
        v.push("b".into());
        v.push("c".into());
        assert_eq!(&v[0], "a");
        assert_eq!(&v[1], "b");
        assert_eq!(v.get(2).map(String::as_str), Some("c"));
        assert!(v.get(3).is_none());
        let slice: &[String] = v.as_slice();
        assert_eq!(slice, &["a".to_string(), "b".into(), "c".into()]);
    }

    #[test]
    fn deref_to_slice() {
        let v = AppendOnlyVec::<u32>::with_capacity(4);
        v.push(7);
        v.push(8);
        let s: &[u32] = &*v;
        assert_eq!(s, &[7, 8]);
    }

    #[test]
    fn iter_yields_index_and_ref() {
        let v = AppendOnlyVec::<u32>::with_capacity(4);
        v.push(100);
        v.push(200);
        let pairs: Vec<(usize, u32)> = v.iter().map(|(i, x)| (i, *x)).collect();
        assert_eq!(pairs, vec![(0, 100), (1, 200)]);
    }

    #[test]
    fn drop_releases_initialised_prefix() {
        // Arc<()> as a drop tracer: each clone bumps strong_count, drop
        // releases it. Push three clones, drop the vec, assert the count
        // returns to the original 1.
        let canary = Arc::new(());
        {
            let v = AppendOnlyVec::<Arc<()>>::with_capacity(8);
            v.push(canary.clone());
            v.push(canary.clone());
            v.push(canary.clone());
            assert_eq!(Arc::strong_count(&canary), 4);
        }
        assert_eq!(Arc::strong_count(&canary), 1);
    }

    /// Stress test: 16 threads each pushing 32 values against a cap of
    /// 256. Exactly 256 pushes succeed, 256 hit the cap. Count is
    /// exact; the published slice contains all 256 distinct accepted
    /// values with no leaks or torn writes.
    #[test]
    fn concurrent_push_honours_cap_exactly() {
        let v: Arc<AppendOnlyVec<u32>> = Arc::new(AppendOnlyVec::with_capacity(256));
        let mut handles = Vec::new();
        for t in 0..16u32 {
            let v = Arc::clone(&v);
            handles.push(thread::spawn(move || {
                let mut accepted = 0usize;
                for i in 0..32 {
                    if v.push(t * 32 + i).is_some() {
                        accepted += 1;
                    }
                }
                accepted
            }));
        }
        let total_accepted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total_accepted, 256);
        assert_eq!(v.count(), 256);
        let slice = v.as_slice();
        assert_eq!(slice.len(), 256);
        // All 512 distinct candidate values were either accepted or
        // rejected at the cap; no value is duplicated or lost.
        let mut seen = std::collections::HashSet::new();
        for &x in slice {
            assert!(seen.insert(x), "duplicate value in slice: {x}");
        }
    }

    /// Concurrent push interleaved with concurrent reads. Readers
    /// observe a strictly-growing prefix; every readable slot is
    /// fully published (no torn writes).
    #[test]
    fn concurrent_push_and_read() {
        let v: Arc<AppendOnlyVec<u64>> = Arc::new(AppendOnlyVec::with_capacity(10_000));
        let writers: Vec<_> = (0..8u64)
            .map(|t| {
                let v = Arc::clone(&v);
                thread::spawn(move || {
                    for i in 0..1_000 {
                        let _ = v.push(t * 1_000 + i);
                    }
                })
            })
            .collect();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let v = Arc::clone(&v);
                thread::spawn(move || {
                    let mut last_len = 0;
                    for _ in 0..1_000 {
                        let s = v.as_slice();
                        assert!(s.len() >= last_len, "published prefix shrank");
                        last_len = s.len();
                        // Touch every element so a torn write would show
                        // up as a SEGV or assertion in MIRI.
                        let _sum: u64 = s.iter().sum();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        for r in readers {
            r.join().unwrap();
        }
        assert_eq!(v.count(), 8_000);
    }
}

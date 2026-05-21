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

//! # **AppendOnlyVec** - *Lock-free Append-only Vec with Stable Element Addresses*
//!
//! Used internally by `Dictionary<T>` as the code-indexed value array.
//! Elements live in bucketed heap allocations that are never moved or
//! freed during the vector's lifetime, so `&T` references survive
//! subsequent pushes - which lets `Dictionary` hand out `&str` borrows
//! tied to `&self` without holding any read lock.
//!
//! ## Concurrency model
//! - **Multi-reader concurrent**: `get` / `iter` are lock-free; readers
//!   see only fully-published slots via an `Acquire` load on each slot's
//!   `init` flag.
//! - **Multi-writer concurrent**: `push` and `push_bounded` are `&self`
//!   and safe to call from multiple threads simultaneously. Slot
//!   assignment is via a CAS-or-`fetch_add` on a global `reserved`
//!   counter; each writer claims a distinct slot and publishes it
//!   independently. No writer blocks another.
//!
//! ## Allocation strategy
//! Buckets at doubling sizes starting at 16. Bucket `i` holds `16 << i`
//! entries; 28 buckets cover ~4 billion entries (greater than `u32::MAX`,
//! the widest categorical index Minarrow targets).

use std::alloc::{Layout, alloc, dealloc};
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// First bucket holds 16 entries.
const FIRST_BUCKET: usize = 16;
/// 28 buckets cover 16 * (2^28 - 1) entries, > u32::MAX.
const N_BUCKETS: usize = 28;

/// Per-slot storage. `init` distinguishes fully-published slots (writer
/// has finished writing) from reserved-but-not-yet-published ones.
struct Slot<T> {
    init: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

/// Lock-free append-only Vec with stable element addresses. See module docs.
pub struct AppendOnlyVec<T> {
    /// Bucket pointers. Each entry is either null (bucket not allocated)
    /// or points to an array of `bucket_size(i)` `Slot<T>` entries with
    /// `init=false` and `value` uninitialised at allocation time.
    buckets: [AtomicPtr<Slot<T>>; N_BUCKETS],
    /// Number of slots claimed by writers. May exceed the number of
    /// fully-published slots if writes are in flight; readers must
    /// consult each slot's `init` flag before dereferencing.
    reserved: AtomicUsize,
}

impl<T> Default for AppendOnlyVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AppendOnlyVec<T> {
    /// Constructs an empty vector with no allocations.
    pub fn new() -> Self {
        let buckets: [AtomicPtr<Slot<T>>; N_BUCKETS] =
            std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut()));
        Self {
            buckets,
            reserved: AtomicUsize::new(0),
        }
    }

    /// Constructs an empty vector and pre-allocates buckets sufficient to
    /// hold at least `cap` entries without further allocation.
    pub fn with_capacity(cap: usize) -> Self {
        let v = Self::new();
        let mut remaining = cap;
        let mut bucket_idx = 0;
        while remaining > 0 && bucket_idx < N_BUCKETS {
            // SAFETY: bucket_idx in range; no concurrent access during construction.
            let _ = unsafe { v.alloc_bucket(bucket_idx) };
            let size = Self::bucket_size(bucket_idx);
            remaining = remaining.saturating_sub(size);
            bucket_idx += 1;
        }
        v
    }

    /// Number of slots currently reserved. Some may not yet be published
    /// to readers (writers may still be in flight); `iter` and `get`
    /// respect each slot's `init` flag separately.
    #[inline]
    pub fn count(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }

    /// Returns `&T` at logical index `idx`, or `None` if the slot is out
    /// of range or has not yet been published by its writer. Lock-free.
    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx >= self.reserved.load(Ordering::Acquire) {
            return None;
        }
        let (bucket_idx, slot) = Self::locate(idx);
        let bucket_ptr = self.buckets[bucket_idx].load(Ordering::Acquire);
        if bucket_ptr.is_null() {
            return None;
        }
        // SAFETY: bucket_ptr is non-null and was Release-stored by the
        // writer that allocated this bucket. The slot pointer arithmetic
        // is within the allocated bucket.
        unsafe {
            let slot_ptr = bucket_ptr.add(slot);
            if !(*slot_ptr).init.load(Ordering::Acquire) {
                return None;
            }
            // SAFETY: init==true implies the writer has called write() on
            // the cell and stored init with Release. The Acquire load
            // synchronises with that, so the value is fully published.
            Some((*(*slot_ptr).value.get()).assume_init_ref())
        }
    }

    /// Append `value` and return its index. Multi-writer concurrent.
    /// Unbounded: assumes `usize::MAX` is unreachable. For width-bounded
    /// pushes (categorical u8/u16 etc.), use [`push_bounded`].
    pub fn push(&self, value: T) -> usize {
        let idx = self.reserved.fetch_add(1, Ordering::Relaxed);
        // SAFETY: idx is exclusively ours by the atomic claim.
        unsafe { self.write_at(idx, value) };
        idx
    }

    /// Append `value` only if the total reserved count would remain
    /// strictly below `max_cap`. Returns `None` if the cap is exhausted -
    /// no slot is reserved, no value is dropped, no state leaks.
    ///
    /// Uses a `compare_exchange_weak` loop on `reserved` so capacity is
    /// enforced exactly under any concurrent contention.
    pub fn push_bounded(&self, value: T, max_cap: usize) -> Option<usize> {
        let mut current = self.reserved.load(Ordering::Relaxed);
        let idx = loop {
            if current >= max_cap {
                return None;
            }
            match self.reserved.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break current,
                Err(actual) => current = actual,
            }
        };
        // SAFETY: idx is exclusively ours by the successful CAS.
        unsafe { self.write_at(idx, value) };
        Some(idx)
    }

    /// Lock-free iterator yielding `(index, &T)` pairs over the
    /// fully-published prefix of the vector. Stops at the first slot
    /// that is reserved but not yet published; that slot and any later
    /// ones are invisible to this iterator (subsequent `iter()` calls
    /// may see them).
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            vec: self,
            idx: 0,
            upper: self.reserved.load(Ordering::Acquire),
        }
    }

    // ----- internal -----

    /// Bucket size schedule: bucket `i` holds `FIRST_BUCKET << i` entries.
    #[inline]
    fn bucket_size(i: usize) -> usize {
        FIRST_BUCKET << i
    }

    /// Logical index → `(bucket, slot_within_bucket)`.
    #[inline]
    fn locate(logical: usize) -> (usize, usize) {
        // Sum of bucket sizes 0..i is FIRST_BUCKET * (2^i - 1).
        // For logical `n`, find i such that
        //   FIRST_BUCKET * (2^i - 1) <= n < FIRST_BUCKET * (2^(i+1) - 1)
        // i.e. (n / FIRST_BUCKET) + 1 falls in [2^i, 2^(i+1)).
        let shifted = (logical / FIRST_BUCKET) + 1;
        let bucket = (usize::BITS - 1 - shifted.leading_zeros()) as usize;
        let bucket_start = FIRST_BUCKET * ((1usize << bucket) - 1);
        let slot = logical - bucket_start;
        (bucket, slot)
    }

    /// Allocate bucket `bucket_idx` if not already allocated and return
    /// the (potentially racing) winner's pointer.
    fn ensure_bucket(&self, bucket_idx: usize) -> *mut Slot<T> {
        let existing = self.buckets[bucket_idx].load(Ordering::Acquire);
        if !existing.is_null() {
            return existing;
        }
        // SAFETY: bucket_idx in range; CAS resolves the multi-writer race.
        unsafe { self.alloc_bucket(bucket_idx) }
    }

    /// Allocate bucket `bucket_idx`. Multi-writer safe: if another writer
    /// races and installs first, we free our allocation and return their
    /// pointer.
    unsafe fn alloc_bucket(&self, bucket_idx: usize) -> *mut Slot<T> {
        let size = Self::bucket_size(bucket_idx);
        let layout = Layout::array::<Slot<T>>(size)
            .expect("bucket layout fits in usize");
        // SAFETY: layout.size() > 0 because size >= 1.
        let raw = unsafe { alloc(layout) } as *mut Slot<T>;
        if raw.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // Initialise each slot's `init` flag and `value` cell. We must
        // not skip the init=false write: ptr::write past uninitialised
        // memory for AtomicBool is required so that subsequent atomic
        // operations see a defined value.
        for i in 0..size {
            // SAFETY: raw.add(i) is in-bounds within the freshly allocated
            // bucket.
            unsafe {
                ptr::write(
                    raw.add(i),
                    Slot {
                        init: AtomicBool::new(false),
                        value: UnsafeCell::new(MaybeUninit::uninit()),
                    },
                );
            }
        }
        // Install via CAS. If we lose the race, free our buffer and use
        // the winner's.
        match self.buckets[bucket_idx].compare_exchange(
            ptr::null_mut(),
            raw,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => raw,
            Err(existing) => {
                // SAFETY: `raw` was allocated by us with `layout`; drop
                // each Slot (only the AtomicBool needs dropping; value
                // is MaybeUninit) and free the allocation.
                for i in 0..size {
                    unsafe { ptr::drop_in_place(raw.add(i)) };
                }
                unsafe { dealloc(raw as *mut u8, layout) };
                existing
            }
        }
    }

    /// Write `value` into the slot at logical index `idx` and publish
    /// `init=true`. Caller must hold an exclusive claim on the slot via
    /// `reserved.fetch_add` or a successful `compare_exchange_weak`.
    ///
    /// # Safety
    /// `idx` must be a slot the caller exclusively claimed and has not
    /// yet written to.
    unsafe fn write_at(&self, idx: usize, value: T) {
        let (bucket_idx, slot) = Self::locate(idx);
        let bucket_ptr = self.ensure_bucket(bucket_idx);
        // SAFETY: slot_ptr is in-bounds within the bucket; the slot is
        // exclusively ours by the caller's claim.
        unsafe {
            let slot_ptr = bucket_ptr.add(slot);
            (*(*slot_ptr).value.get()).write(value);
            (*slot_ptr).init.store(true, Ordering::Release);
        }
    }
}

unsafe impl<T: Send> Send for AppendOnlyVec<T> {}
unsafe impl<T: Sync> Sync for AppendOnlyVec<T> {}

impl<T> std::ops::Index<usize> for AppendOnlyVec<T> {
    type Output = T;

    /// Panicking indexed access. Use [`get`](Self::get) for a fallible
    /// version. Panics if `idx` is out of range or if the slot is
    /// reserved but not yet published by its writer.
    fn index(&self, idx: usize) -> &T {
        self.get(idx).unwrap_or_else(|| {
            panic!(
                "AppendOnlyVec index out of range or slot not yet published: {idx}"
            )
        })
    }
}

impl<T> Drop for AppendOnlyVec<T> {
    fn drop(&mut self) {
        let total = *self.reserved.get_mut();
        let mut remaining = total;
        for bucket_idx in 0..N_BUCKETS {
            let bucket_ptr = *self.buckets[bucket_idx].get_mut();
            if bucket_ptr.is_null() {
                break;
            }
            let size = Self::bucket_size(bucket_idx);
            // Drop initialised slots. A reserved-but-not-yet-published
            // slot has init=false, so we leave its value cell as
            // MaybeUninit and only drop the Slot's AtomicBool (via the
            // ptr::drop_in_place below).
            for slot in 0..size {
                // SAFETY: slot_ptr is in-bounds within the bucket.
                unsafe {
                    let slot_ptr = bucket_ptr.add(slot);
                    if *(*slot_ptr).init.get_mut() {
                        (*(*slot_ptr).value.get()).assume_init_drop();
                    }
                    ptr::drop_in_place(slot_ptr);
                }
            }
            remaining = remaining.saturating_sub(size);
            let layout = Layout::array::<Slot<T>>(size)
                .expect("bucket layout fits in usize");
            // SAFETY: bucket_ptr was allocated with this layout.
            unsafe { dealloc(bucket_ptr as *mut u8, layout) };
        }
        let _ = remaining;
    }
}

/// Iterator yielded by [`AppendOnlyVec::iter`].
pub struct Iter<'a, T> {
    vec: &'a AppendOnlyVec<T>,
    idx: usize,
    /// Upper bound captured at iterator construction; the iterator never
    /// looks past this index.
    upper: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (usize, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.upper {
            let i = self.idx;
            self.idx += 1;
            // get() returns None for not-yet-published slots; stop on the
            // first such slot so iteration yields a consistent prefix.
            match self.vec.get(i) {
                Some(v) => return Some((i, v)),
                None => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_starts_empty() {
        let v: AppendOnlyVec<String> = AppendOnlyVec::new();
        assert_eq!(v.count(), 0);
        assert!(v.get(0).is_none());
        assert_eq!(v.iter().count(), 0);
    }

    #[test]
    fn push_and_get_returns_stable_addresses() {
        let v: AppendOnlyVec<String> = AppendOnlyVec::new();
        let mut refs: Vec<*const String> = Vec::new();
        for i in 0..200 {
            let idx = v.push(format!("v{i}"));
            assert_eq!(idx, i);
            refs.push(v.get(i).unwrap() as *const String);
        }
        // After all the pushes (crossing multiple bucket boundaries),
        // earlier references must still resolve to the original strings.
        for (i, r) in refs.iter().enumerate() {
            // SAFETY: AppendOnlyVec guarantees stable addresses for the
            // vector's lifetime.
            let s: &String = unsafe { &**r };
            assert_eq!(s, &format!("v{i}"));
        }
    }

    #[test]
    fn iter_yields_in_order() {
        let v: AppendOnlyVec<String> = AppendOnlyVec::new();
        for i in 0..50 {
            v.push(format!("e{i}"));
        }
        let items: Vec<(usize, String)> =
            v.iter().map(|(i, s)| (i, s.clone())).collect();
        assert_eq!(items.len(), 50);
        for (i, (actual_idx, actual_str)) in items.iter().enumerate() {
            assert_eq!(*actual_idx, i);
            assert_eq!(actual_str, &format!("e{i}"));
        }
    }

    #[test]
    fn locate_round_trips_across_buckets() {
        assert_eq!(AppendOnlyVec::<String>::locate(0), (0, 0));
        assert_eq!(AppendOnlyVec::<String>::locate(15), (0, 15));
        assert_eq!(AppendOnlyVec::<String>::locate(16), (1, 0));
        assert_eq!(AppendOnlyVec::<String>::locate(47), (1, 31));
        assert_eq!(AppendOnlyVec::<String>::locate(48), (2, 0));
        assert_eq!(AppendOnlyVec::<String>::locate(111), (2, 63));
    }

    #[test]
    fn push_bounded_respects_cap_exactly() {
        let v: AppendOnlyVec<u32> = AppendOnlyVec::new();
        for i in 0..10 {
            assert_eq!(v.push_bounded(i, 10), Some(i as usize));
        }
        // Cap hit; further pushes return None and don't reserve a slot.
        assert_eq!(v.push_bounded(99, 10), None);
        assert_eq!(v.count(), 10);
        assert_eq!(v.push_bounded(99, 10), None);
        assert_eq!(v.count(), 10);
    }

    #[test]
    fn concurrent_multi_writer_respects_cap() {
        use std::sync::Arc;
        use std::thread;

        // Stress the u8-style 256-cap race: many threads each trying to
        // push, with 256 total slots available.
        let v: Arc<AppendOnlyVec<usize>> = Arc::new(AppendOnlyVec::new());
        let mut handles = Vec::new();
        for t in 0..16 {
            let v = Arc::clone(&v);
            handles.push(thread::spawn(move || {
                let mut local = Vec::new();
                for i in 0..100 {
                    if let Some(idx) = v.push_bounded(t * 1000 + i, 256) {
                        local.push(idx);
                    }
                }
                local
            }));
        }
        let mut all: Vec<usize> = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        // Exactly 256 successful pushes across all threads.
        assert_eq!(all.len(), 256);
        // Every claimed index is unique and in 0..256.
        all.sort();
        for (i, idx) in all.iter().enumerate() {
            assert_eq!(*idx, i);
        }
        assert_eq!(v.count(), 256);
    }

    #[test]
    fn concurrent_readers_with_multi_writer() {
        use std::sync::Arc;
        use std::thread;

        let v: Arc<AppendOnlyVec<String>> = Arc::new(AppendOnlyVec::new());
        let n_per_writer = 2_000;
        let n_writers = 4;

        let mut writer_handles = Vec::new();
        for t in 0..n_writers {
            let v = Arc::clone(&v);
            writer_handles.push(thread::spawn(move || {
                for i in 0..n_per_writer {
                    v.push(format!("w{t}_{i}"));
                }
            }));
        }

        let mut reader_handles = Vec::new();
        for _ in 0..3 {
            let v = Arc::clone(&v);
            reader_handles.push(thread::spawn(move || {
                let target = n_writers * n_per_writer;
                let mut seen_max = 0;
                while seen_max < target {
                    let n = v.count();
                    for i in 0..n {
                        // get() returns None for slots in flight; that's
                        // fine, we'll see them on the next pass.
                        if let Some(s) = v.get(i) {
                            assert!(s.starts_with('w'));
                        }
                    }
                    seen_max = n;
                }
            }));
        }

        for h in writer_handles {
            h.join().unwrap();
        }
        for h in reader_handles {
            h.join().unwrap();
        }
        assert_eq!(v.count(), n_writers * n_per_writer);
    }

    #[test]
    fn drop_runs_for_initialised_slots_only() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted(usize, &'static AtomicUsize);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.1.fetch_add(1, Ordering::SeqCst);
            }
        }

        static DROPS: AtomicUsize = AtomicUsize::new(0);
        {
            let v: AppendOnlyVec<Counted> = AppendOnlyVec::new();
            for i in 0..200 {
                v.push(Counted(i, &DROPS));
            }
        }
        assert_eq!(DROPS.load(Ordering::SeqCst), 200);
    }
}

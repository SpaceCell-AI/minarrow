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

//! Benchmark: LBuffer producer and consumer paths.
//!
//! Four groups, all reported as element throughput:
//!
//! - `push` - per-element `push`, batched `push_slice`, and a `Vec64::push`
//!   baseline. Shows the cost of per-element publication versus one `Release`
//!   per batch.
//! - `read` - a filled column read three ways: cached-bound slice sum (the
//!   recommended pattern), `n_rows()` re-evaluated per iteration over an
//!   LBuffer-backed table, and the same over an owned table. Isolates the
//!   length-read cost the live path adds.
//! - `tailing` - producer fill throughput with 0, 1, 2, and 4 passive reader
//!   threads tailing the live column with the cached-bound pattern. Shows the
//!   per-reader cost a real consumer imposes.
//!
//! Run with:
//!   cargo bench --bench lbuffer --features lbuffer

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;

use std::hint::black_box;

use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main,
};
use minarrow::{
    Array, Buffer, FieldArray, IntegerArray, LBuffer, LBufferV, Table, Vec64,
};

/// Elements per benchmarked operation.
const N: usize = 1 << 20;

/// Builds a two-column `Table` whose columns are filled LBuffer views of length
/// `n`. The returned handles are sealed and kept alive so the views stay valid.
fn filled_lbuffer_table(n: usize) -> (LBuffer<i64>, LBuffer<i64>, Table) {
    let mut a = LBuffer::<i64>::with_capacity(n);
    let mut b = LBuffer::<i64>::with_capacity(n);
    for i in 0..n {
        a.push(i as i64).unwrap();
        b.push(i as i64 * 2).unwrap();
    }
    a.seal();
    b.seal();
    let table = Table::new(
        "t".to_string(),
        Some(vec![
            FieldArray::from_arr(
                "a",
                Array::from_int64(IntegerArray::<i64> {
                    data: a.as_buffer(),
                    null_mask: None,
                }),
            ),
            FieldArray::from_arr(
                "b",
                Array::from_int64(IntegerArray::<i64> {
                    data: b.as_buffer(),
                    null_mask: None,
                }),
            ),
        ]),
    );
    (a, b, table)
}

/// Builds a two-column `Table` with owned columns of length `n`.
fn filled_owned_table(n: usize) -> Table {
    let a: Vec64<i64> = (0..n as i64).collect();
    let b: Vec64<i64> = (0..n as i64).map(|i| i * 2).collect();
    Table::new(
        "t".to_string(),
        Some(vec![
            FieldArray::from_arr(
                "a",
                Array::from_int64(IntegerArray::<i64> {
                    data: Buffer::from_vec64(a),
                    null_mask: None,
                }),
            ),
            FieldArray::from_arr(
                "b",
                Array::from_int64(IntegerArray::<i64> {
                    data: Buffer::from_vec64(b),
                    null_mask: None,
                }),
            ),
        ]),
    )
}

/// Sums column 0 over the current row count, reading `n_rows()` once and summing the slice
fn sum_table(table: &Table) -> i64 {
    let n = table.n_rows();
    let data = table.cols[0]
        .array
        .num_ref()
        .unwrap()
        .i64_ref()
        .unwrap()
        .data
        .as_slice();
    data[..n].iter().copied().sum()
}

fn bench_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("push");
    group.throughput(Throughput::Elements(N as u64));

    group.bench_function("lbuffer_push", |bencher| {
        bencher.iter_batched(
            || LBuffer::<i64>::with_capacity(N),
            |mut lb| {
                for i in 0..N {
                    lb.push(i as i64).unwrap();
                }
                black_box(&lb);
            },
            BatchSize::LargeInput,
        )
    });

    let src: Vec<i64> = (0..N as i64).collect();
    group.bench_function("lbuffer_push_slice", |bencher| {
        bencher.iter_batched(
            || LBuffer::<i64>::with_capacity(N),
            |mut lb| {
                lb.push_slice(black_box(&src)).unwrap();
                black_box(&lb);
            },
            BatchSize::LargeInput,
        )
    });

    group.bench_function("vec64_push", |bencher| {
        bencher.iter_batched(
            || Vec64::<i64>::with_capacity(N),
            |mut v| {
                for i in 0..N {
                    v.push(i as i64);
                }
                black_box(&v);
            },
            BatchSize::LargeInput,
        )
    });

    group.finish();
}

fn bench_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("read");
    group.throughput(Throughput::Elements(N as u64));

    let (_a, _b, lbuffer_table) = filled_lbuffer_table(N);
    let owned_table = filled_owned_table(N);
    let view: LBufferV<i64> = {
        // A standalone view for the cached-bound slice sum. The view holds an
        // Arc to the cell, so it stays valid after the producer handle drops.
        let mut lb = LBuffer::<i64>::with_capacity(N);
        for i in 0..N {
            lb.push(i as i64).unwrap();
        }
        lb.seal();
        lb.view()
    };

    group.bench_function("cached_bound_slice_sum", |bencher| {
        bencher.iter(|| {
            let slice = view.as_slice();
            black_box(slice.iter().copied().sum::<i64>())
        })
    });

    group.bench_function("table_sum_lbuffer", |bencher| {
        bencher.iter(|| black_box(sum_table(&lbuffer_table)))
    });

    group.bench_function("table_sum_owned", |bencher| {
        bencher.iter(|| black_box(sum_table(&owned_table)))
    });

    group.finish();
}

fn bench_tailing(c: &mut Criterion) {
    let mut group = c.benchmark_group("tailing");
    group.throughput(Throughput::Elements(N as u64));

    for n_readers in [0usize, 1, 2, 4] {
        // The current live view the readers tail. Replaced each producer
        // iteration with the fresh buffer's view.
        let slot: Arc<RwLock<Option<LBufferV<i64>>>> = Arc::new(RwLock::new(None));
        let stop = Arc::new(AtomicBool::new(false));

        let mut readers = Vec::with_capacity(n_readers);
        for _ in 0..n_readers {
            let slot = Arc::clone(&slot);
            let stop = Arc::clone(&stop);
            readers.push(thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let view = slot.read().unwrap().clone();
                    if let Some(view) = view {
                        // Cached-bound read: one Acquire load, then sum.
                        black_box(view.as_slice().iter().copied().sum::<i64>());
                    } else {
                        std::hint::spin_loop();
                    }
                }
            }));
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(n_readers),
            &n_readers,
            |bencher, _| {
                bencher.iter_batched(
                    || {
                        let lb = LBuffer::<i64>::with_capacity(N);
                        *slot.write().unwrap() = Some(lb.view());
                        lb
                    },
                    |mut lb| {
                        for i in 0..N {
                            lb.push(i as i64).unwrap();
                        }
                        black_box(&lb);
                    },
                    BatchSize::LargeInput,
                )
            },
        );

        stop.store(true, Ordering::Release);
        for handle in readers {
            handle.join().unwrap();
        }
    }

    group.finish();
}

criterion_group!(benches, bench_push, bench_read, bench_tailing);
criterion_main!(benches);

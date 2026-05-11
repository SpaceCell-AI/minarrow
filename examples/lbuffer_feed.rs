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

//! # LBuffer rolling quote feed example
//!
//! A top-of-book quote feed into an `LBuffer` window.
//!
//! The `SuperTable` holds every window, and its last batch is live: an
//! ordinary `Table` whose `bid` and `ask` field arrays are backed by
//! `LBuffer`s. The producer holds the write handles; the table lives in the
//! `SuperTable`.
//!
//! - Each tick carries a `bid` and an `ask`. The bid is always present. The
//!   ask is sometimes absent - an empty offer side, with no resting sell
//!   orders - and is pushed as a null, so `ask` is a masked column: its
//!   validity tails the producer the same way the values do.
//! - When the window fills, the producer seals it in place and rolls a fresh
//!   one in as the new live tail.
//! - The consumer reads the `SuperTable` through the ordinary array API.
//!   `n_rows()` reports the row floor every column has reached, and `is_null`
//!   skips quotes with no offer.
//!
//! The producer pushes per tick with no lock; the per-tick growth of values and
//! validity is published through atomic lengths alone. The only cross-thread
//! handoff is at a roll: the producer sends the freshly opened batch over a
//! channel. The hot path stays lock-free.
//!
//! Run with:
//!
//! ```ignore
//! cargo run --release --features lbuffer --example lbuffer_feed
//! # longer / faster / different window:
//! FEED_SECS=30 FEED_RATE=250000 FEED_WINDOW=250000 \
//!   cargo run --release --features lbuffer --example lbuffer_feed
//! ```

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use minarrow::{Array, FieldArray, FloatArray, LBuffer, MaskedArray, SuperTable, Table};

/// Wall-clock duration of the feed, in seconds. Override with `FEED_SECS`.
const DEFAULT_SECS: u64 = 30;

/// Target tick rate per second. Override with `FEED_RATE`.
const DEFAULT_RATE: usize = 100_000;

/// Rows per window. The live last batch seals and a new one rolls in once it
/// reaches this many rows. Override with `FEED_WINDOW`.
const DEFAULT_WINDOW: usize = 100_000;

/// Consumer dashboard refresh interval.
const REFRESH: Duration = Duration::from_millis(250);

/// Reads an environment variable as the requested type, falling back to `default`.
fn env_or<T: FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Opens a new window: builds a `Table` whose `bid` and `ask` field arrays
/// are `LBuffer`-backed, hands the consumer that batch over the channel, and
/// returns the producer handles that feed those columns.
///
/// `ask` is a masked column - an empty offer side is pushed as a null, and its
/// validity tails the producer the way the values do. `bid` is always present.
fn open_window(batches: &Sender<Arc<Table>>, window: usize) -> (LBuffer<f64>, LBuffer<f64, true>) {
    let bid = LBuffer::<f64>::with_capacity(window);
    let ask = LBuffer::<f64>::with_capacity_masked(window);
    let table = Arc::new(Table::new(
        "quotes".to_string(),
        Some(vec![
            FieldArray::from_arr(
                "bid",
                Array::from_float64(FloatArray::<f64> {
                    data: bid.as_buffer(),
                    null_mask: None,
                }),
            ),
            FieldArray::from_arr(
                "ask",
                Array::from_float64(FloatArray::<f64> {
                    data: ask.as_buffer(),
                    null_mask: Some(ask.as_bitmask()),
                }),
            ),
        ]),
    ));
    // Hand the freshly opened batch to the consumer. It tails this same batch
    // as the producer fills it through the returned handles.
    batches.send(table).unwrap();
    (bid, ask)
}

/// Rolling aggregation over the whole feed.
struct Aggregates {
    rows: usize,
    no_offer: usize,
    last_bid: f64,
    last_ask: f64,
    min_bid: f64,
    max_bid: f64,
    mean_bid: f64,
}

/// Folds one window's `bid`/`ask` columns over its first `n` rows. The bid is
/// always present; an `ask` with no offer is null - counted, not quoted.
fn fold_batch(
    table: &Table,
    min_bid: &mut f64,
    max_bid: &mut f64,
    sum_bid: &mut f64,
    rows: &mut usize,
    no_offer: &mut usize,
    last_bid: &mut f64,
    last_ask: &mut f64,
) {
    let n = table.n_rows();
    if n == 0 {
        return;
    }
    let bid = table.cols[0].array.num_ref().unwrap().f64_ref().unwrap();
    let ask = table.cols[1].array.num_ref().unwrap().f64_ref().unwrap();
    let bid_rows = &bid.data.as_slice()[..n];
    let ask_rows = &ask.data.as_slice()[..n];

    for i in 0..n {
        let b = bid_rows[i];
        *min_bid = min_bid.min(b);
        *max_bid = max_bid.max(b);
        *sum_bid += b;
        if ask.is_null(i) {
            *no_offer += 1;
        } else {
            *last_ask = ask_rows[i];
        }
    }
    *rows += n;
    *last_bid = bid_rows[n - 1];
}

/// Aggregates across every batch, the last of which is the live tail. Returns
/// `None` while the feed is still empty.
fn aggregate(feed: &SuperTable) -> Option<Aggregates> {
    let mut min_bid = f64::INFINITY;
    let mut max_bid = f64::NEG_INFINITY;
    let mut sum_bid = 0.0;
    let mut rows = 0usize;
    let mut no_offer = 0usize;
    let mut last_bid = 0.0;
    let mut last_ask = 0.0;

    for batch in feed.batches() {
        fold_batch(
            batch,
            &mut min_bid,
            &mut max_bid,
            &mut sum_bid,
            &mut rows,
            &mut no_offer,
            &mut last_bid,
            &mut last_ask,
        );
    }

    if rows == 0 {
        return None;
    }
    Some(Aggregates {
        rows,
        no_offer,
        last_bid,
        last_ask,
        min_bid,
        max_bid,
        mean_bid: sum_bid / rows as f64,
    })
}

fn main() {
    let secs = env_or("FEED_SECS", DEFAULT_SECS);
    let rate = env_or("FEED_RATE", DEFAULT_RATE);
    let window = env_or("FEED_WINDOW", DEFAULT_WINDOW);

    println!("LBuffer rolling quote feed demo\n");
    println!(
        "A producer appends ~{rate} top-of-book quotes/s for {secs}s into the live last\n\
         batch of a SuperTable, a window of {window} rows. Each full window seals in place\n\
         and a fresh live batch rolls in. A consumer tails the SuperTable through the\n\
         ordinary array API, refreshing {}x per second.\n",
        1000 / REFRESH.as_millis()
    );
    println!(
        "Each line is one consumer refresh:\n\
         - batches:  windows in the SuperTable, the last being the live tail\n\
         - rows:     total quotes across all batches\n\
         - no-offer: quotes with an empty ask side (null)\n\
         - bid:      most recent bid\n\
         - ask:      most recent quoted ask\n\
         - min/max/mean: bid over every quote so far\n\
         - rows/s:   quotes observed since the previous refresh\n"
    );

    // The producer hands each opened batch to the consumer over this channel.
    // The consumer builds its own SuperTable from the batches it receives; the
    // last one received is the live window it tails.
    let (batches, batches_rx): (Sender<Arc<Table>>, Receiver<Arc<Table>>) = mpsc::channel();
    let (mut bid_feed, mut ask_feed) = open_window(&batches, window);

    let running = Arc::new(AtomicBool::new(true));

    let consumer = {
        let running = Arc::clone(&running);
        thread::spawn(move || {
            let mut feed = SuperTable::new("feed".to_string());
            let mut last_print = Instant::now();
            let mut last_rows = 0usize;
            loop {
                // Pick up any newly opened batches; the latest is the live tail.
                while let Ok(batch) = batches_rx.try_recv() {
                    feed.push(batch);
                }
                let due = last_print.elapsed() >= REFRESH;
                let stopping = !running.load(Ordering::Acquire);
                if due || stopping {
                    if let Some(agg) = aggregate(&feed) {
                        let dt = last_print.elapsed().as_secs_f64().max(1e-9);
                        let rate_now = (agg.rows - last_rows) as f64 / dt;
                        println!(
                            "batches {:>3} | rows {:>9} | no-offer {:>8} | bid {:>7.2} | ask {:>7.2} | min {:>7.2} | max {:>7.2} | mean {:>7.2} | {:>10.0} rows/s",
                            feed.n_batches(),
                            agg.rows,
                            agg.no_offer,
                            agg.last_bid,
                            agg.last_ask,
                            agg.min_bid,
                            agg.max_bid,
                            agg.mean_bid,
                            rate_now,
                        );
                        last_rows = agg.rows;
                    }
                    last_print = Instant::now();
                }
                if stopping {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            feed.n_batches()
        })
    };

    // Quote generator. The bid is a mean-reverting random walk around 100,
    // driven by a deterministic xorshift so the run is reproducible. The ask
    // sits a small spread above the bid, and is occasionally absent - an empty
    // offer side - in which case it is pushed as a null.
    let start = Instant::now();
    let duration = Duration::from_secs(secs);
    let mut rows_in_window = 0usize;
    let mut total = 0usize;
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut bid = 100.0_f64;

    'feed: loop {
        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }
        // Catch up to the row count the target rate implies by now.
        let target = (elapsed.as_secs_f64() * rate as f64) as usize;
        while total < target {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let step = ((state >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * 0.2;
            let reversion = -0.02 * (bid - 100.0);
            bid = (bid + reversion + step).max(1.0);
            let spread = 0.01 + ((state >> 20) & 0x7) as f64 * 0.01;
            let ask = bid + spread;

            // Bid leads, ask trails: the row count follows the trailing column,
            // so a reader never sees a bid without its (possibly empty) ask.
            bid_feed.push(bid).unwrap();
            if (state >> 29) & 0x7F == 0 {
                // Empty offer side - no resting sell orders - so no ask.
                ask_feed.push_null().unwrap();
            } else {
                ask_feed.push(ask).unwrap();
            }
            rows_in_window += 1;
            total += 1;

            if rows_in_window == window {
                // Window closed: seal it, then open and hand over a fresh batch.
                bid_feed.seal();
                ask_feed.seal();
                let (next_bid, next_ask) = open_window(&batches, window);
                bid_feed = next_bid;
                ask_feed = next_ask;
                rows_in_window = 0;

                if start.elapsed() >= duration {
                    break 'feed;
                }
            }
        }
        thread::sleep(Duration::from_millis(1));
    }

    // Seal the final partial window and signal end of stream. It stays the live
    // last batch the consumer holds - the last window never filled.
    bid_feed.seal();
    ask_feed.seal();
    running.store(false, Ordering::Release);

    let batch_count = consumer.join().unwrap();
    println!("\nfeed closed: {batch_count} batches, {total} total quotes");
}

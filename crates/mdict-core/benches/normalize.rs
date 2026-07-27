//! Phase 3 回归门禁：候选生成热路径的 criterion 微基准。
//!
//! 丈量 `canonical_normalize` / `prefix_upper` / `entry_query_candidates`
//! 在真实词形（ascii、变音、标点、词形回退）下的延迟。这些纯函数是每次
//! 按键查询的必经路径，应保持亚微秒级；任何回退（如把 32 候选展开重新
//! 引入热路径、或归一化逻辑变重）会在此被立即放大。

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use mdict_core::normalize::{canonical_normalize, entry_query_candidates, prefix_upper};

const INPUTS: &[&str] = &[
    "cat",                            // 简单词，候选少
    "running",                        // -ing + 双写辅音
    "categories",                     // -ies → -y
    "Café—menu",                      // 变音 + 标点
    "naïve",                          // 拉丁变音
    "it's",                           // 撇号
    "Pseudopseudohypoparathyroidism", // 长词
];

fn bench_canonical_normalize(c: &mut Criterion) {
    c.bench_function("canonical_normalize/7_inputs", |b| {
        b.iter(|| {
            for &w in black_box(INPUTS) {
                black_box(canonical_normalize(w));
            }
        });
    });
}

fn bench_prefix_upper(c: &mut Criterion) {
    c.bench_function("prefix_upper/7_inputs", |b| {
        b.iter(|| {
            for &w in black_box(INPUTS) {
                black_box(prefix_upper(w));
            }
        });
    });
}

fn bench_entry_query_candidates(c: &mut Criterion) {
    c.bench_function("entry_query_candidates/7_inputs", |b| {
        b.iter(|| {
            for &w in black_box(INPUTS) {
                black_box(entry_query_candidates(w));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_canonical_normalize,
    bench_prefix_upper,
    bench_entry_query_candidates,
);
criterion_main!(benches);

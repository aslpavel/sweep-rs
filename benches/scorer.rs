use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use sweep_lib::{Candidate, FuzzyScorer, Scorer, SubstrScorer};

pub fn scorer_benchmark(c: &mut Criterion) {
    let candidate = String::from("./benchmark/target/release/.fingerprint/semver-parser-a5e84da67081840e/lib-semver_parser-a5e84da67081840e.json");
    let haystack = Candidate::new(candidate, ' ', &None);
    let niddle = "test";
    let fuzzy = FuzzyScorer::new();
    let substr = SubstrScorer::new();

    let mut group = c.benchmark_group("scorer");
    group.throughput(Throughput::Elements(1 as u64));
    group.bench_function("fuzzy", |b| b.iter(|| fuzzy.score_ref(niddle, &haystack)));
    group.bench_function("substr", |b| b.iter(|| substr.score_ref(niddle, &haystack)));
    group.finish();
}

criterion_group!(benches, scorer_benchmark);
criterion_main!(benches);

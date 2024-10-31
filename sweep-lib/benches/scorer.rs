use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use mimalloc::MiMalloc;
use sweep::{FuzzyScorer, KMPPattern, Positions, Score, Scorer, SubstrScorer};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const CANDIDATE: &str = "./benchmark/target/release/.fingerprint/semver-parser-a5e84da67081840e/test/lib-semver_parser-a5e84da67081840e.json";

pub fn scorer_benchmark(c: &mut Criterion) {
    let haystack: Vec<_> = CANDIDATE.chars().collect();
    let needle: Vec<_> = "test".chars().collect();
    let fuzzy = FuzzyScorer::new(needle.clone());
    let substr = SubstrScorer::new(needle.clone());
    let kmp = KMPPattern::new(needle);

    let mut group = c.benchmark_group("scorer");
    group.throughput(Throughput::Elements(1_u64));

    let mut score = Score::MIN;
    let mut positions = Positions::new(CANDIDATE.len());
    group.bench_function("fuzzy", |b| {
        b.iter(|| fuzzy.score_ref(haystack.as_slice(), &mut score, positions.as_mut()))
    });

    let mut score = Score::MIN;
    let mut positions = Positions::new(CANDIDATE.len());
    group.bench_function("substr", |b| {
        b.iter(|| substr.score_ref(haystack.as_slice(), &mut score, positions.as_mut()))
    });

    group.bench_function("knuth-morris-pratt", |b| {
        b.iter(|| kmp.search(haystack.as_slice()))
    });

    group.finish();
}

criterion_group!(benches, scorer_benchmark);
criterion_main!(benches);

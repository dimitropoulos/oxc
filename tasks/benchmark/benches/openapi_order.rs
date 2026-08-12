use oxc_benchmark::{BenchmarkId, Criterion, criterion_group, criterion_main};
use oxc_openapi_order::{Scratch, Table, permutation};

/// The `get` / `post` / ... table, which is the one most mappings in a real spec resolve to,
/// plus `tags` so the table is one entry wider than the six-entry built-in.
const OPERATION: &[&str] =
    &["operationId", "summary", "description", "parameters", "requestBody", "responses", "tags"];

fn bench_openapi_order(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("openapi_order");

    // Fifty unranked keys, to exercise the case-insensitive comparison rather than the rank lookup.
    let unranked: Vec<String> = (0..50).map(|i| format!("x-vendor-{i:02}")).collect();
    let unranked: Vec<&str> = unranked.iter().map(String::as_str).collect();
    let mut unranked_reversed = unranked.clone();
    unranked_reversed.reverse();

    let cases: [(&str, Vec<&str>); 3] = [
        // Already in table order. This is the early-out: `permutation` ranks each key once, finds the
        // ranks non-decreasing, and returns `None` without ever building a permutation. It is the
        // case that matters most in practice, because it is what `--check` on an already-formatted
        // spec costs, and what every already-sorted mapping inside a large document costs.
        ("already-sorted", OPERATION.to_vec()),
        // Exactly reversed, so every key is out of place and the full sort runs.
        ("reversed", OPERATION.iter().rev().copied().collect()),
        // Unranked keys in reverse order: no key is in the table, so the whole ordering falls to the
        // case-insensitive comparator.
        ("unranked-50-reversed", unranked_reversed),
    ];

    for (name, keys) in &cases {
        // One `Scratch` per benchmark, reused across iterations, which is how a format run uses it:
        // allocating per call would measure the allocator instead of the ordering.
        let mut scratch = Scratch::new();
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            // The permutation borrows `scratch`, so it cannot escape the closure; returning its
            // length both satisfies that and keeps the call from being optimised away.
            b.iter(|| permutation(Table::Builtin(OPERATION), keys, &mut scratch).map(<[u32]>::len));
        });
    }

    group.finish();
}

criterion_group!(openapi_order, bench_openapi_order);
criterion_main!(openapi_order);

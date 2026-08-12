//! Differential test of the ordering policy against openapi-format's default pass.
//!
//! Three implementations meet here, which is what makes the test worth running:
//!
//! - the crate, via `subject.rs`;
//! - `reference.rs`, an independent transcription of the reference's traversal, written from its
//!   JavaScript and sharing no code with the crate;
//! - `expected_order` below, an independent stable sort using the transcription's comparator, which
//!   checks the crate's own output rather than only its agreement with the reference.
//!
//! Comparing only against the reference would let any bug that lands inside a documented divergence
//! go unnoticed: index-key order is normalised away by bucketing, and tie order is unconstrained by
//! definition, so the reference cannot police those regions. `expected_order` can.
//!
//! The reference's known artifacts are classified, not excluded: a divergence is only a failure when
//! it belongs to no known bucket. See [`Bucket`].
//!
//! The root content gate here is upstream's, JavaScript truthiness of the `openapi` member, because
//! the point is to compare against upstream. Both formatter backends pass key presence instead; see
//! the crate docs for that divergence, which unit tests pin rather than this fuzz.
//!
//! Out of scope: `PathsOrder`. `sortPathsBy` defaults to `original`, so ordering the `paths` mapping
//! is not part of the default pass this test differentiates against. Both path comparators are
//! pinned by unit tests in `paths.rs`, measured against the reference's `sortPathsByAlphabet` and
//! `sortPathsByTags`.
//!
//! # Validating the oracle
//!
//! The transcription is only worth its fidelity to the real package. To check it:
//!
//! ```sh
//! OPENAPI_ORDER_FUZZ_DUMP=/tmp/openapi-order-corpus \
//!   cargo test -p oxc_openapi_order --test differential
//! node crates/oxc_openapi_order/tests/differential/validate_reference.mjs \
//!   /tmp/openapi-order-corpus
//! ```
//!
//! The script needs `openapi-format` v1.33.6 resolvable from the current directory. It reports the
//! number of documents where the transcription and the real package disagree; that number must be
//! zero.

mod differential {
    pub mod generate;
    pub mod reference;
    pub mod subject;
    pub mod value;
}

use std::{collections::BTreeMap, env, fs, path::Path};

use oxc_openapi_order::{KeyOrderEntry, Options, SortOpenapi, Table, resolve, resolve_root};

use differential::{
    generate, reference,
    reference::RefOptions,
    subject::{self, Order, OwnedStep, borrow},
    value::{Value, array_index_key, js_key_order},
};

/// How many documents each configuration contributes.
const DOCUMENTS_PER_CONFIG: u64 = 1_500;

/// Floors for what the corpus must actually reach, so a generator regression fails loudly instead of
/// quietly shrinking the test. Bands rather than `> 0`: every one of these dropped by more than half
/// at some point during development without any assertion noticing.
const MIN_MAPPINGS_COMPARED: u64 = 45_000;
const MIN_TABLES_RESOLVED: u64 = 25_000;

/// Why a mapping's key order differs from the reference's.
///
/// Every bucket except [`Bucket::Unexplained`] is a documented divergence; see the crate docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    /// D1: the reference rebuilt a sequence as a mapping keyed by index.
    ReferenceChangedNodeKind,
    /// D2: the reference's order is ours re-bucketed by ECMAScript object property order.
    ReferenceBucketedIndexKeys,
    /// D3: the reference dropped a `__proto__` key.
    ReferenceDroppedProtoKey,
    /// D4: the reference ordered this mapping twice, once from a parent in child role and then
    /// again from its own table, and its second sort was stable, so keys its own table ties kept the
    /// order the parent's table gave them. We order once and break ties by source position.
    TieBrokenByParentTable,
    /// Anything else. One of these is a test failure.
    Unexplained,
}

/// One configuration to fuzz.
struct Config {
    name: &'static str,
    /// The crate's options.
    /// Our options in their owned form, so this harness exercises the same owned-to-borrowed path
    /// the formatter backends do rather than a shape only a test can build.
    ours: SortOpenapi,
    /// The transcription's options.
    theirs: RefOptions,
    /// Compare key orders against the transcription.
    ///
    /// `false` where the reference has no like-for-like spelling of the option, in which case only
    /// the invariants and `expected_order` apply.
    compare: bool,
    /// Dump the corpus for `validate_reference.mjs`.
    validate_oracle: bool,
}

fn configs() -> Vec<Config> {
    vec![
        Config {
            name: "default",
            ours: SortOpenapi::default(),
            theirs: RefOptions::default(),
            compare: true,
            validate_oracle: true,
        },
        Config {
            name: "properties",
            ours: SortOpenapi { properties: true, ..SortOpenapi::default() },
            theirs: RefOptions { components: false, properties: true },
            compare: true,
            validate_oracle: true,
        },
        Config {
            name: "components",
            ours: SortOpenapi { components: true, ..SortOpenapi::default() },
            theirs: RefOptions { components: true, properties: false },
            // `sortComponentsSet` being non-empty also switches on the reference's
            // `arraySort(node, 'name')` pass, which reorders sequence elements by their `name`
            // member. The crate deliberately never reorders elements, and the transcription does not
            // model that pass, so comparing key orders here would be comparing against an oracle
            // known to be unfaithful. The option's own behaviour is pinned by unit tests measured
            // against the real package, and `expected_order` still polices every mapping.
            compare: false,
            // Same reason, plus: no fixed list expresses "every member", because the arm itself
            // creates member names (`"0"`, `"1"`, ...) when it rebuilds a sequence.
            validate_oracle: false,
        },
        Config {
            name: "keyOrder",
            ours: SortOpenapi {
                key_order: vec![KeyOrderEntry {
                    key: "get".to_string(),
                    fields: vec!["summary".to_string(), "operationId".to_string()],
                }],
                ..SortOpenapi::default()
            },
            theirs: RefOptions::default(),
            // The reference's `sortSet` replaces the whole table set rather than layering over it,
            // so there is no like-for-like comparison.
            compare: false,
            validate_oracle: false,
        },
    ]
}

/// Counters and findings for one run.
#[derive(Default)]
struct Report {
    buckets: BTreeMap<Bucket, u64>,
    unexplained: Vec<String>,
    mappings_compared: u64,
    tables_resolved: u64,
    /// Mappings whose order `expected_order` independently confirmed.
    orders_confirmed: u64,
}

#[test]
#[expect(clippy::print_stdout, reason = "the harness reports what it actually covered")]
fn differential_against_openapi_format() {
    let dump_dir = env::var("OPENAPI_ORDER_FUZZ_DUMP").ok();
    if let Some(dir) = &dump_dir {
        fs::create_dir_all(dir).expect("dump directory");
    }

    let mut documents = 0_u64;
    let mut report = Report::default();
    let mut override_changed_something = false;

    for (index, config) in configs().into_iter().enumerate() {
        let Config { name, ours, theirs: ref_options, compare, validate_oracle } = config;
        let options = ours.options();
        // Each configuration gets its own seed range: reusing one range would quadruple the document
        // count while covering the same 1,500 inputs.
        let base = index as u64 * DOCUMENTS_PER_CONFIG;

        for seed in base..base + DOCUMENTS_PER_CONFIG {
            let original = generate::document(seed);
            documents += 1;

            let mut ours = original.clone();
            subject::order(&mut ours, &options, Order::PreOrder);

            // Ordering must never change the document's shape or its key sets.
            assert_structure_preserved(&original, &ours, name, seed);

            // Ordering each mapping once is confluent. This guards `resolve`'s purity and `apply`
            // leaving keys alone; it cannot detect a wrong table.
            let mut post = original.clone();
            subject::order(&mut post, &options, Order::PostOrder);
            assert_eq!(
                ours, post,
                "[{name}/{seed}] pre-order and post-order walks disagree, so a mapping's order \
                 depends on when it was visited"
            );

            // Ordering is idempotent.
            let mut again = ours.clone();
            subject::order(&mut again, &options, Order::PreOrder);
            assert_eq!(ours, again, "[{name}/{seed}] ordering is not idempotent");

            if name == "keyOrder" {
                let mut plain = original.clone();
                subject::order(&mut plain, &Options::default(), Order::PreOrder);
                override_changed_something |= plain != ours;
            }

            let mut theirs = original.clone();
            reference::sort(&mut theirs, &ref_options);

            if let Some(dir) = &dump_dir
                && validate_oracle
            {
                dump(Path::new(dir), name, seed, &original, &theirs);
            }

            let mut path: Vec<OwnedStep> = Vec::new();
            walk(
                &original,
                &ours,
                if compare { Some(&theirs) } else { None },
                &mut path,
                &options,
                &mut report,
                name,
                seed,
            );
        }
    }

    let total: u64 = report.buckets.values().sum();
    println!("documents:          {documents}");
    println!("mappings compared:  {}", report.mappings_compared);
    println!("orders confirmed:   {}", report.orders_confirmed);
    println!("tables resolved:    {}", report.tables_resolved);
    println!("divergences:        {total}");
    for (bucket, count) in &report.buckets {
        println!("  {bucket:?}: {count}");
    }

    assert!(
        documents >= 3_000,
        "the corpus must cover at least 3,000 documents, covered {documents}"
    );
    assert!(
        report.mappings_compared >= MIN_MAPPINGS_COMPARED,
        "only {} mappings compared, expected at least {MIN_MAPPINGS_COMPARED}; the generator has \
         regressed",
        report.mappings_compared
    );
    assert!(
        report.tables_resolved >= MIN_TABLES_RESOLVED,
        "only {} mappings had a table resolved, expected at least {MIN_TABLES_RESOLVED}; the corpus \
         is no longer reaching the rule",
        report.tables_resolved
    );
    assert!(override_changed_something, "the `keyOrder` override changed no document's order");
    assert!(
        report.unexplained.is_empty(),
        "{} unexplained divergences, first 5:\n{}",
        report.unexplained.len(),
        report.unexplained.iter().take(5).cloned().collect::<Vec<_>>().join("\n")
    );
    // Each documented divergence must stay reachable, or the bucket is dead code that silently stops
    // protecting anything. Bands, not `> 0`: a generator change once halved one of these unnoticed.
    for (bucket, min) in [
        (Bucket::ReferenceChangedNodeKind, 1_500),
        (Bucket::ReferenceBucketedIndexKeys, 2_500),
        (Bucket::ReferenceDroppedProtoKey, 300),
        (Bucket::TieBrokenByParentTable, 1),
    ] {
        let count = report.buckets.get(&bucket).copied().unwrap_or(0);
        assert!(count >= min, "{bucket:?} fired {count} times, expected at least {min}");
    }
}

/// The order the crate should produce for `keys` under `table`, derived independently.
///
/// A stable sort using the transcription's comparator. The crate's comparator is the same relation
/// plus an explicit source-position tiebreak, which is what a stable sort gives for free, so this is
/// a second implementation of the crate's own specification rather than a restatement of it.
fn expected_order<'k>(keys: &[&'k str], table: Option<Table<'_>>) -> Vec<&'k str> {
    let Some(table) = table else { return keys.to_vec() };
    // The oracle comparator models JavaScript and wants a plain slice. Flattening allocates, which
    // is why the production path does not do it -- here it is the oracle, so cost does not matter.
    let table: Vec<&str> = match table {
        Table::Builtin(table) => table.to_vec(),
        Table::User(table) => table.iter().map(String::as_str).collect(),
    };
    let mut ordered = keys.to_vec();
    ordered.sort_by(|left, right| reference::prop_comparator(&table, left, right));
    ordered
}

/// Ordering may permute entries. It may not do anything else.
fn assert_structure_preserved(before: &Value, after: &Value, config: &str, seed: u64) {
    assert_eq!(
        before.kind(),
        after.kind(),
        "[{config}/{seed}] node kind changed: {} -> {}",
        before.kind(),
        after.kind()
    );
    match (before, after) {
        (Value::Seq(left), Value::Seq(right)) => {
            assert_eq!(left.len(), right.len(), "[{config}/{seed}] sequence length changed");
            for (left, right) in left.iter().zip(right) {
                assert_structure_preserved(left, right, config, seed);
            }
        }
        (Value::Map(left), Value::Map(right)) => {
            let mut before_keys: Vec<&str> = left.iter().map(|(key, _)| key.as_str()).collect();
            let mut after_keys: Vec<&str> = right.iter().map(|(key, _)| key.as_str()).collect();
            before_keys.sort_unstable();
            after_keys.sort_unstable();
            assert_eq!(before_keys, after_keys, "[{config}/{seed}] key set changed");
            for (key, value) in right {
                let original = left
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
                    .expect("key sets already compared equal");
                assert_structure_preserved(original, value, config, seed);
            }
        }
        // Scalars: `kind()` already proved the type matches, and equality proves the value does.
        (left, right) => assert_eq!(left, right, "[{config}/{seed}] scalar value changed"),
    }
}

/// Walk all three trees together: check our order against [`expected_order`], then against the
/// reference's, classifying any difference.
///
/// `theirs` is `None` for configurations the reference cannot model; the independent check still
/// applies.
#[expect(clippy::too_many_arguments, reason = "a walk over three trees plus its report")]
fn walk(
    original: &Value,
    ours: &Value,
    theirs: Option<&Value>,
    path: &mut Vec<OwnedStep>,
    options: &Options<'_>,
    report: &mut Report,
    config: &str,
    seed: u64,
) {
    match (original, ours) {
        (Value::Seq(original_items), Value::Seq(our_items)) => {
            // The reference rebuilds a sequence as a mapping keyed by index whenever it sorts one.
            // Rather than stop here, keep descending: its entries are in index order, so they still
            // line up element for element, and the subtrees below stay under comparison.
            if matches!(theirs, Some(Value::Map(_))) {
                *report.buckets.entry(Bucket::ReferenceChangedNodeKind).or_default() += 1;
            }
            for (index, (original, ours)) in original_items.iter().zip(our_items).enumerate() {
                let theirs = match theirs {
                    Some(Value::Seq(items)) => items.get(index),
                    Some(Value::Map(entries)) => entries.get(index).map(|(_, value)| value),
                    _ => None,
                };
                path.push(OwnedStep::Index(u32::try_from(index).expect("fits in u32")));
                walk(original, ours, theirs, path, options, report, config, seed);
                path.pop();
            }
        }
        (Value::Map(original_entries), Value::Map(our_entries)) => {
            let source_keys: Vec<&str> =
                original_entries.iter().map(|(key, _)| key.as_str()).collect();
            let our_keys: Vec<&str> = our_entries.iter().map(|(key, _)| key.as_str()).collect();

            let table = resolve_for(original, path, options);
            if table.is_some() {
                report.tables_resolved += 1;
            }

            // The independent check: our order must be what a stable sort with the transcription's
            // comparator produces. This is the only check that constrains our output inside the
            // documented divergences.
            let expected = expected_order(&source_keys, table);
            assert_eq!(
                our_keys,
                expected,
                "[{config}/{seed}] our order disagrees with an independent stable sort at {}",
                describe(path)
            );
            report.orders_confirmed += 1;

            if let Some(Value::Map(their_entries)) = theirs {
                report.mappings_compared += 1;
                let their_keys: Vec<&str> =
                    their_entries.iter().map(|(key, _)| key.as_str()).collect();
                if our_keys != their_keys {
                    let bucket =
                        classify(original, path, options, &source_keys, &our_keys, &their_keys);
                    *report.buckets.entry(bucket).or_default() += 1;
                    if bucket == Bucket::Unexplained {
                        report.unexplained.push(format!(
                            "[{config}/{seed}] at {} ours={our_keys:?} theirs={their_keys:?}",
                            describe(path)
                        ));
                    }
                }
            } else if let Some(theirs) = theirs {
                // Our mapping, their something else: the reference changed the node's kind.
                if matches!(theirs, Value::Seq(_)) {
                    *report.buckets.entry(Bucket::ReferenceChangedNodeKind).or_default() += 1;
                }
            }

            for (key, ours) in our_entries {
                let original = original_entries
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
                    .expect("structure already verified");
                let theirs = match theirs {
                    Some(Value::Map(entries)) => {
                        entries.iter().find(|(name, _)| name == key).map(|(_, value)| value)
                    }
                    _ => None,
                };
                path.push(OwnedStep::Key(key.clone()));
                walk(original, ours, theirs, path, options, report, config, seed);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The table that orders the mapping at `path`, including the root's content gate.
fn resolve_for<'o>(
    original: &Value,
    path: &[OwnedStep],
    options: &Options<'o>,
) -> Option<Table<'o>> {
    if path.is_empty() {
        let gate = match original {
            Value::Map(entries) => {
                entries.iter().any(|(key, value)| key == "openapi" && value.is_truthy())
            }
            _ => false,
        };
        return resolve_root(options, gate);
    }
    resolve(options, &borrow(path))
}

fn describe(path: &[OwnedStep]) -> String {
    if path.is_empty() {
        return "<root>".to_string();
    }
    path.iter()
        .map(|step| match step {
            OwnedStep::Key(key) => key.clone(),
            OwnedStep::Index(index) => format!("#{index}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Which documented divergence explains `their_keys` differing from `our_keys`?
///
/// Each branch is positive: it reconstructs what the reference would have produced and requires an
/// exact match. A residual "close enough" test would accept genuine bugs, because every divergence
/// this test exists to catch has the same shape as one of these.
fn classify(
    original: &Value,
    path: &[OwnedStep],
    options: &Options<'_>,
    source_keys: &[&str],
    our_keys: &[&str],
    their_keys: &[&str],
) -> Bucket {
    // D3: the reference cannot round-trip a `__proto__` own property.
    if our_keys.contains(&"__proto__") && !their_keys.contains(&"__proto__") {
        let survivors: Vec<&str> =
            source_keys.iter().copied().filter(|key| *key != "__proto__").collect();
        let table = resolve_for(original, path, options);
        if bucketed(&expected_order(&survivors, table)) == their_keys {
            return Bucket::ReferenceDroppedProtoKey;
        }
    }

    // D2: ECMAScript object property order applied to the order we agree on.
    if bucketed(our_keys) == their_keys && our_keys.iter().any(|key| array_index_key(key).is_some())
    {
        return Bucket::ReferenceBucketedIndexKeys;
    }

    // D4: the reference ordered this mapping from its parent's child-role table first, then stably
    // from its own. Reconstruct exactly that composition; a residual test here would accept any tie
    // permutation, which is the signature of every tie-order bug.
    if let Some(parent) = path.len().checked_sub(1).map(|len| &path[..len])
        && let Some(parent_table) = reference::child_role_table(&to_segs(parent))
    {
        let first = expected_order(source_keys, Some(Table::Builtin(parent_table)));
        let table = resolve_for(original, path, options);
        if bucketed(&expected_order(&first, table)) == their_keys {
            return Bucket::TieBrokenByParentTable;
        }
    }

    Bucket::Unexplained
}

/// The path in the transcription's own segment type.
fn to_segs(path: &[OwnedStep]) -> Vec<reference::Seg> {
    path.iter()
        .map(|step| match step {
            OwnedStep::Key(key) => reference::Seg::Key(key.clone()),
            OwnedStep::Index(_) => reference::Seg::Index,
        })
        .collect()
}

/// `keys` in ECMAScript object property order.
fn bucketed<'k>(keys: &[&'k str]) -> Vec<&'k str> {
    js_key_order(keys).into_iter().map(|index| keys[index]).collect()
}

/// Write one case out for `validate_reference.mjs`.
fn dump(dir: &Path, config: &str, seed: u64, input: &Value, expected: &Value) {
    let path = dir.join(format!("{config}-{seed:05}.json"));
    let contents = format!("{{\"input\":{},\"expected\":{}}}", input.to_json(), expected.to_json());
    fs::write(path, contents).expect("write dump");
}

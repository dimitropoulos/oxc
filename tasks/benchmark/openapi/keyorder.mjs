// Compares the key order of two YAML/JSON documents, ignoring everything else.
//
//   node keyorder.mjs a.yaml b.yaml     # exit 0 when every mapping's key sequence matches
//
// Why this exists, rather than a byte diff of the two tools' output:
//
// The obvious test is a fixed point -- format both openapi-format's output and the original with
// oxfmt, and expect identical bytes, so that emitter style cancels out. That reasoning assumes the
// formatter normalises style. oxfmt does not: it deliberately preserves the scalar style it is given,
// so openapi-format's re-emission (a folded `description: >-` becoming a different style, for
// instance) survives oxfmt and shows up as a byte difference that has nothing to do with ordering.
// Measured on the stripe corpus: the byte diff fails, while key order agrees at all 44,350 mappings.
//
// So the comparison has to be made on the thing actually being claimed. This walks both documents and
// compares the key sequence at every mapping, which isolates ordering from style exactly.
//
// One ordered parser reads both inputs, because YAML is a superset of JSON. `JSON.parse` would be
// wrong: JavaScript enumerates integer-like keys in ascending numeric order, silently reordering
// every `responses` block before the comparison could see it.

import fs from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

// `yaml` is not a dependency of this repository; it comes from the throwaway install `run.sh` makes.
//
// Resolved explicitly rather than with a bare `import`, because ESM resolves bare specifiers from the
// importing file's directory upward, and this file lives in the repo, not next to the install. A `cd`
// into the install directory does not help -- that is exactly how the sibling validate_reference.mjs
// came to fail for a maintainer running it by hand.
function requireFrom(dir, name) {
  return createRequire(join(dir, "package.json"))(name);
}

const roots = [process.env.OPENAPI_BENCH_NODE_DIR, process.cwd()].filter(Boolean);
let YAML;
for (const root of roots) {
  try {
    YAML = requireFrom(root, "yaml");
    break;
  } catch {
    /* try the next root */
  }
}
if (!YAML) {
  console.error(
    `cannot resolve the 'yaml' package from any of: ${roots.join(", ")}\n` +
      "Set OPENAPI_BENCH_NODE_DIR to a directory containing node_modules/yaml, " +
      "or run this through `tasks/benchmark/openapi/run.sh`, which installs it.",
  );
  process.exit(2);
}

// Keyed by path, not by position in a flat list. A list would turn one reordered mapping into a
// divergence at every later line, purely from index shifting: on the github corpus that inflated a
// single real difference into 1256 reported ones.
function trace(node, path = "$", out = new Map()) {
  if (YAML.isMap(node)) {
    const keys = node.items.map((item) => String(item.key?.value ?? item.key));
    // A repeated path can only come from duplicate keys, which specs do not have; disambiguate
    // anyway rather than silently dropping a mapping from the comparison.
    let key = path;
    for (let n = 2; out.has(key); n++) key = `${path}#${n}`;
    out.set(key, keys);
    node.items.forEach((item, i) => trace(item.value, `${path}.${keys[i]}`, out));
  } else if (YAML.isSeq(node)) {
    node.items.forEach((item, i) => trace(item, `${path}[${i}]`, out));
  }
  return out;
}

// JavaScript object property order: canonical array indices ascending, then the rest in insertion
// order. openapi-format round-trips documents through plain objects, so its `responses` blocks come
// back numerically ordered whatever the source said.
const isArrayIndex = (key) => /^(?:0|[1-9][0-9]*)$/.test(key) && Number(key) < 2 ** 32 - 1;
const indices = (keys) => keys.filter(isArrayIndex);
const strings = (keys) => keys.filter((key) => !isArrayIndex(key));
const isBucketed = (keys) => {
  const ints = indices(keys);
  const ascending = ints.every((key, i) => i === 0 || Number(ints[i - 1]) <= Number(key));
  // In JS order every integer key precedes every string key.
  const firstString = keys.findIndex((key) => !isArrayIndex(key));
  const lastInt = keys.reduce((last, key, i) => (isArrayIndex(key) ? i : last), -1);
  return ascending && (firstString === -1 || lastInt < firstString);
};

const load = (file) =>
  trace(YAML.parseDocument(fs.readFileSync(file, "utf8"), { merge: true }).contents);

const [left, right] = process.argv.slice(2);
if (!left || !right) {
  console.error("usage: keyorder.mjs <a> <b>");
  process.exit(2);
}

const a = load(left);
const b = load(right);
let identical = 0;
let integerOrderUnverifiable = 0;
let unexplained = 0;

for (const [path, ours] of a) {
  const theirs = b.get(path);
  if (theirs === undefined) {
    if (unexplained < 5) console.error(`missing on the other side: ${path}`);
    unexplained++;
    continue;
  }
  if (ours.join(",") === theirs.join(",")) {
    identical++;
    continue;
  }

  // Different key sets are never a mere ordering difference.
  if ([...ours].sort().join(",") !== [...theirs].sort().join(",")) {
    if (unexplained < 5) {
      console.error(`key set differs at ${path}\n  ours:   ${ours}\n  theirs: ${theirs}`);
    }
    unexplained++;
    continue;
  }

  // The only divergence this oracle is allowed to tolerate is the one its representation forces.
  //
  // What is not tolerated, and is checked here: the relative order of the string keys must match,
  // and their side must actually be in JavaScript property order. Both tools rank string keys by the
  // same table and comparator, so a real ordering bug shows up in that subsequence.
  //
  // What cannot be checked here: the relative order of the integer-like keys. openapi-format read the
  // document into a plain object, which enumerates them numerically, so its output carries no record
  // of what the source said. This is counted as unverifiable rather than explained: it is not
  // evidence of agreement, and calling it "explained" would have made the corruption of 873 mappings'
  // integer-key order pass silently, which is exactly what it did before this was split out.
  //
  // Our own integer-key handling is verified elsewhere, with an oracle that can answer it: the
  // differential test in `crates/oxc_openapi_order/tests/differential.rs` checks every mapping's order
  // against an independent model, and `validate_reference.mjs` checks that model against this same
  // package.
  const sameStringOrder = strings(ours).join(",") === strings(theirs).join(",");
  if (sameStringOrder && isBucketed(theirs)) {
    integerOrderUnverifiable++;
  } else {
    if (unexplained < 5) {
      const why = sameStringOrder ? "their side is not in JS property order" : "string-key order differs";
      console.error(`unexplained divergence at ${path} (${why})\n  ours:   ${ours}\n  theirs: ${theirs}`);
    }
    unexplained++;
  }
}
for (const path of b.keys()) {
  if (!a.has(path)) {
    if (unexplained < 5) console.error(`missing on our side: ${path}`);
    unexplained++;
  }
}

console.log(
  `mappings: ${a.size}; identical: ${identical}; ` +
    `integer-key order not verifiable against this oracle: ${integerOrderUnverifiable}; ` +
    `unexplained: ${unexplained}`,
);
process.exit(unexplained === 0 ? 0 : 1);

// Validates `reference.rs` — the Rust transcription of openapi-format's ordering pass — against
// the real package.
//
// `reference.rs` is the oracle the differential test compares against, so its fidelity is the one
// thing that test cannot check itself. This script closes that loop.
//
// Usage:
//   OPENAPI_ORDER_FUZZ_DUMP=/tmp/openapi-order-corpus \
//     cargo test -p oxc_openapi_order --test differential
//   node crates/oxc_openapi_order/tests/differential/validate_reference.mjs \
//     /tmp/openapi-order-corpus
//
// Requires openapi-format v1.33.6, resolved from OPENAPI_FORMAT_ROOT or the cwd. The corpus files
// each hold {"input": <document>, "expected": <what reference.rs produced>}; this script runs the
// real `openapiSort` over `input` and compares the full key-order trace. Any mismatch means the
// transcription has drifted and the differential test is measuring against the wrong oracle.
//
// The configuration is taken from the filename prefix, matching `configs()` in differential.rs.
// Only configurations whose option has a like-for-like spelling in the real package are dumped:
// `keyOrder` is excluded because upstream's `sortSet` replaces the whole table set instead of
// layering over it, and `components` because `sortComponentsSet` is a fixed list of names while the
// crate's option means "every member" -- a distinction that cannot be closed, since the arm itself
// creates member names (`"0"`, `"1"`, ...) when it rebuilds a sequence under `components`.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { createRequire } from "node:module";

// Resolved EXPLICITLY, from an env var first and only then the cwd.
//
// The cwd-only version of this failed for a maintainer: `require` resolves from the path given to
// `createRequire`, so it only worked when run from inside a directory that already had
// openapi-format installed, which nothing in the repo does. `just validate-openapi-reference` sets
// the variable; running it by hand from the install directory still works.
function loadOpenapiFormat() {
  const roots = [process.env.OPENAPI_FORMAT_ROOT, process.cwd()].filter(Boolean);
  for (const root of roots) {
    try {
      return createRequire(join(root, "package.json"))("openapi-format");
    } catch {
      /* try the next root */
    }
  }
  console.error(
    `cannot resolve 'openapi-format' from any of: ${roots.join(", ")}\n` +
      "Run `just validate-openapi-reference`, which installs it and sets OPENAPI_FORMAT_ROOT, or set\n" +
      "that variable to a directory containing node_modules/openapi-format@1.33.6.",
  );
  process.exit(2);
}

const { openapiSort } = loadOpenapiFormat();

const dir = process.argv[2];
if (!dir) {
  console.error("usage: validate_reference.mjs <corpus-dir>");
  process.exit(2);
}

const OPTIONS = {
  default: {},
  properties: { sortComponentsProps: true },
};

// The key-order trace of every mapping, keyed by path. Sequences contribute their elements.
function trace(value, path = "", out = []) {
  if (Array.isArray(value)) {
    value.forEach((item, index) => trace(item, `${path}/${index}`, out));
  } else if (value && typeof value === "object") {
    out.push([path, Object.keys(value)]);
    for (const key of Object.keys(value)) trace(value[key], `${path}/${key}`, out);
  }
  return out;
}

function sameTrace(a, b) {
  const left = trace(a);
  const right = trace(b);
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i++) {
    if (left[i][0] !== right[i][0]) return false;
    if (left[i][1].length !== right[i][1].length) return false;
    for (let k = 0; k < left[i][1].length; k++) {
      if (left[i][1][k] !== right[i][1][k]) return false;
    }
  }
  return true;
}

const files = readdirSync(dir).filter((name) => name.endsWith(".json")).sort();
if (files.length === 0) {
  console.error(`no corpus files in ${dir}; did the dump env var get set?`);
  process.exit(2);
}

let checked = 0;
let mismatched = 0;
const samples = [];

for (const name of files) {
  const config = name.slice(0, name.lastIndexOf("-"));
  if (!(config in OPTIONS)) continue;
  const { input, expected } = JSON.parse(readFileSync(join(dir, name), "utf8"));
  // `openapiSort` mutates nothing, but it does deep-copy; pass a fresh clone regardless.
  const actual = (await openapiSort(structuredClone(input), OPTIONS[config])).data;
  checked++;
  if (!sameTrace(actual, expected)) {
    mismatched++;
    if (samples.length < 5) {
      samples.push({ name, real: trace(actual), transcription: trace(expected) });
    }
  }
}

console.log(`corpus files:   ${files.length}`);
console.log(`checked:        ${checked}`);
console.log(`mismatched:     ${mismatched}`);
for (const sample of samples) {
  console.log(`\n--- ${sample.name}`);
  for (let i = 0; i < Math.max(sample.real.length, sample.transcription.length); i++) {
    const real = sample.real[i];
    const mine = sample.transcription[i];
    const equal = JSON.stringify(real) === JSON.stringify(mine);
    if (!equal) {
      console.log(`  real          ${JSON.stringify(real)}`);
      console.log(`  transcription ${JSON.stringify(mine)}`);
    }
  }
}

process.exit(mismatched === 0 ? 0 : 1);

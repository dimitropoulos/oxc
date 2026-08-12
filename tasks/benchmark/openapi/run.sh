#!/usr/bin/env bash
#
# Verification and benchmark harness for `sortOpenapi`, over four real OpenAPI specs.
#
#   tasks/benchmark/openapi/run.sh              # verify + benchmark every corpus
#   tasks/benchmark/openapi/run.sh --verify      # verification only, no timings (fast)
#   tasks/benchmark/openapi/run.sh --self-test   # prove each assertion can FAIL
#   ONLY=petstore,stripe tasks/benchmark/openapi/run.sh
#
# WHAT IS ASSERTED, and why it is key ORDER rather than bytes:
#
# On defaults openapi-format deletes every comment and bundles `$ref`s, so its output can never be
# byte-identical to a formatter's; `--no-bundle --keepComments` keeps the two inputs comparable in
# content. The tempting next step is a FIXED POINT -- format both its output and the original with
# oxfmt and expect identical bytes, on the reasoning that emitter style cancels out because oxfmt emits
# both sides. That assumes the formatter NORMALISES style, and oxfmt deliberately preserves it, so
# openapi-format's re-emission survives and the diff reports style rather than order.
#
# The assertion is therefore made on the key-order trace (`keyorder.mjs`), which isolates ordering
# exactly, and the byte fixed point is reported beside it as a diagnostic. One thing that comparison
# CANNOT establish is the relative order of integer-like keys, because openapi-format read the document
# through a JavaScript object and lost it; those mappings are counted as unverifiable rather than as
# agreement, and our own handling of them is verified by the differential test in
# `crates/oxc_openapi_order/`, whose oracle can answer it.
#
# Nothing here runs in CI: it needs the network, Node, and ~40 MB of specs. It is the evidence behind
# the numbers in the PR, and it is meant to be re-runnable by a maintainer from a clean checkout.

set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# An ABSOLUTE path to this script, for the self-test's re-invocations.
#
# `${BASH_SOURCE[0]}` alone is not usable as a command: invoked as `bash run.sh` it is the bare name
# `run.sh`, which bash resolves through PATH, so the re-invocation died with "command not found" and
# the self-test blamed its own fixture for a lookup failure. It is also always run as `bash "$SELF"`
# rather than executed, so it does not depend on the exec bit surviving a checkout.
readonly SELF="${SCRIPT_DIR}/$(basename "${BASH_SOURCE[0]}")"
readonly REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# OUTSIDE the repository, and that is forced rather than chosen.
#
# The specs total ~40 MB, so they must not be committed -- `tasks/common/src/test_file.rs` sets the
# precedent of downloading benchmark inputs on demand into `target/`. But oxfmt honours ignore rules,
# and every candidate in-repo location is gitignored, so it refuses to format there:
#
#   Expected at least one target file. All matched files may have been excluded by ignore rules.
#
# `--ignore-path /dev/null` does not override it. A gitignored directory therefore cannot host files
# this harness needs to run oxfmt over, and scratch data has no business in the tree anyway.
# Override with OXC_OPENAPI_BENCH_DIR if /tmp is small.
readonly WORK="${OXC_OPENAPI_BENCH_DIR:-${TMPDIR:-/tmp}/oxc-openapi-bench}"
readonly CORPUS_DIR="${WORK}/corpora"
readonly OUT_DIR="${WORK}/out"
readonly OXFMT="${REPO_ROOT}/target/release/oxfmt"

# openapi-format, and the directory its `yaml` dependency can be resolved from.
#
# Prefers an existing install: set OPENAPI_FORMAT to a binary and OPENAPI_BENCH_NODE_DIR to a
# directory containing `node_modules/yaml`, or just have `openapi-format` on PATH. Falls back to a
# throwaway npm install under the work directory, so a maintainer with neither still gets a run.
OPENAPI_FORMAT="${OPENAPI_FORMAT:-}"
NODE_DIR="${OPENAPI_BENCH_NODE_DIR:-${WORK}/node}"

# The 22.6 MB spec exhausts Node's default old-space.
readonly NODE_HEAP=8192

# name|url|expected_bytes
# The byte sizes are asserted after download: these are living documents, and a size change means the
# upstream spec moved on, so the numbers in the PR would no longer describe the same input.
readonly CORPORA=(
  "petstore|https://raw.githubusercontent.com/OAI/learn.openapis.org/main/examples/v3.0/petstore.yaml|2766"
  "stripe|https://raw.githubusercontent.com/stripe/openapi/master/openapi/spec3.yaml|6364174"
  "github|https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/api.github.com.yaml|9826433"
  "cloudflare|https://raw.githubusercontent.com/cloudflare/api-schemas/main/openapi.yaml|23695464"
  # Written by `--self-test`, never downloaded: an empty URL means "already present, no size check".
  "selftest||"
)

FAILURES=0
SKIPS=0
MODE="all"
case "${1:-}" in
  --verify) MODE="verify" ;;
  --self-test) MODE="self-test" ;;
  --help | -h)
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac

# Fault injection, used only by `--self-test`, which re-invokes this script with SELF_TEST_FAULT set
# and requires the run to FAIL. It lives on the real code path on purpose: a self-test that pokes at
# helper functions in isolation proves nothing about whether an assertion can fire.
inject_fault() {
  local stage="$1" target="$2" source="${3:-}"
  [[ ${SELF_TEST_FAULT:-} == "${stage}" ]] || return 0
  case "${stage}" in
    unsorted)
      # Pretend oxfmt did no ordering at all, which is the failure the whole harness exists to catch.
      #
      # Formatted from the SOURCE, not from `target`: `target` is already sorted, and re-formatting it
      # with the option off would leave it sorted and inject nothing. That mistake made this very case
      # report "not caught" until the injected file was inspected.
      local dir
      dir="$(mktemp -d)"
      printf '{ "sortOpenapi": false }\n' >"${dir}/.oxfmtrc.json"
      cp "${source}" "${dir}/spec.yaml"
      "${OXFMT}" --write "${dir}/spec.yaml" >/dev/null 2>&1
      cp "${dir}/spec.yaml" "${target}"
      ;;
    keyset) sed -i '/^openapi:/d' "${target}" ;;
    idempotent) printf '\n' >>"${target}" ;;
    comments) sed -i '/^[[:space:]]*#/d' "${target}" ;;
    probe) sed -i '/oxfmt-probe-leading/d' "${target}" ;;
    *)
      echo "unknown SELF_TEST_FAULT: ${stage}" >&2
      exit 2
      ;;
  esac
  info "self-test: injected fault '${stage}' into $(basename "${target}")"
}

section() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '   %s\n' "$*"; }
pass() { printf '   \033[32mPASS\033[0m %s\n' "$*"; }
fail() {
  printf '   \033[31mFAIL\033[0m %s\n' "$*" >&2
  FAILURES=$((FAILURES + 1))
}

# --- setup -------------------------------------------------------------------------------------

need() {
  command -v "$1" >/dev/null || {
    echo "missing required tool: $1${2:+ ($2)}" >&2
    exit 127
  }
}

setup() {
  need curl
  need node
  need cmp
  need diff

  mkdir -p "${CORPUS_DIR}" "${NODE_DIR}" "${OUT_DIR}"

  if [[ ! -x ${OXFMT} ]]; then
    # `--no-default-features` is not optional. `napi` is a DEFAULT feature, and a binary built with
    # it aborts on any real invocation ("External services must be set when `napi` feature is
    # enabled", walk_runner.rs) because the Node side is not there to install them.
    section "Building oxfmt (release, --no-default-features)"
    (cd "${REPO_ROOT}" && cargo build --release -p oxfmt --no-default-features)
  fi

  # Resolve openapi-format: explicit override, then PATH, then install one.
  if [[ -z ${OPENAPI_FORMAT} ]] && command -v openapi-format >/dev/null; then
    OPENAPI_FORMAT="$(command -v openapi-format)"
  fi
  if [[ -z ${OPENAPI_FORMAT} || ! -x ${OPENAPI_FORMAT} ]]; then
    section "Installing openapi-format@1.33.6"
    mkdir -p "${NODE_DIR}"
    (cd "${NODE_DIR}" && printf '{"name":"openapi-bench","private":true}\n' >package.json &&
      npm install --silent --no-audit --no-fund openapi-format@1.33.6 yaml)
    OPENAPI_FORMAT="${NODE_DIR}/node_modules/.bin/openapi-format"
  fi

  # PINNED to 1.33.6: the ordering this feature reproduces is that version's. A later release could
  # change it, and the mismatch would look like our bug rather than a version skew, so refuse rather
  # than silently measure against the wrong reference.
  local version
  # `|| true`: under `set -o pipefail` a binary that fails `--version` would make this assignment
  # non-zero and `set -e` would abort here, so the diagnostic below would never be reached.
  version="$("${OPENAPI_FORMAT}" --version 2>/dev/null | tr -d '\r' || true)"
  if [[ ${version} != "1.33.6" ]]; then
    echo "openapi-format must be 1.33.6, found '${version}' at ${OPENAPI_FORMAT}" >&2
    exit 3
  fi
  info "openapi-format ${version} (${OPENAPI_FORMAT})"

  # `keyorder.mjs` needs the `yaml` package, resolved from NODE_DIR (see its own header for why the
  # resolution is explicit rather than a bare import).
  if [[ ! -d ${NODE_DIR}/node_modules/yaml ]]; then
    mkdir -p "${NODE_DIR}"
    [[ -f "${NODE_DIR}/package.json" ]] ||
      printf '{"name":"openapi-bench","private":true}\n' >"${NODE_DIR}/package.json"
    (cd "${NODE_DIR}" && npm install --silent --no-audit --no-fund yaml)
  fi
  info "yaml package from ${NODE_DIR}"

  # Only the SELECTED corpora, so `ONLY=petstore` does not pull 40 MB to format 2.7 kB.
  for entry in "${CORPORA[@]}"; do
    IFS='|' read -r name url expected <<<"${entry}"
    selected "${name}" || continue
    local file="${CORPUS_DIR}/${name}.yaml"
    [[ -n ${url} ]] || continue # the self-test's synthetic corpus: present already, no size to check
    if [[ ! -f ${file} ]]; then
      info "downloading ${name}"
      curl -sSfL --retry 3 -o "${file}" "${url}"
    fi
    local actual
    actual=$(wc -c <"${file}" | tr -d ' ')
    if [[ ${actual} != "${expected}" ]]; then
      fail "${name}: expected ${expected} bytes, got ${actual} -- upstream spec has changed, so the
        PR's numbers no longer describe this input. Delete ${file} and re-read them."
    fi
  done
}

# Whether `name` is in the ONLY filter (all corpora when unset).
selected() {
  [[ -z ${ONLY:-} || ",${ONLY}," == *",$1,"* ]]
}

# --- measurements ------------------------------------------------------------------------------

# Lines whose first non-space character is `#`.
#
# Deliberately lexical, and it OVER-counts: a `#` line inside a block scalar (`description: |`) is
# content, not a comment, and these specs are full of Markdown that starts with `#`. It is still the
# right measure for a PRESERVATION check, because both sides are measured the same way and the number
# only has to be stable, not semantically exact. The synthetic probe below is what actually tests
# comment handling.
count_hash_lines() {
  # `grep -c` exits 1 when it matches nothing, which is not an error here. A missing file IS, though,
  # so it reports MISSING rather than becoming an empty string that compares equal to another one.
  [[ -r "$1" ]] || {
    printf 'MISSING'
    return
  }
  grep -cE '^[[:space:]]*#' "$1" || true
}

# `$ref` values that leave the document: anything whose target does not start with `#`.
#
# Written out rather than as one regex because the obvious regex is wrong: with `[[:space:]]*` before
# an optional quote, the "not a #" class happily matches the SPACE after the colon, so every internal
# `$ref: "#/components/..."` counts as external. The self-test caught exactly that.
count_external_refs() {
  awk '
    /\$ref:/ {
      value = $0
      sub(/^.*\$ref:[ \t]*/, "", value)
      first = substr(value, 1, 1)
      # Step over one opening quote, if present.
      if (first == "\"" || first == "\047") {
        value = substr(value, 2)
        first = substr(value, 1, 1)
      }
      if (first != "#" && first != "") { n++ }
    }
    END { print n + 0 }
  ' "$1"
}

# Format a copy in place. oxfmt has no `--stdout`, and `--stdin-filepath` parses but is unreachable in
# this build (it routes to WalkRunner, which only accepts Mode::Cli), so writing a temp copy is the
# only way to capture output.
#
# Returns oxfmt's status so the caller can REPORT a failure rather than have `set -e` kill the run
# mid-corpus with no summary — which is what happened on a document oxfmt could not parse.
oxfmt_to() {
  local src="$1" dest="$2"
  cp "${src}" "${dest}"
  "${OXFMT}" --write "${dest}" >"${dest}.oxfmt.log" 2>&1
}

peak_rss_kb() {
  # `/usr/bin/time -v` reports "Maximum resident set size (kbytes)". Bash's builtin `time` cannot, and
  # `/usr/bin/time` is a separate package on most distributions.
  #
  # Checked rather than assumed: without it the pipeline yields an EMPTY string and the caller happily
  # reports `peak RSS: kB`, which looks like a measurement and is not one.
  [[ -x /usr/bin/time ]] || {
    echo "MISSING(/usr/bin/time)"
    return
  }
  # The measured command's own status has to be checked, not just the parse: `time -v` prints its
  # resource block even when the command failed to start, so a crashed or OOM-killed run would
  # otherwise contribute a small, plausible-looking number to the PR.
  local kb status
  kb=$(/usr/bin/time -v "$@" 2>&1 >/dev/null | awk '/Maximum resident set size/ {print $NF}')
  status=${PIPESTATUS[0]}
  if [[ ${status} -ne 0 ]]; then
    printf 'FAILED(exit %s)' "${status}"
    return
  fi
  printf '%s' "${kb:-MISSING(unparsed)}"
}

# --- per-corpus verification -------------------------------------------------------------------

verify_corpus() {
  local name="$1" file="${CORPUS_DIR}/$1.yaml"
  local ref="${OUT_DIR}/${name}.reference.yaml"
  local ours="${OUT_DIR}/${name}.ours.yaml"
  local ref_fixed="${OUT_DIR}/${name}.reference.oxfmt.yaml"
  local twice="${OUT_DIR}/${name}.twice.yaml"

  section "${name} ($(wc -c <"${file}" | tr -d ' ') bytes)"
  info "source comment-like lines: $(count_hash_lines "${file}")"
  info "external \$ref count:      $(count_external_refs "${file}")"

  # openapi-format's ordering pass, with its two content-destroying defaults turned off so the only
  # difference left between the two inputs can be key order.
  #
  # Its banner goes to a log rather than the terminal, and the log is shown only if it fails -- a
  # silenced failure here would leave `${ref}` stale or empty and make the fixed-point check
  # meaningless.
  local log="${OUT_DIR}/${name}.openapi-format.log"
  if ! NODE_OPTIONS="--max-old-space-size=${NODE_HEAP}" \
    "${OPENAPI_FORMAT}" "${file}" --no-bundle --keepComments -o "${ref}" >"${log}" 2>&1; then
    fail "openapi-format failed on ${name}; log follows"
    tail -20 "${log}" >&2
    return
  fi
  [[ -s ${ref} ]] || {
    fail "openapi-format produced an empty output for ${name}"
    return
  }

  if ! oxfmt_to "${file}" "${ours}"; then
    fail "oxfmt failed to format ${name}; log follows"
    tail -10 "${ours}.oxfmt.log" >&2
    return
  fi
  inject_fault unsorted "${ours}" "${file}"
  inject_fault keyset "${ours}"

  # (1) THE assertion: the two tools must agree on key ORDER at every mapping.
  #
  # Measured on the key-order trace rather than on bytes. A byte-level fixed point --
  # oxfmt(openapi-format(x)) == oxfmt(x) -- is the tempting spelling, and it is WRONG here: it assumes
  # oxfmt normalises scalar style, whereas oxfmt deliberately preserves the style it is given. So
  # openapi-format's re-emission survives oxfmt and the diff reports style, not order. Measured: that
  # byte diff fails on stripe while key order agrees at all 44,350 mappings. See keyorder.mjs.
  local order_verdict
  if OPENAPI_BENCH_NODE_DIR="${NODE_DIR}" node "${SCRIPT_DIR}/keyorder.mjs" "${ours}" "${ref}" \
    >"${OUT_DIR}/${name}.keyorder.log" 2>&1; then
    pass "key order agrees with openapi-format"
    info "$(tail -1 "${OUT_DIR}/${name}.keyorder.log")"
    order_verdict="emitter style; key order asserted equal above"
  else
    fail "KEY ORDER DIFFERS from openapi-format for ${name}"
    cat "${OUT_DIR}/${name}.keyorder.log" >&2
    # Must not claim agreement in the diagnostic below when the assertion above just failed.
    order_verdict="key order ALSO differs -- see the failure above"
  fi

  # (1b) And the byte-level fixed point, reported rather than asserted, for the reason above. When it
  # DOES hold there is nothing left to explain; when it does not, (1) is what says the difference is
  # confined to emitter style.
  #
  # Deliberately AFTER (1) and tolerant of failure: this is the only step that needs oxfmt to parse
  # openapi-format's output, and on the cloudflare corpus it cannot. openapi-format re-emits the
  # source's quoted key `'... Further paths ...'` unquoted, and oxc's YAML parser rejects a plain
  # scalar beginning with `...` ("expected a node") even though YAML 1.2 only reserves `...` when it
  # is alone on a line. That is a pre-existing parser limitation, unrelated to key ordering -- the
  # ORIGINAL spec has the key quoted and formats fine -- so it must not be allowed to mask (1).
  if cp "${ref}" "${ref_fixed}" && "${OXFMT}" --write "${ref_fixed}" >"${OUT_DIR}/${name}.reformat.log" 2>&1; then
    if cmp -s "${ref_fixed}" "${ours}"; then
      info "byte fixed point also holds: oxfmt(openapi-format(x)) == oxfmt(x)"
    else
      info "byte fixed point differs (${order_verdict}): $(
        diff -u "${ours}" "${ref_fixed}" | grep -cE '^[+-]' || true
      ) changed lines"
    fi
  else
    SKIPS=$((SKIPS + 1))
    info "byte fixed point NOT MEASURED: oxfmt cannot re-read openapi-format's output --"
    info "  $(grep -m1 'Syntax error' "${OUT_DIR}/${name}.reformat.log" || echo 'see the log')"
  fi

  # (2) Idempotency: formatting our own output again must change nothing.
  oxfmt_to "${ours}" "${twice}" || true
  inject_fault idempotent "${twice}"
  if cmp -s "${ours}" "${twice}"; then
    pass "idempotent: oxfmt(oxfmt(x)) == oxfmt(x)"
  else
    fail "NOT idempotent for ${name}"
    diff -u "${ours}" "${twice}" | head -20 >&2 || true
  fi

  # (3) Comment preservation, counted the same way on both sides.
  local src_hash ours_hash ref_hash
  inject_fault comments "${ours}"
  src_hash=$(count_hash_lines "${file}")
  ours_hash=$(count_hash_lines "${ours}")
  ref_hash=$(count_hash_lines "${ref}")
  info "comment-like lines: source=${src_hash} oxfmt=${ours_hash} openapi-format=${ref_hash}"
  if [[ ${ours_hash} == "${src_hash}" ]]; then
    pass "oxfmt preserved all ${src_hash} comment-like lines"
  else
    fail "oxfmt changed comment-like line count: ${src_hash} -> ${ours_hash}"
  fi

  # (4) A synthetic probe, because the corpora carry almost no REAL comments -- nearly every `#`
  # above is Markdown inside a block scalar. Without this, "comments preserved" would be a claim
  # about content that no comment ever touched.
  local probe="${OUT_DIR}/${name}.probe.yaml" probe_out="${OUT_DIR}/${name}.probe.oxfmt.yaml"
  {
    echo "# oxfmt-probe-leading"
    cat "${file}"
  } >"${probe}"
  oxfmt_to "${probe}" "${probe_out}" || true
  inject_fault probe "${probe_out}"
  if grep -q '^# oxfmt-probe-leading$' "${probe_out}"; then
    pass "synthetic comment survived oxfmt"
  else
    fail "synthetic comment LOST by oxfmt"
  fi
  local probe_ref="${OUT_DIR}/${name}.probe.reference.yaml"
  NODE_OPTIONS="--max-old-space-size=${NODE_HEAP}" \
    "${OPENAPI_FORMAT}" "${probe}" --no-bundle -o "${probe_ref}" >>"${log}" 2>&1 || true
  if grep -q 'oxfmt-probe-leading' "${probe_ref}"; then
    info "openapi-format kept the probe (unexpected on defaults)"
  else
    info "openapi-format dropped the probe on defaults, as documented"
  fi
}

# --- per-corpus timings ------------------------------------------------------------------------

bench_corpus() {
  local name="$1" file="${CORPUS_DIR}/$1.yaml"
  local scratch="${OUT_DIR}/bench-${name}.yaml"

  section "${name}: timings"
  need hyperfine "cargo install hyperfine"

  # `--write` needs UNFORMATTED input on every iteration, or from the second run onward it would be
  # timed on already-sorted bytes. The re-copy therefore goes in `--prepare`, which hyperfine runs
  # OUTSIDE the measured interval: putting it inside the timed command charges oxfmt for a file copy
  # its competitor never pays, which is 23% of the petstore figure and ~3% of the large ones.
  #
  # `--check` is timed on an already-formatted copy, which is the realistic case (CI checking a
  # formatted tree). Pointing it at unformatted input would exit 1 -- correctly, the file does need
  # formatting -- and hyperfine aborts on a non-zero exit.
  #
  # NOTE the two oxfmt rows are not comparable with the openapi-format row on equal terms: openapi-
  # format is always given unformatted input and always writes a new file. hyperfine's own Summary
  # block cross-compares all three regardless, so it is suppressed with `--style basic` and the
  # per-command means are what should be quoted.
  local formatted="${OUT_DIR}/bench-${name}.formatted.yaml"
  oxfmt_to "${file}" "${formatted}" || fail "oxfmt failed on ${name}; --check timing will be wrong"

  hyperfine \
    --warmup 1 --min-runs 3 \
    --command-name "openapi-format" \
    "NODE_OPTIONS=--max-old-space-size=${NODE_HEAP} '${OPENAPI_FORMAT}' '${file}' --no-bundle --keepComments -o '${OUT_DIR}/hf-${name}.yaml'" \
    --prepare "cp '${file}' '${scratch}'" \
    --command-name "oxfmt --write (unformatted input)" \
    "'${OXFMT}' --write '${scratch}'" \
    --command-name "oxfmt --check (formatted input)" \
    "'${OXFMT}' --check '${formatted}'" \
    --export-json "${OUT_DIR}/hyperfine-${name}.json" || fail "hyperfine failed for ${name}"

  cp "${file}" "${scratch}"
  local rss_ours rss_ref
  rss_ours=$(peak_rss_kb "${OXFMT}" --write "${scratch}")
  rss_ref=$(NODE_OPTIONS="--max-old-space-size=${NODE_HEAP}" peak_rss_kb \
    "${OPENAPI_FORMAT}" "${file}" --no-bundle --keepComments -o "${OUT_DIR}/rss-${name}.yaml")
  info "peak RSS: oxfmt=${rss_ours} kB  openapi-format=${rss_ref} kB"
}

# --- self-test ---------------------------------------------------------------------------------

# Proves each assertion FAILS when it should. A harness that cannot fail is worse than none: it
# reports success whether or not the tools agree.
#
# Every case re-invokes this script for real, with SELF_TEST_FAULT set, and requires a non-zero exit
# and the expected FAIL line. Nothing here inspects a helper in isolation: an earlier version of this
# self-test did exactly that, and it still reported "5/5 assertions demonstrated able to fail" after
# the load-bearing key-order comparison had been replaced by `exit 0`.

# A synthetic corpus, so the faults land on something small and so the fixture itself carries the
# shapes the assertions need: comments to preserve, keys out of canonical order, and two status codes.
readonly SELF_TEST_CORPUS="selftest"
write_self_test_corpus() {
  cat >"${CORPUS_DIR}/${SELF_TEST_CORPUS}.yaml" <<'YAML'
# a leading comment, which must survive
openapi: 3.0.0
paths:
  /b:
    get:
      responses:
        "404": { description: gone }
        "200": { description: ok }
      operationId: b
      summary: second
  /a:
    get:
      responses: {}
      operationId: a
info:
  title: self-test
  version: "1"
YAML
}

self_test() {
  section "Self-test: every assertion must be able to fail"
  write_self_test_corpus

  # Being unable to re-invoke myself is a different fault from the fixture failing, and must not be
  # reported as the latter: every case below is a child run, so a lookup failure would otherwise read
  # as five caught faults or as a broken fixture.
  local checked=0 caught=0
  if [[ ! -r ${SELF} ]]; then
    fail "cannot re-invoke myself: ${SELF} is not readable, so no assertion below can be exercised"
    return
  fi

  # The fixture must PASS clean, or every failure below could be the fixture rather than the fault.
  local clean_status=0
  ONLY="${SELF_TEST_CORPUS}" SELF_TEST_FAULT="" bash "${SELF}" --verify \
    >"${OUT_DIR}/selftest-clean.log" 2>&1 || clean_status=$?
  if [[ ${clean_status} -eq 0 ]]; then
    pass "the self-test fixture passes clean (so a failure below is the injected fault)"
  elif [[ ${clean_status} -eq 126 || ${clean_status} -eq 127 ]]; then
    fail "cannot re-invoke myself (exit ${clean_status}); the fixture was never actually run"
    tail -5 "${OUT_DIR}/selftest-clean.log" >&2
    return
  else
    fail "the self-test fixture does NOT pass clean (exit ${clean_status}); the cases below prove nothing"
    tail -20 "${OUT_DIR}/selftest-clean.log" >&2
    return
  fi

  # fault name | the assertion it must break
  local cases=(
    "unsorted|key order agrees"
    "keyset|key order agrees"
    "idempotent|idempotent"
    "comments|comment-like line count"
    "probe|synthetic comment"
  )
  for entry in "${cases[@]}"; do
    IFS='|' read -r fault expect <<<"${entry}"
    checked=$((checked + 1))
    local log="${OUT_DIR}/selftest-${fault}.log" status=0
    ONLY="${SELF_TEST_CORPUS}" SELF_TEST_FAULT="${fault}" bash "${SELF}" --verify \
      >"${log}" 2>&1 || status=$?
    if [[ ${status} -eq 0 ]]; then
      fail "fault '${fault}' did NOT make the run fail -- the assertion on '${expect}' cannot fire"
    elif [[ ${status} -eq 126 || ${status} -eq 127 ]]; then
      # A non-zero exit that is not an assertion failure must not be counted as a caught fault.
      fail "fault '${fault}': could not re-invoke myself (exit ${status}), so nothing was exercised"
    elif grep -q "FAIL" "${log}"; then
      caught=$((caught + 1))
      pass "fault '${fault}' caught: $(grep -m1 'FAIL' "${log}" | sed 's/.*FAIL[^ ]* //')"
    else
      fail "fault '${fault}' made the run exit non-zero but reported no FAIL; see ${log}"
    fi
  done

  info "self-test: ${caught}/${checked} faults were caught by a real assertion"
  [[ ${caught} == "${checked}" ]] || fail "self-test incomplete"
}

# --- main --------------------------------------------------------------------------------------

main() {
  setup

  local filter="${ONLY:-}"
  local names=()
  for entry in "${CORPORA[@]}"; do
    IFS='|' read -r name _ _ <<<"${entry}"
    if selected "${name}"; then names+=("${name}"); fi
  done
  [[ ${#names[@]} -gt 0 ]] || {
    echo "no corpora selected (ONLY=${filter})" >&2
    exit 2
  }

  if [[ ${MODE} == "self-test" ]]; then
    self_test
  else
    for name in "${names[@]}"; do verify_corpus "${name}"; done
    if [[ ${MODE} == "all" ]]; then
      for name in "${names[@]}"; do bench_corpus "${name}"; done
    fi
  fi

  section "Summary"
  [[ ${SKIPS} -eq 0 ]] || info "${SKIPS} diagnostic(s) not measured -- see the NOT MEASURED lines above"
  if [[ ${FAILURES} -eq 0 ]]; then
    pass "all assertions held"
  else
    fail "${FAILURES} assertion(s) failed"
    exit 1
  fi
}

main

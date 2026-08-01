# Whole-codebase maintainability audit: final ranked synthesis

## Snapshot and coverage

- Final synthesis snapshot: 2026-07-28 19:45:47 +08:00.
- Branch: `main`.
- HEAD: `0c62febd2b6afdd1800da1591b68f7a600a3835e`
  (`docs(audio): record benchmark, playback, and resampler evidence`).
- Reviewed inventory: 82 Rust files and 55,258 logical lines across `src/`,
  `tests/`, `benches/`, and `examples/`, plus public documentation, CI, and the
  backend Trellis specifications.
- The final working tree still contained concurrent edits in `CHANGELOG.md`,
  `README.md`, `src/lib.rs`, `src/pipeline.rs`,
  `src/processor/lockfree_params.rs`, `src/processor/mod.rs`, and
  `src/processor/traits.rs`. The audit did not modify those files.
- Their final mtimes were all earlier than the area 06 source re-read. A final
  content recheck confirmed that none of the area 01/02 findings in the moving
  playback facade had been fixed or otherwise superseded. The area 06 snapshot
  recorded `src/processor/mod.rs` as 15:41:20; its actual final mtime was
  13:42:06. That metadata typo does not affect any source conclusion.
- Detailed evidence remains split by domain in the sibling `00` through `06`
  research documents. This report consolidates related findings; it does not
  replace their exact evidence, consequences, test gaps, or non-findings.

The eight area documents contain 78 confirmed finding groups after assigning
mixed severities to their higher-risk path: 17 P1 or P1-on-affected-path
groups, 35 P2 groups, and 26 P3 groups. Several documentation findings mirror
an underlying code or benchmark defect, so these numbers measure reviewable
findings rather than 78 independent runtime bugs.

## Direct verdict

The codebase is **not a uniform "spaghetti code" system**. Its central
streaming contract, realtime prohibitions, processor composition, convolver
ownership, resampler engines, and benchmark support have recognizable owners,
strong invariants, and unusually good focused tests. Much of the apparent
complexity is required by allocation-free callbacks, variable input/output
progress, latency/tail preservation, DSP continuity, or independent numerical
oracles. Removing those mechanisms merely to reduce types or line count would
make the engine less correct.

It nevertheless has serious maintainability hotspots. The strongest one is a
split public boundary: the canonical facade and checked block APIs are safer
than adjacent exported raw processors, decoder/source types, legacy configs,
and one-shot helpers. Callers can receive `Ok` for a discarded update, mutate
decoder invariants directly, trigger callback panics through accepted geometry,
or get different defaults and validation depending on the entry point. Remote
input handling also lacks an adequate trust boundary, and two advertised
quality gates can report more assurance than they actually provide.

In practical terms:

- The realtime/DSP core is mostly **complex but disciplined**.
- External-input, public-API, and lifecycle validation boundaries contain
  **current correctness and security defects**.
- Legacy surface, duplicated defaults/ranges/mappings, and cross-file field
  copies are the clearest **long-term "code smell" traits**.
- Public docs and Trellis are broad but are no longer a coherent executable
  source of truth.
- The repository is behaviorally well tested. Both full feature matrices,
  both Clippy matrices, formatting, diff hygiene, doctests, and support tests
  passed on the final dirty playback snapshot during Phase 2.2.

## P1 findings: correctness, security, and false assurance

The following order reflects likely impact and remediation dependency, not file
order. Detailed reproductions and exact surrounding contracts are in the linked
area documents.

| Rank | Finding | Consequence | Primary evidence |
|---:|---|---|---|
| 1 | HTTP Range responses are trusted without requiring 206, validating `Content-Range`, or bounding the returned body. | A server can bypass the streaming memory budget, exhaust memory, or supply bytes from the wrong offset after a seek. | `src/decoder/source.rs:342-386`, `:509-583`; [area 03b](03b-decoder-and-runtime-modules.md) |
| 2 | Credentials and complete signed URLs are printable. | Plaintext passwords, query tokens, or URL userinfo can enter logs, crash output, and issue reports. | `src/decoder/source.rs:28-36`, `:137-149`; [area 03b](03b-decoder-and-runtime-modules.md) |
| 3 | Unsupported positioned channels are discarded and then guessed from channel count. | Height/discrete channels can be confidently relabelled as rear speakers, corrupting loudness weighting and downmix. | `src/decoder/streaming.rs:615-659`, `src/channel_layout.rs:139-181`; [area 03b](03b-decoder-and-runtime-modules.md) |
| 4 | AutoMix Full mode omits the tail when one analysis window is shorter than the track but two windows would overlap. | Fade, cut, and mix positions can be reported near the first-window boundary while program material continues. | `src/processor/automix_analysis.rs:277-297`, `:398-459`, `:532-543`; [area 03a](03a-dsp-and-analysis-modules.md) |
| 5 | `DynamicLoudnessProcessor::reset` clears the applied compensation but leaves the cached publication generation unchanged. | The next stream can silently run without the still-published compensation until another control write occurs. | `src/processor/adapters.rs:1765-1789`, `:1833-1836`, `src/processor/dynamic_loudness.rs:667-678`; [area 03a](03a-dsp-and-analysis-modules.md) |
| 6 | Exported raw DSP processors accept zero/mismatched geometry that their adapters reject. | Direct callback users can divide by zero or index beyond setup-sized state; the facade path is safer but the public contract is split. | `src/processor/dsp.rs:90-97`, `:547-565`, `dynamic_loudness.rs:423-450`, `:619-625`, `loudness/limiter.rs:177-276`, `loudness/normalizer.rs:30-53`, `:234-242`, `spectrum.rs:21-96`; [area 03a](03a-dsp-and-analysis-modules.md) |
| 7 | `decode_all` and the memory-budget resolver use unchecked conversions and multiplications over untrusted duration/channel values. | Malformed metadata or a 32-bit target can turn a typed size rejection into panic, wrapped estimates, or an uncontrolled allocation attempt. | `src/decoder/streaming.rs:478-503`, `src/diagnostics.rs:3-47`; [area 03b](03b-decoder-and-runtime-modules.md) |
| 8 | Unsupported-target audio-thread initialization logs from the callback. | On affected architectures, the documented no-lock/no-allocation realtime boundary is violated during initialization. | `src/runtime.rs:6-18`, `:67-70`; [area 03b](03b-decoder-and-runtime-modules.md) |
| 9 | `PlaybackParameters::set_eq_band_gain_db` returns `Ok(())` for an out-of-range band while the low layer silently drops it. | Integrations can persist or display a successful edit that never took effect. | `src/pipeline.rs:698-703`, `src/processor/lockfree_params.rs:500-509`; [area 01](01-public-api-and-control-boundaries.md) |
| 10 | One `set_saturation_gains_db` call performs two separately observable publications. | The callback can see a mixed new-input/old-output gain pair despite the facade's complete-snapshot promise. | `src/pipeline.rs:789-804`, `src/processor/lockfree_params.rs:791-810`; [area 01](01-public-api-and-control-boundaries.md) |
| 11 | Invalid `ChainFinishPolicy` is accepted by `PlaybackBuilder::build` and first rejected during callback-side drain. | Deterministic preset errors are deferred to the realtime lifecycle path, contradicting strict build-time validation. | `src/pipeline.rs:540-644`, `src/processor/dsp_chain.rs:63-79`, `:208-219`; [area 02](02-pipeline-and-chain-boundaries.md) |
| 12 | Gapless benchmark enforcement moves a correctness failure into `skipped` and can still pass when another fixture succeeds. | An advertised `--enforce` command can return green while omitting an attempted fixture's failed correctness result. | `benches/audio_gapless_comparison_perf.rs:216-225`, `:293-303`, `:979-996`; [area 05](05-tests-and-benchmarks.md) |
| 13 | Trellis release checklists require backend-less and service-feature-only builds that the crate intentionally rejects. | An agent following the executable spec cannot make the required matrix green without violating the supported backend invariant. | `.trellis/spec/backend/quality-guidelines.md:1097-1125`, `:1162-1171`, `src/processor/resampler/mod.rs:16-20`; [area 06](06-documentation-and-spec-drift.md) |
| 14 | `docs/quality.md` states that performance `--enforce` always proves complete work and report integrity. | Readers can treat both the gapless false-green path and the nonstandard lock-free probe as stronger evidence than they are. | `docs/quality.md:46-50`, `:19`, `:110-114`; [areas 05](05-tests-and-benchmarks.md) and [06](06-documentation-and-spec-drift.md) |
| 15 | Four public `src/config.rs` effect configs are orphaned duplicates of the playback facade. | Public callers can choose a configuration model with no production consumer and already-drifted behavior. | `src/config.rs:111-190`, `src/pipeline.rs:282-471`; [area 04](04-legacy-and-duplication.md) |
| 16 | Crossfeed default mix is `0.3`, `0.35`, or `0.5` depending on entry path. | Identically described defaults produce different audio and demonstrate that the duplicated configuration owners have already diverged. | `src/config.rs:169`, `src/processor/crossfeed.rs:18`, `src/pipeline.rs:373`; [area 04](04-legacy-and-duplication.md) |
| 17 | DSP cores re-encode public clamp ranges as literals, and core saturation gain setters do not enforce the published bounds. | A range change can be silently re-clamped by stale core code; direct saturation users can bypass the facade's +/-24 dB contract. | `src/processor/lockfree_params.rs:40-95`, `eq.rs:115`, `saturation.rs:383-440`, `crossfeed.rs:247-253`, `dsp.rs:80`; [area 04](04-legacy-and-duplication.md) |

Items 13 and 14 are executable-document/evidence defects rather than audio
runtime failures, but they remain P1 because they can direct future work toward
unsupported builds or authorize a false release/performance conclusion.

## P2 boundary debt

These findings are less immediately destructive than the P1 set, but together
they explain why the code can feel difficult to change: capability, ownership,
validation, and error policy are not consistently located at one boundary.

### 1. Processor capability is broader than schedulers can honor

- `StreamingProcessor::set_enabled` promises transparent bypass, but volume is
  always on and resampling is graph geometry; both implementations silently
  ignore the operation (`traits.rs:689-701`, `adapters.rs:1499-1580`,
  `resampler/mod.rs:1128-1202`).
- `DspChain::add` accepts every `StreamingProcessor`, although it only drives a
  fixed in-place topology. An unequal-rate `StreamingResampler` is therefore
  type-correct to add and fails only during processing
  (`dsp_chain.rs:150-188`, `resampler/mod.rs:1133-1148`).
- `DspChain::new`/`with_capacity` accept zero and do not establish that each
  processor shares the chain rate; later mutation rejects zero and timing can
  silently degrade to zero/unknown (`dsp_chain.rs:114-153`, `:493-542`).

This is a capability-model problem, not a request to make the callback chain
allocate variable-rate scratch. Narrow the accepted processor class or expose
explicit fixed-in-place and bypass capabilities.

### 2. Callback, offline, and single-consumer ownership are mixed

- `OutputChainParams` requires `source_sample_rate` for callback construction,
  then the callback explicitly ignores it and uses only the output/device rate
  (`output_chain.rs:1329-1402`).
- Cloneable builders borrow `&self`, but their embedded convolver control can
  have only one live audio consumer. The lease is correct; the builder's reuse
  semantics are weaker than `Clone` suggests.
- A test named as proof of lease release fails before acquiring the lease, so
  its evidence is narrower than its name (`output_chain/tests.rs:72-84`).
- `PlaybackController` owns lifecycle/convolver authority but also proxies an
  unexplained subset of ordinary `PlaybackParameters` operations
  (`pipeline.rs:948-1027`).

Separate callback-only geometry from offline source boundaries, and make
single-consumer versus cloneable-control semantics explicit in the API shape.

### 3. Standalone DSP validation does not share the facade policy

- Direct EQ, saturation, volume, FIR EQ, and limiter setters accept non-finite
  values that the facade/atomic publication boundary rejects or retains.
- `LoudnessMeter` can suppress initialization failure and later report a
  reliable measurement using only elapsed sample count.
- `AtomicDynamicLoudnessParams::set_ref_volume_db` performs a whole-snapshot
  read before writer serialization and can overwrite a concurrent partial
  update (`lockfree_params.rs:1268-1280`).

The fix direction is one shared validated parameter policy with clearly named
unchecked kernels kept private. Merely adding another facade check would leave
the split public surface intact.

### 4. AutoMix combines inconsistent scopes and weak result types

- Loudness/true-peak receives a complete decoder packet before the bounded
  analysis loop truncates the other metrics to `max_frames`.
- Public analysis returns `Result<_, String>`, erasing cancellation, decoder,
  seek, and I/O classes.
- The key DTO reserves four optional payloads while its only status is
  `Unsupported`, allowing contradictory externally constructed states.
- The energy profile allocates by declared full duration even though only
  bounded head/tail evidence exists.

These are one ownership theme: the analysis result does not encode which
interval and capability produced each value.

### 5. Decoder source and error identity is string- and path-driven

- Local versus HTTP selection uses `AsRef<Path>`, lossy string conversion, and
  repeated case-sensitive prefix checks rather than a typed media location.
- Range initialization falls back from almost every error and may repeat HEAD;
  steady-state reads lose retry/cancellation/network classification through
  `io::Error` and Symphonia string conversion.
- Error variants conflate unsupported format, truncated input, unseekable
  sources, feature-disabled HTTP, and transport failures.
- `DecodeCancelToken` exposes `Arc<AtomicBool>` construction instead of owning
  the cancel protocol.

A typed `MediaLocation` and preserved structured source error are the natural
boundary. They would also centralize URL redaction and cache identity.

### 6. Public metadata is also mutable decoder control state

`StreamingDecoder` and its builder expose mutable `AudioInfo`, then trust those
fields for staging geometry, gapless counters, allocation, seek math, and
position. Observation data is therefore an unvalidated control channel
(`decoder/streaming.rs:33-103`, `:368-410`, `:478-503`, `:565-590`). Keep
operational geometry private and expose an immutable DTO; test-only mutations
belong behind crate-private fixtures.

### 7. Loudness cache has no stable source-identity/freshness contract

- Newer scanner versions, missing/unreadable local files, and every HTTP URL
  can be treated as fresh.
- Whole-second mtime plus size can miss rapid replacements.
- Windows track IDs lowercase URL paths/queries and do not canonicalize local
  paths, producing both collisions and duplicates.
- Database operations return strings, and `get_outdated_tracks` silently drops
  row-decoding errors.

The cache needs distinct local and remote identities, exact version matching,
explicit unknown/unreadable outcomes, and typed errors or explicit partial
results.

### 8. Resampler engine contracts are hidden behind a weak public facade

- The one-shot API accepts zero geometry on equal-rate bypass and truncates an
  incomplete final frame on unequal rates, while streaming rejects both.
- SoXR maps public `Standard` and `High` to the same recipe, but benchmarks
  present them as separate quality levels.
- The output-chain finish bound uses a process-output estimate and remains
  correct only because private construction fixes linear phase.
- Three public sizing helpers mix current-call estimates, backend-step claims,
  whole-stream expectations, magic `64` margins, and unchecked arithmetic.
- One-shot multi-mono output pads/truncates channel divergence that streaming
  correctly treats as an invariant failure.

Keep the specialized engines. Tighten the facade around validated geometry,
resolved backend recipes, exact units, checked arithmetic, pending state, and
timing-derived finish bounds.

### 9. Legacy surface remains public without a lifecycle policy

`RingBuffer`, `VolumeController`, the test-oracle `PolyphaseResampler`, the
compatibility `ConvolverControl::publish`, four orphan effect configs, several
settings/types/constants, and a benchmark-only FIR EQ remain exported without
deprecation or a documented support status. Legitimate downstream use is
possible, but the repository neither exercises nor identifies much of this
surface. The result is a larger compatibility burden and multiple apparent
owners for the same capability.

### 10. State representations require synchronized manual edits

- Saturation/noise-shaper enums and their atomic `u8` encodings live in
  separate match tables with fallback-to-default behavior.
- Saturation config crosses a facade config, snapshot, adapter cache, repeated
  setter list, and core copy/settings structures through hand-written fields.
- Snapshot `Default`s often mean effect-on while facade defaults mean off.
- Dynamic loudness telemetry repeats a fixed `[f64; 7]` rather than the model
  constant.

These are precisely the conditions under which a new field or variant compiles
while silently failing to reach audio. Consolidate representations where the
realtime copy contract permits it and add exhaustive conversion tests where it
does not.

### 11. Benchmark authority is uneven outside the shared harness

- Gapless accepts but ignores shared baseline flags.
- `audio_lockfree_params_perf` has one wall-clock sample, no JSON/environment/
  case/work identity, an unstable fixed 3% assertion, and no CI execution.
- Ten ordinary probes have private baseline/enforcement branches with no
  synthetic tests; routine CI exercises only their no-baseline path.

This is not a reason to replace the custom harnesses with Criterion. Move small
pure comparison/enforcement code into testable support modules and state which
probes are evidence gates versus exploratory measurements.

### 12. The playback facade has no Trellis-owned contract

`PlaybackPipeline::process`, its lifecycle channel, idle-silence exception,
control coalescing, range rules, convolver authority, and realtime
prohibitions live in rustdoc/changelog but not in an owning backend spec. Older
file-wide specs still describe `pipeline.rs` as a RingBuffer module and allow
logging generically in a file that now contains callback code. This makes the
highest-level recommended API especially vulnerable to a future well-intended
spec-driven regression.

## P3 maintainability and naming smells

These are real cleanup opportunities but should follow the correctness and
boundary work above.

| Theme | Current evidence and maintenance cost |
|---|---|
| Positional/incomplete readback | `PlaybackParameters` returns tuples of indistinguishable `f64`s; `saturation()` omits much of writable saturation state and calls latest publication "applied" (`pipeline.rs:904-935`). |
| Inaccurate error/name choices | Fade duration uses `InvalidGeometry`; `PeakLimiter::set_threshold` accepts dB; `MonoBackend` owns multichannel engines; active SoXR scratch is named `legacy_*`; `DspChain` prefixes used sample-rate parameters with `_`. |
| Alias surface without distinct behavior | Convolver `process_*` and `try_process_*` pairs are equally fallible; limiter threshold aliases duplicate bodies; several public types have no non-test consumer. |
| Stage-registration fan-out | The canonical output manifest is valuable, but a stage addition still touches callback arms, offline fields/constructors, processing, reset, finish, sample-rate, and timing macros. |
| Repeated lifecycle reset bookkeeping | `DspChain` clears overlapping finish-state field sets in reset, sample-rate change, clear, and stage transitions. A field addition must be remembered at each site. |
| Repeated units/helpers | dB conversions, tail policies in frames versus milliseconds, limiter defaults, sample-rate literals, and one RBJ coefficient implementation have multiple owners with slightly different edge behavior. |
| Overbuilt small cache | `TrackLoudness` embeds two `Cell`s for one off-RT `powf`, making a data-record type non-`Sync` for an unproven benefit. |
| Benchmark metadata duplication | Nine probes repeat baseline-reference structs and report headers; artifact round-trip validation varies by probe and one global schema version covers unrelated shapes. |
| Fixture/test duplication | A callback signal/IR corpus is copied into output-render benchmarking; adjacent resampler quality tests repeat local tone helpers; legacy RingBuffer and facade tests share one large inline namespace. |
| Stale ticket archaeology | Production comments such as `FIX for Defect 30/33/36`, `P1-5`, and `MINOR-03` describe old work items instead of durable current invariants. |
| Repository debris | Four unrelated untracked cache-TTL Markdown files plus `.pi-subagents/` and `.tmp/` are present at the root. They were preserved and must remain outside a crate commit unless the user decides otherwise. |

Lower-confidence naming questions remain explicitly non-findings: notably
whether `DownmixCoefficients::AtscA85` should say `Inspired`, and whether the
default callback finish policy intentionally has fixed frame work rather than
fixed wall-clock duration across sample rates.

## Documentation and specification drift

Documentation is extensive and often high quality, but it currently describes
several incompatible generations of the architecture as if all were current.

| Severity | Drift | Consequence |
|---|---|---|
| P1 | Release specs require backend-less feature matrices. | Mandatory checks cannot pass under the intentional backend contract. |
| P1 | Public quality prose overstates all `--enforce` paths. | A green command can be mistaken for complete correctness/report evidence. |
| P2 | Playback facade/lifecycle has no owned realtime or lifecycle scenario. | Generic processor rules and file-wide logging guidance can be misapplied. |
| P2 | README says the crate both recommends a high-level playback facade and is unsuitable for a high-level playback API. | "Playback" ambiguously means DSP callback control in one place and complete player/device orchestration in another. |
| P2 | Error spec says `UnsupportedFormat` is never constructed and omits new facade errors. | Completed work remains listed as a gap, while current variants lack an executable contract. |
| P2 | `docs/quality.md` describes the retired all-spectral nonlinear Rubato route and v2 benchmark identity. | The dominant 147:160 route and current v4 baseline identity are misrepresented. |
| P2 | README/examples say no optional features are needed and hard-code SoX VHQ. | Bare `--no-default-features` actually fails; Rubato-only runs the same backend-neutral examples. |
| P2 | CONTRIBUTING says five listed quick commands are four and that all checks run on three operating systems. | Contributors cannot tell which gates have platform or baseline evidence. |
| P3 | Directory spec omits current modules/resampler tree and still labels `pipeline.rs` as RingBuffer. | New work is routed toward obsolete ownership boundaries. |
| P3 | Living docs quote about 7 ns and about 13 ns for lock-free reads without a versioned artifact. | A noisy, non-JSON microbenchmark cannot support one traceable current value copied across documents. |

README doctest inclusion is a good anti-drift mechanism: all six doctests pass
under all-features and Rubato-only matrices. The drift above is semantic and
architectural, not broken code blocks.

## Complexity that is justified and should be retained

The following mechanisms were inspected specifically to avoid misclassifying
necessary audio-engine complexity as over-design:

1. **Packed lifecycle command channel.** One atomic word keeps request kind,
   fade payload, and generation coherent while requests coalesce at callback
   block boundaries. It solves a real `&mut PlaybackPipeline` ownership problem
   without callback locks or allocation.
2. **Pre-registered realtime snapshot readers.** Control-side Arc ownership and
   reclamation stay off the audio thread while callback readers copy bounded
   snapshots through hazard slots. Conventional locks/ArcSwap guards would
   violate the project's strongest invariant.
3. **Streaming finish/tail state machine.** It must preserve upstream tails
   through downstream processors, honor backpressure, separate latency from
   semantic tail, observe energy before dither, and bound unknown tails.
4. **Canonical output-stage manifest.** It meaningfully derives callback and
   offline order and supports parity tests. Residual declaration fan-out is a
   cost to reduce, not a reason to delete the manifest.
5. **Convolver consumer lease and ownership slots.** They prevent heavy kernel
   destruction or ambiguous ownership on the audio thread. Overlap-save and
   partitioned engines serve different short/long-IR latency-cost envelopes.
6. **EQ transition banks, saturation quality banks, and ramps.** Their state
   preserves filter-history ownership, automation continuity, fixed latency,
   and allocation-free quality changes.
7. **True-peak limiter structures.** Preallocated lookahead, FIR true-peak
   detection, and a monotonic maximum queue implement intersample protection
   with bounded callback work.
8. **Staged decoder construction and multiple decode APIs.** Open/probe/build
   exposes exact staging ownership without reopening media; borrowed,
   append-into, packet-owned, and decode-all APIs have distinct allocation
   contracts.
9. **Exclusive gapless ownership.** Codec allowlisting, seek timestamps, true
   stream start/end, and fallback delay/padding state prevent double trimming.
10. **HTTP Range adapter.** A bounded seekable adapter is the right abstraction
    for Symphonia. Its response validation and error preservation are broken;
    the abstraction itself is not needless.
11. **Specialized resampler engines and fixed rings.** Half-band, FFT/sinc,
    spectral nonlinear, and contiguous polyphase routes have distinct measured
    quality/ratio envelopes. Static enum dispatch, setup-time SIMD selection,
    fixed rings, and an independent slow oracle are appropriate realtime and
    numerical choices.
12. **Custom benchmark binaries and independent cross-project adapters.** They
    provide versioned reports, raw callback-tail distributions, exact work,
    native provenance, and independent comparison controls that a shared
    production adapter or Criterion-only approach would weaken.

## Strong quality signals

- Both supported feature paths are explicitly exercised: default/all-features
  resolves to SoXR, while Rubato-only proves the pure-Rust implementation.
- Focused tests cover progress validation, backpressure, terminal idempotence,
  reset isolation, exact duration, latency/tail, allocation-free first use,
  callback order, convolver reclamation, SIMD/scalar equivalence, and numerical
  or legacy oracles.
- Shared benchmark support validates environment identity, case sets, finite
  timing, compatible baselines, percentile definitions, dirty state, and JSON
  artifacts; its integration tests are meaningful.
- Current docs clearly distinguish library benchmarks from device/driver/DAC
  and end-to-end playback latency. This audit makes no device-level claim.
- The latest changelog accurately describes the moving playback facade and was
  a useful map for identifying missing Trellis ownership.

## Evidence gaps and limits

The findings above are source-backed; not every hypothetical impact was
reproduced at runtime. The highest-value missing tests are:

- adversarial local HTTP server fixtures for Range geometry, ignored Range,
  oversized/short bodies, HEAD 405, error propagation, cancellation, and log
  redaction;
- invalid EQ indices, coherent paired saturation publication, dynamic-loudness
  concurrent publication, and invalid drain policy at build time;
- raw-DSP zero/mismatched geometry and non-finite setter matrices;
- AutoMix overlap-window/tail and packet-boundary scope fixtures;
- decoder size-overflow/32-bit behavior, unknown positioned channels, and
  immutable metadata invariants;
- loudness-cache deleted/replaced/remote/future-version and corrupt-row cases;
- one-shot resampler malformed geometry/divergent channels and exact capacity
  helper properties, including nonlinear finish bounds;
- synthetic per-probe baseline pass/fail/incompatible/missing-case tests;
- an actually post-acquisition convolver build-failure lease test.

Validation accumulated during the area reviews:

- The initial full checks passed 383 all-feature and 425 Rubato-only library
  tests but preceded the final playback edits. They are superseded as final
  validation by the larger current-snapshot runs in the addendum below.
- Focused facade/chain/DSP/decoder/resampler suites all exited successfully as
  recorded in areas 01-03c.
- Benchmark-support integration tests passed 20 cases; resampler-comparison
  support passed 25 cases in both feature paths with one explicitly ignored
  native-shim prerequisite.
- Six doctests passed on the final documentation snapshot under both feature
  paths.
- `cargo check --no-default-features` failed exactly at the intentional missing
  backend guard; `cargo check --no-default-features --features rubato
  --examples` passed.
- No performance benchmark was executed during this audit. No new timing,
  regression, device, driver, DAC, or end-to-end latency claim is made.

## Remediation order

This audit is read-only, but the evidence suggests the following bounded task
sequence rather than a broad rewrite:

1. Secure and validate the HTTP/decoder trust boundary: response geometry,
   bounded body reads, secret redaction, typed source identity/errors, channel
   layout, and checked size arithmetic.
2. Restore callback/runtime correctness: dynamic-loudness reset, invalid drain
   policy at build, raw-DSP geometry, and unsupported-target logging.
3. Make high-level control acknowledgement truthful: invalid EQ indices and
   coherent paired saturation publication.
4. Fix AutoMix interval ownership and metric scope.
5. Repair evidence authority: gapless enforcement, lock-free benchmark status,
   feature matrices, and quality prose.
6. Consolidate public configuration/ranges/defaults and decide the legacy API
   lifecycle before adding more facade knobs.
7. Narrow processor/chain capabilities and resampler capacity contracts.
8. Repair cache identity/error boundaries and then reduce lower-risk naming,
   tuple, helper, fixture, and comment debt.

Do not start with a file-size-driven rewrite of the resampler, convolver,
streaming finish machine, or benchmark adapter tree. Their internal complexity
is not the main source of current risk.

## Final quality-check addendum

Quality verification completed at 2026-07-28 20:01:12 +08:00. The seven
concurrently edited tracked files retained the same sizes, mtimes, and scoped
diff size (1,688 insertions and 96 deletions) throughout the final checks.

| Check | Exact outcome |
|---|---|
| `task.py validate 07-28-codebase-maintainability-audit` | passed; `implement.jsonl` and `check.jsonl` are valid with zero entries, as expected for the inline read-only audit |
| task Markdown trailing-whitespace scan | passed; no matches |
| `cargo fmt --all -- --check` | passed; this supersedes the historical formatting failure in area 00 |
| `git diff --check` | passed; emitted only the existing LF-to-CRLF working-copy warnings |
| `cargo clippy --all-targets --all-features -- -D warnings` | passed |
| `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings` | passed |
| `cargo test --all-features` | passed: 386 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests; 1 native-shim support test explicitly ignored because its separately built shim prerequisite was absent |
| `cargo test --no-default-features --features rubato` | passed: 428 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests; the same 1 native-shim prerequisite test explicitly ignored |

The two Rubato nonlinear numerical-oracle tests ran for more than 60 seconds
and completed successfully; the Rubato library group finished in 134.76
seconds. No test, lint, formatting, or task-validation failure remains in this
snapshot.

No benchmark binary was run during Phase 2.2, so these green gates do not close
the benchmark-evidence defects documented in areas 05/06 and do not establish
any performance regression, device, driver, DAC, or end-to-end latency result.
No spec was changed: the stale/conflicting spec content is an audit finding and
source/spec remediation remains intentionally out of scope.

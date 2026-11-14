# edzsplit-rs — Port `edzsplit` C# → Rust

Status: **implemented and verified** (release build). Byte parity with the
patched C# reference on all 1,824 outputs across all 10 source archives.
**Clippy, rustfmt and 15 unit tests pass.**

> Machine constraint discovered during implementation: the default MSVC
> toolchain has no working linker on this machine (partial Visual Studio 2019
> install), so the `x86_64-pc-windows-gnu` toolchain is used. Win32 system
> imports are identical (KERNEL32 / msvcrt / ntdll / bcryptprimitives);
> GNU binutils (`as.exe`) must be on PATH for rustup's `dlltool`
> (see `scripts/build-gnu.ps1`).

Reference implementation: `../edzsplit/src/edzsplit` (C#/.NET 8) plus the locally patched
`Splitter.cs` that produced the 1,824 output files under `D:\Downloads\Brave\159`.

## 1. Locked decisions

| Item | Decision |
|---|---|
| Archive backend | Native `sevenz-rust2` first, automatic fallback to the `7z` CLI |
| Output compression | `Copy` (stored) by default; `--compress lzma2[:level]` opt-in |
| Parallelism | `--jobs auto` by default (`min(available_parallelism, 8)`); `--jobs 1` deterministic |
| Crate location | `edzsplit-rs/` next to `edzsplit/` |
| One-part-per-file | `--strict-part` opt-in; default keeps C# closure semantics |
| CLI compatibility | Commands, exit codes and JSONL event names preserved |
| C# sources | Not modified; used only as the behavioural reference |

## 2. Goals

1. A standalone Rust binary `edzsplit.exe` with no .NET runtime dependency.
2. Behavioural parity with the patched C# splitter:
   - output file names and package/item order,
   - `manifest.xml` byte format,
   - 7z `Copy` container layout and entry names,
   - exit codes `0 / 2 / 3 / 4`,
   - JSONL event stream (semantically identical; timestamps/order may differ).
3. Include both patches from the temporary build:
   - dedupe physical entries by member path,
   - drop unresolved items from the output manifest.
4. Fix the C# defects:
   - `--verify-structural` actually verifies,
   - `--jobs` actually parallelises,
   - `--only` / `--dry-run` do not force a full archive extraction.
5. Performance: lazy member reads, payload-reference cache, parallel output writes,
   optional LZMA2 compression.

## 3. Architecture

```
edzsplit-rs/
├── Cargo.toml
├── PLAN.md
├── .cargo/config.toml
├── src/
│   ├── main.rs              entry point, exit-code mapping
│   ├── cli.rs               clap definitions + usage text
│   ├── error.rs             UsageError / AppError
│   ├── event.rs             JSONL sink (stdout + optional file)
│   ├── model.rs             Manifest / Package / Item; parse + exact serializer
│   ├── payload.rs           P_ARTICLE_* refs, MD variables, accessory refs
│   ├── graph.rs             DependencyGraph + closure BFS
│   ├── split.rs             split orchestration, selection, resume, state
│   ├── verify.rs            light/deep structural verification
│   ├── selftest.rs          synthetic end-to-end tests
│   ├── util.rs              sanitize, path helpers, SHA-256, run id
│   └── archive/
│       ├── mod.rs           ArchiveSource trait, Member, ArchiveIndex
│       ├── native.rs        sevenz-rust2 backend
│       ├── cli7z.rs         7z CLI backend
│       ├── zip.rs           ZIP backend
│       └── writer.rs        7z Copy writer + optional LZMA2
├── tests/
│   ├── cli.rs
│   ├── manifest.rs
│   └── writer.rs
└── scripts/parity.ps1
```

### Data flow

```
source EDZ
  → open/list (native or 7z CLI or zip)
  → read manifest.xml
  → parse manifest
  → build archive index (exact + slash-normalized + non-ASCII-sanitized)
  → build dependency graph (part payload refs parsed once)
  → per-part BFS closure
  → clone manifest (repair locators, drop missing items)
  → build entry list (dedupe physical entries, manifest last)
  → 7z Copy writer (stream data, patch start header)
  → light structural verify
  → SHA-256 + resume state + JSONL events
```

## 4. Module contracts

### `archive::Member`
- `path: String` — original archive name, normalized to backslashes.
- `size: u64`, `packed_size: u64`, `crc: Option<u32>`.
- `exact_name: String` — name in the container, used by native `read_file`.

### `ArchiveSource` trait
```rust
trait ArchiveSource: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn members(&self) -> &[Member];
    fn read(&self, member: &Member) -> Result<Vec<u8>>;
    fn extract_all(&self, dest: &Path) -> Result<()>;
    fn test(&self) -> Result<()>;
}
```
Implementations:
- `NativeSevenZ`: header parsed once; `read` opens a `SevenZReader` (or keeps a
  `Mutex<SevenZReader>`) and calls `read_file`. For non-solid archives this is a
  direct seek/decode; for solid archives it extracts once into a cache directory.
- `CliSevenZ`: wraps `7z.exe`; lists with `l -slt -sccUTF-8`, extracts with
  `x -y -spd -scsUTF-8 -sccUTF-8`, tests with `t`.
- `ZipSource`: `zip::ZipArchive` with CRC verification on read/test.

`--backend auto`: native if the 7z header parses; fall back to CLI on
unsupported codec, encryption, or native error before any output is written.

### `model`
- `Manifest { version, packages }`, `Package { typ, key, name, items }`,
  `Item { typ, name, locator }`.
- `Item::member_path()` returns `items\<type>\<normalized locator>`.
- Parser: `quick-xml`, local-name matching, tolerant of unknown elements.
- Serializer: exact .NET `XmlWriter` output:
  - `<?xml version="1.0" encoding="utf-8"?>\r\n`
  - 1-space indentation, CRLF, no BOM,
  - self-closing ` />` with a space,
  - escapes `& < > "` and `\r \n \t` as numeric refs,
  - preserves package/item order and duplicate items.

### `payload`
- Regex equivalent to the C# `AttributeRegex`, with separate double/single quote
  capture groups (Rust regex does not allow duplicate named groups).
- Kinds: `macro`, `groupsymbolmacro`, `picture`, `externalDocument`,
  `construction`, `connectionpoints`, `accessoryId`, `isAccessory`.
- `expand_md_variables`: token order
  `MD_MACROS`, `MD_IMG`, `MD_IMAGES`, `MD_DOCUMENTS`, `MD_DOC`, `MD_MISC`.
- Accessory list/placement readers with `quick-xml`.
- `first_line` cuts at `&#10;` or `\n`.

### `graph`
- Packages stored in manifest order; `HashMap<String, Vec<usize>>` for key lookups.
- Items indexed by normalized lower-case member path; owners list `(package, item)`.
- Resolved member per unique member path, exact then sanitized fallback.
- Accessory-list reverse index keyed by referenced part number.
- Connection-point prefix index.
- Closure BFS reproduces the C# node semantics exactly:
  - seed package node,
  - accessory lists referencing the seed (mode != none),
  - item nodes expand part payload refs, construction, connection points,
  - owner-follow only for structural item kinds,
  - full mode follows part references inside accessory lists.
- `--strict-part` addition: owner-follow and accessory-list forward expansion must
  not enqueue any `part` package other than the seed. Output contains exactly one
  `part` package.

### `split`
- Selection: `--only` + `--from-file`, case-insensitive.
- Dedupe selected part packages with identical item sets; if duplicate keys have
  different item sets, keep both and warn.
- Resume state `<out>/.edzsplit-state.jsonl`: append `{partnr,status,file,sha256,items}`
  and `{partnr,status:"failed",error}`. On resume, skip only when the output file
  still exists.
- Closure ratio guard.
- Manifest clone:
  1. clone packages in closure order,
  2. repair locators resolved through the sanitized index,
  3. drop items that do not resolve,
  4. keep duplicate manifest items (e.g. manufacturer + supplier),
  5. physical entries deduped by member path, manifest last.
- Payload rewriting (`--fix-refs auto`): only `partxml` items, only
  macro/groupsymbolmacro/picture/externalDocument refs that resolve only through
  the sanitized index; replace the raw value with the sanitized token tail.
- Parallel execution:
  - `--jobs auto` = `min(available_parallelism, 8)`,
  - per-part output is independent; state/log use a mutex,
  - `--fail-fast` sets a cancel flag; running tasks finish.
- Post-write verification: `--verify-structural` (default) runs the light verify;
  `--verify-deep` runs the full decode/CRC test.

### `archive::writer`
Exact port of `SevenZipCopyWriter`:
- signature 32 bytes, version 0.4,
- one folder, one packed stream, Copy coder,
- `PackInfo`, `UnpackInfo`, `SubStreamsInfo`, `FilesInfo`,
- UTF-16LE entry names, forward slashes,
- CRC32 of concatenated uncompressed data,
- start-header patch at offset 12 and StartHeaderCRC at offset 8.
- Atomic write to `<name>.tmp-<id>` then rename.
- Optional `--compress lzma2`: use the `sevenz-rust2` writer, validate with
  `7z t` and structural verify.

### `verify`
- Light: container signature, header parse, manifest parse, all manifest items
  resolve, no traversal, known package types.
- Deep: decode every entry and verify CRCs (native) or `7z t` (CLI).
- `--strict` promotes warnings to errors.
- Path traversal guard on entry names.

### `cli`
Commands:
- `inspect <archive> [--json]`
- `split <archive> --out <dir> [options]`
- `verify <file.edz>... [--strict]`
- `case-a <archive> --out <dir>`
- `selftest`
- `version` / `--version`

New options:
- `--backend auto|native|cli`
- `--jobs <n|auto>`
- `--compress copy|lzma2[:level]`
- `--strict-part`
- `--verify-deep`
- `--progress`
- `--cache <dir>`

Exit codes: `0` ok, `2` usage, `3` partial/warnings, `4` failed.

## 5. Optimisations

| # | C# behaviour | Rust behaviour |
|---|---|---|
| 1 | Extract the whole archive into a cache | Native lazy `read_file` per member (non-solid) |
| 2 | Re-read/re-parse part XML for every closure | Parse payload refs once per member |
| 3 | Sequential split | `rayon` with `--jobs` |
| 4 | `--verify-structural` is a no-op | Light verify by default; deep opt-in |
| 5 | `--jobs` ignored | Real thread pool |
| 6 | Duplicate part packages processed and overwritten | Dedupe identical packages |
| 7 | Copy only | Copy default + LZMA2 opt-in |
| 8 | Manifest extracted to a temp file | Read directly into memory |
| 9 | .NET + 7z.exe required | Standalone binary; 7z.exe only fallback |
| 10 | SHA-256 re-reads each output | Keep, but run inside the parallel task |

## 6. Testing and parity

### Unit tests
- `write_number`: `0`, `1`, `0x7F`, `0x80`, `0x3FFF`, `0x4000`.
- CRC32 vector `"123456789" → 0xCBF43926`.
- Manifest parse/serialize golden: CRLF, no BOM, 1-space indent, ` />`,
  attribute escaping, umlauts, empty items.
- Archive index: exact, slash-normalized, sanitized, case-insensitive, first-wins.
- Payload refs: double/single quotes, `&#10;`, MD variables.
- Graph closure: none/related/full, strict, missing items.
- Writer round-trip with the native reader and `7z t`.

### Selftest
- Synthetic two-part EDZ: write, verify, split, verify outputs.
- Shared manufacturer dedupe.
- Missing macro item drop.
- Unicode `ä` archived as `_`, `--fix-refs auto` repairs locator and payload.
- `--only`, `--dry-run`, `--resume`.
- Traversal `..\evil` rejected.
- `--strict-part` produces exactly one part package.

### Parity against the 1,824 reference files
Commands:
```
edzsplit split <source>.edz --out <dir> --closure full --fix-refs auto
```
Compare:
- output file names,
- `manifest.xml` bytes,
- entry list names/sizes/CRCs (`7z l -slt`),
- whole-file SHA-256 in Copy mode,
- `7z t` for every output,
- `verify` for every output.
Run on HTC (116), SEW (1000), Wittenstein (142) first, then all 10 sets.

### Benchmark
- C# patched vs Rust, `--jobs 1` and `--jobs auto`.
- Wall time, peak working set, output size, cache size.
- Record in `docs/BENCH.md`.

## 7. Implementation phases

### Phase 0 — read-only parity
- Crate, CLI, errors, event log.
- Manifest model + exact serializer.
- Archive index + native/CLI/ZIP backends.
- `inspect`, `verify`.
- Acceptance: `inspect --json` matches C# on 10 EDZ; `verify` on reference outputs.

### Phase 1 — split core
- Payload refs, graph, closure.
- Copy writer.
- `split`, manifest clone, dedupe/drop, resume, state.
- `selftest`.
- Acceptance: HMS (7) and HTC (116) match reference; selftest passes.

### Phase 2 — optimisation + parity
- Native lazy default, `--jobs auto`, real verify, dedupe part packages,
  `--strict-part`.
- Full 10-set parity, 1,824 outputs.
- Benchmark.
- Acceptance: 100% verify, 0 missing refs, output semantics identical.

### Phase 3 — packaging
- `--compress lzma2`, `--progress`, ZIP `case-a`.
- Docs, release build, static CRT.
- Acceptance: compressed output `7z t` passes; docs complete.

## 8. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Native codec unsupported | Auto fallback CLI; gate test on all 10 EDZ |
| Manifest/header byte differences | Custom serializer, golden tests, reference comparison |
| Non-deterministic logs with auto jobs | `--jobs 1` for parity/evidence; document |
| Peak memory with large macros | Default cap 8 jobs; document; stream one entry at a time |
| Windows long paths / German names | Rust std verbatim paths; IDS test |
| LZMA2 output rejected by EPLAN | Opt-in only; test `7z t` + structural verify |
| Dedupe of non-identical duplicate parts | Compare item sets; keep both and warn if different |
| Resume state from older runs | Tolerant JSONL reader; verify file existence |
| EPLAN UAT still pending | Keep the structural-only honesty notice |

## 9. Definition of done

- `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` pass.
- `edzsplit selftest` passes.
- 10 EDZ → 1,824 outputs, 100% verify, 0 missing references.
- File names/manifest/entry CRC match the reference; Copy mode byte-identical
  where possible.
- Exit codes correct.
- `--jobs auto` faster than the C# sequential implementation.
- `--strict-part` yields exactly one part package per output.
- `--compress lzma2` output passes `7z t`.
- Standalone release binary builds with static CRT.

---

## Appendix A — Implementation report (2026-11-14)

### Verified results

- `cargo +stable-x86_64-pc-windows-gnu test`: 15/15 unit tests pass.
- `cargo +stable-x86_64-pc-windows-gnu clippy --all-targets -- -D warnings`: clean.
- `cargo fmt`: clean.
- Release build passes; `edzsplit selftest` passes.
- Parity (`scripts/parity.ps1`, release): all **1,824 outputs byte-identical**
  (names + SHA-256) to the C# reference. SEW-EURODRIVE_1 exits **3**
  (warnings, source lacks asset files) — same as the C# reference. All other
  sets exit 0.
- Deep verify of all 1,824 outputs: 0 failures (62 s).
- `--compress lzma2` output passes `7z t` (LZMA2:23, 7 blocks).
- `--strict-part` yields exactly one `part` package per output.
- `case-a` extract+verify of a nested-EDZ ZIP works.

### Deviations from the original plan

- No `clap`; CLI is hand-rolled to keep the exact C# argument semantics
  (exit code 2 on usage errors).
- No `.cargo/config.toml`; the local GNU setup lives in `scripts/build-gnu.ps1`.
- `zip` `deflate` default; ZIP entry names are read via `ZipFile::name()`
  (fallible in zip 9.5).
- The writer opens output files read+write: the start-header patch reads
  bytes back; a write-only handle fails with `Access Denied` on Windows.
- `--verify-structural` performs the light check; `--verify-deep` adds full
  decode. The C# always-`7z t` behaviour is not the default for split
  outputs: native member reads already decode every byte that is written
  (declared byte counts enforced; CRC verification where the source carries
  per-file CRCs).
- Parity outputs were written under the project tree only (this machine
  forbids writes for unsigned binaries in profile/D: paths) and deleted after
  comparison; `parity/_reports/parity.csv` and the per-set JSONL logs are
  kept.

### Pending / explicitly not done

- EPLAN UAT — structural verification is not an import guarantee.
- Packaging as a signed MSI/installer is out of scope of the port.
- `Hitachi-Industrial-Equipment.edz` produces the 116 outputs in
  `D:\Downloads\Brave\159\HTC` (identical; part numbers start with `HITA.`);
  the `Hitachi-Industrial-Equipment` output directory is empty in the source
  reference.

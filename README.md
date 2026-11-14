# edzsplit-rs

**Split a multi-device EPLAN EDZ into one standalone EDZ per part — pure Rust, no .NET runtime required.**

[Tiếng Việt](README.vi.md) · [Research report (VI)](https://github.com/wuwenhou/edzsplit/blob/master/docs/REPORT.vi.md) · [Research report (EN)](https://github.com/wuwenhou/edzsplit/blob/master/docs/REPORT.en.md) · [Evidence](https://github.com/wuwenhou/edzsplit/blob/master/docs/EVIDENCE.md)

> **Status / proof honesty notice:** This is a full functional port of the patched
> C# `edzsplit` (v0.1.2 lineage). It is verified **byte-identical** to that reference
> on all **1,824 EDZ outputs** from all 10 manufacturer archives shipped for acceptance.
> Structural verification (parse, reference resolution, path-traversal guard, decode)
> passes 100 %. **Importing an output EDZ into a live EPLAN Parts Database has NOT been
> executed on the development machine.** Claims about EPLAN import compatibility therefore
> remain *unproven*. Read the reports before adopting.

## What it does

EPLAN `.edz` files are how parts (articles) are exchanged with the EPLAN Parts Database. They come in two shapes:

* **Case A** — an archive (e.g. `Theben.7z`) that already contains many single-part `.edz` files. Handled by `edzsplit case-a`.
* **Case B** — *one* `.edz` that contains thousands of parts and resources (macros, images, documents). This is the hard problem `edzsplit-rs` solves: dependency analysis plus a writer that produces one valid EDZ per part.

## Why Rust (vs the C# original)

* Single self-contained binary; imports only Windows system DLLs (`KERNEL32`, `msvcrt`, `ntdll`, `bcryptprimitives`); no .NET SDK/runtime dependency.
* Native 7z engine in pure Rust (`sevenz-rust2`) with automatic fallback to the `7z`/`7za` CLI.
* Lazy member reads — `--only` / `--dry-run` do not extract the whole archive (the C# version always extracted everything to a cache).
* Parallel part writers (`rayon`, `--jobs auto` by default). The C# tool was sequential and ignored `--jobs`.
* `--verify-structural` actually verifies. The C# flag was a no-op.
* Identical CLI semantics, exit codes and JSONL event names.
* ~5–12× faster than the C# tool on the same machine (see [docs/BENCH.md](docs/BENCH.md)).

## Parity evidence (release build)

Byte-identical (file name + SHA-256) to the patched C# reference under `D:\Downloads\Brave\159`:

| Source archive | Parts | Identical |
|---|---|---|
| Hitachi-Industrial-Equipment → `HTC` | 116 | 116/116 |
| HMS | 7 | 7/7 |
| IDS | 97 | 97/97 |
| Jetter | 21 | 21/21 |
| Meltric | 90 | 90/90 |
| Secomea | 4 | 4/4 |
| SES-Sterling | 256 | 256/256 |
| SEW-EURODRIVE_1 (exit 3 — source lacks assets) | 1,000 | 1,000/1,000 |
| SIKA | 91 | 91/91 |
| Wittenstein | 142 | 142/142 |
| **Total** | **1,824** | **1,824/1,824** |

Deep verify of every output: 0 failures (62 s). `--compress lzma2` passes `7z t`. `--strict-part` yields exactly one `part` package per output.

Reproduce:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\parity.ps1
```

(The parity corpus itself is not part of the repository.)

## Download

Latest release: **v0.2.0** — `edzsplit.exe` for Windows x64 (GNU static build; imports only system DLLs).

```text
https://github.com/wuwenhou/edzsplit-rs/releases/tag/v0.2.0
```

SHA-256 of the release binary is published in the release notes and in `edzsplit-0.2.0-x86_64-pc-windows.sha256`.

## Build from source

```powershell
cargo build --release
# or on machines without a working MSVC linker
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 release
```

Requires Rust ≥ 1.93. Tests: `cargo test`. End-to-end smoke test: `edzsplit selftest`.

## Usage

```powershell
edzsplit inspect <archive> [--json] [--backend auto|native|cli]
edzsplit split <archive> --out <dir> [options]
edzsplit verify <file.edz>... [--strict] [--fast] [--backend auto|native|cli]
edzsplit case-a <archive> --out <dir>
edzsplit selftest
edzsplit version
```

### `split` options

| Option | Default | Meaning |
|---|---|---|
| `--only <p1,p2>` | all parts | Split only the listed part numbers |
| `--from-file <file>` | — | Part numbers, one per line (`#` comments) |
| `--closure <mode>` | `full` | `none` / `related` / `full` |
| `--fix-refs <mode>` | `auto` | `auto` rewrites payload refs that only resolve through the non-ASCII-sanitized archive index; `never` disables it |
| `--verify-structural` | on | Light verify each output (parse + reference resolution) |
| `--verify-deep` | off | Also decode every output entry while verifying |
| `--no-verify` | — | Skip per-output verification |
| `--resume` | off | Skip parts already recorded as `done` in `.edzsplit-state.jsonl` |
| `--keep-cache` | off | Keep the intermediate extraction cache |
| `--keep-going` | on | Continue after individual part failures |
| `--fail-fast` | off | Stop at the first failed part |
| `--max-closure-ratio <r>` | `1.0` | Warn when closure exceeds `r` × source bytes |
| `--dry-run` | off | Plan only; write nothing |
| `--log <path>` | — | Also append JSONL events |
| `--workdir <dir>` | `<out>/.work-<id>` | Cache directory override |
| `--backend <mode>` | `auto` | `auto` / `native` / `cli` (7-Zip CLI fallback) |
| `--jobs <n\|auto>` | `auto` | Parallel part writers (`auto` = min(cores, 8)) |
| `--compress <mode>` | `copy` | `copy` / `lzma2[:level]` |
| `--strict-part` | off | Exactly one `part` package per output |
| `--progress` | off | Progress lines to stderr |

Exit codes: `0` ok · `2` usage · `3` finished with warnings · `4` failed.

Each run prints JSONL events to stdout (pipe to a file for evidence):
`split.begin`, `backend.selected`, `archive.listed`, `manifest.parsed`,
`cache.extracted`, `resume.state`, `part.written`, `part.failed`,
`part.not-found`, `part.duplicate`, `payload.rewritten`, `locator.repaired`,
`item.missing`, `item.dropped`, `part.plan` (dry-run), `split.summary`, `warning`.

## Security notes

* Archive member paths are rejected when they are absolute (`C:\…`, `\raw`) or contain `..` segments — extraction is refused before touching disk.
* Output writes use a temporary `<file>.tmp-<id>` sibling and an atomic rename; `--resume` skips only parts recorded as done whose output file still exists.
* `--verify-structural` actually verifies; `--verify-deep` also decodes and CRC-checks every output entry.
* 7z CLI invocation uses argument lists (no shell), `-spd` literal names and a 1 h timeout.

## Scripts

| File | Purpose |
|---|---|
| `scripts/build-gnu.ps1` | Build/test/clippy/release on machines where the MSVC toolchain cannot link |
| `scripts/parity.ps1` | Re-run the reference comparison and write `parity\_reports\parity.csv` |

## License

MIT — see [LICENSE](LICENSE).

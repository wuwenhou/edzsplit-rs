# edzsplit (Rust)

Rust port of `edzsplit` (C#/.NET 8): split a multi-device EPLAN `.edz` archive
into one EDZ per part. Standalone binary; the .NET runtime is **not** required.

Status: behavioural **byte-for-byte parity** with the patched C# reference on
all 1,824 EDZ outputs from the 10 source archives shipped for acceptance
testing (outputs match file name, manifest bytes and SHA-256).

## Build

```powershell
cargo build --release
```

On machines with a working MSVC toolchain this is all that is needed.

On **this** machine the MSVC linker is missing, so the `x86_64-pc-windows-gnu`
toolchain is used with extra setup (see below). Either way, the produced
binary imports only Windows system DLLs.

### Local build helper (this machine)

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 build     # debug build
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 test      # unit tests
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 clippy    # clippy -D warnings
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 release   # release build
```

Requirements for that helper:

* rustup toolchain `stable-x86_64-pc-windows-gnu`
* GNU binutils (`as.exe`, `dlltool.exe`) on `PATH` — installed under
  `%TEMP%\opencode\mingw-binutils\mingw64\bin`
* the rust self-contained binutils directory contains the same `as.exe`
* set `EDZSPLIT_TMP` (optional) to force the temp root used by `verify`,
  `selftest` and the unit tests — useful on hardened machines that deny
  writes to the system temp directory.

## Usage

```powershell
edzsplit inspect <archive> [--json] [--backend auto|native|cli]
edzsplit split <archive> --out <dir> [options]
edzsplit verify <file.edz>... [--strict] [--fast] [--backend auto|native|cli]
edzsplit case-a <archive> --out <dir>
edzsplit selftest
edzsplit version
```

`split` options (identical to the C# tool, plus new flags):

| Option | Default | Meaning |
|---|---|---|
| `--only <p1,p2>` | all parts | Split only the listed part numbers |
| `--from-file <file>` | — | Part numbers, one per line (`#` comments) |
| `--closure <mode>` | `full` | `none` / `related` / `full` |
| `--fix-refs <mode>` | `auto` | `auto` rewrites payload refs that only resolve through the non-ASCII-sanitized archive index; `never` disables it |
| `--verify-structural` | on | Light verify each output |
| `--verify-deep` | off | Also decode every output entry during verification |
| `--no-verify` | — | Skip per-output verification |
| `--resume` | off | Skip parts recorded as `done` in `<out>/.edzsplit-state.jsonl` |
| `--keep-cache` | off | Keep the intermediate cache |
| `--keep-going` | on | Continue after individual part failures |
| `--fail-fast` | off | Stop at the first failed part |
| `--max-closure-ratio <r>` | `1.0` | Warn when closure exceeds `r` × source bytes |
| `--dry-run` | off | Plan only; write nothing |
| `--log <path>` | — | Also append JSONL events |
| `--workdir <dir>` | `<out>/.work-<id>` | Cache directory override |
| `--backend <mode>` | `auto` | `auto` / `native` / `cli` (7-Zip CLI fallback) |
| `--jobs <n\|auto>` | `auto` | Parallel part writers |
| `--compress <mode>` | `copy` | `copy` / `lzma2[:level]` |
| `--strict-part` | off | Exactly one `part` package per output |
| `--progress` | off | Progress lines to stderr |

Exit codes: `0` ok · `2` usage · `3` finished with warnings · `4` failed.

Each run prints JSONL events to stdout (pipe to a file for evidence):
`split.begin`, `backend.selected`, `archive.listed`, `manifest.parsed`,
`cache.extracted`, `resume.state`, `part.written`, `part.failed`,
`part.not-found`, `part.duplicate`, `payload.rewritten`, `locator.repaired`,
`item.missing`, `item.dropped`, `part.plan` (dry-run), `split.summary`,
`warning`.

## Why the parity matters

The acceptance corpus lives outside this repository
(`D:\Downloads\Brave\159\<set>.edz` sources plus the frozen C# outputs in
`D:\Downloads\Brave\159\<set>`).

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\parity.ps1
```

For every set the script

1. splits the source into `.\parity\<set>` (writing only under the project
   tree, because this machine denies writes for unsigned binaries elsewhere),
2. compares output file names with the reference directory,
3. compares SHA-256 of every shared file,

and writes `parity\_reports\parity.csv` plus one `*.jsonl` event log per set.

Current result (release build):

| Set | outputs | byte-identical |
|---|---|---|
| Hitachi-Industrial-Equipment | 116 | 116/116 |
| HMS | 7 | 7/7 |
| IDS | 97 | 97/97 |
| Jetter | 21 | 21/21 |
| Meltric | 90 | 90/90 |
| Secomea | 4 | 4/4 |
| SES-Sterling | 256 | 256/256 |
| SEW-EURODRIVE_1 | 1000 | 1000/1000 (exit 3: source lacks asset files) |
| SIKA | 91 | 91/91 |
| Wittenstein | 142 | 142/142 |
| **Total** | **1,824** | **1,824/1,824** |

The reference `HTC` directory is the output of `Hitachi-Industrial-Equipment.edz`
(the part numbers start with `HITA.`; the empty `Hitachi-Industrial-Equipment`
folder is a leftover of the C# run).

## Verification

* `cargo test` — 15 unit tests (writer byte format, manifest CRLF/escaping,
  payload refs, graph/closure paths, traversal guard).
* `edzsplit selftest` — end-to-end synthetic EDZ: write, verify, split,
  verify, `--only`, `--dry-run`, `--resume`, traversal rejection, `--strict-part`.
* Deep verify of all 1,824 outputs: `failed=0` in 62 s.
* `--compress lzma2` output passes `7z t`.
* `--strict-part` yields exactly one `part` package per output.
* `--jobs auto` throughput is 5–12× the C# sequential tool (see
  `docs/BENCH.md`).

## Security notes

* Archive member paths are rejected when they are absolute (`C:\…`, `\raw`)
  or contain `..` segments — extraction is refused before touching disk.
* Output writes use a temporary `<file>.tmp-<id>` sibling and an atomic rename;
  `--resume` skips only parts recorded as done whose output file still exists.
* `--verify-structural` actually verifies; `--verify-deep` also decodes and
  CRC-checks every output entry.
* 7z CLI invocation uses argument lists (no shell), `-spd` literal names
  and a 1 h timeout.

## Known caveats

* SEW-EURODRIVE_1 source has no macro/picture/PDF assets for most parts, so it
  produces 1,000 valid outputs **with warnings** and exit code 3 — identical to
  the C# reference.
* The C# `--verify-structural` was a no-op and `--jobs` was ignored; both are
  real here. Byte parity holds regardless.
* EDZ files produced by this tool are structurally verified only; the EPLAN
  import UAT remains with EPLAN itself.

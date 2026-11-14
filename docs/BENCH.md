# Benchmark: `edzsplit` C#/.NET 8 + 7z CLI vs `edzsplit` Rust

Machine: Windows dev box (existing workspace), .NET SDK 8.0.425, 7-Zip 23.01,
Rust 1.98.1 (MSVC toolchain unavailable) / release build via
`x86_64-pc-windows-gnu` with LTO, `codegen-units = 1`, `panic = abort`, `strip`.

Methodology:

* All runs produce **byte-identical** output (SHA-256 compared against the
  frozen C# reference under `D:\Downloads\Brave\159`).
* Output goes to a fresh directory per run under the project tree; the source
  archive is read once per run.
* Wall time is a single run (not a statistical sample), machine otherwise idle.
* `--verify-structural` enabled in both implementations (default).
* Sources: `SEW-EURODRIVE_1.edz` (1.98 MB, 1,000 parts, 1000 outputs,
  ~27.6 MB written), `Wittenstein.edz` (10.5 MB, 142 parts,
  1.16 GB written — macro-heavy), and `Hirschmann.edz` (216 MB,
  521 parts → 474 outputs, ~1.06 GB written).

| Tool | Mode | SEW-EURODRIVE_1 | Wittenstein | Hirschmann |
|---|---|---|---|---|
| C# (patched) + 7z CLI | sequential | 66.68 s | 35.30 s | 46.80 s |
| Rust release | `--jobs 1` | 49.33 s | 43.72 s | — |
| Rust release | `--jobs auto` (8) | **5.49 s** | **6.60 s** | **7.08 s** |

Full 10-set parity run (1,824 outputs, `edzsplit split … --jobs auto`):
~31 s total wall time; deep verify of all 1,824 outputs 62 s, 0 failures.

Hirschmann results are also byte-identical between the two implementations
(474/474 outputs, file name + SHA-256) and ship in the release as
`Hirschmann-split-474.zip` for independent evaluation.

Deep verify of the Hirschmann outputs: 474/474 OK. `--jobs 1` was not measured
on Hirschmann.

Notes:

* `.NET`+7z extracts to a cache directory then copies files; Rust `--jobs 1`
  streams member reads through the native LZMA reader without a disk cache.
  Part XML members are decoded twice (graph closure + fix-refs pass); all
  other members are decoded exactly once.
* `jobs=auto` peaks at ~8 concurrent member decodes; the largest single member
  is ~38 MB (`Wittenstein` 3D macros), so memory headroom is ~300 MB above
  steady-state.
* Both implementations exit with code 3 for `SEW-EURODRIVE_1` (source lacks
  assets) and 0 for `Wittenstein`.

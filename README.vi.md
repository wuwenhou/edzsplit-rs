# edzsplit-rs

**Tách EDZ đa thiết bị EPLAN thành một EDZ độc lập cho từng part — thuần Rust, không cần .NET runtime.**

[English](README.md) · [Báo cáo nghiên cứu (VI)](https://github.com/wuwenhou/edzsplit/blob/master/docs/REPORT.vi.md) · [Research report (EN)](https://github.com/wuwenhou/edzsplit/blob/master/docs/REPORT.en.md) · [Bằng chứng](https://github.com/wuwenhou/edzsplit/blob/master/docs/EVIDENCE.md)

> **Trạng thái / tuyên bố trung thực:** Đây là bản port đầy đủ và đã được kiểm chứng
> **trùng khớp byte-for-byte** với bản C# `edzsplit` đã vá (lineage v0.1.2) trên
> **toàn bộ 1.824 output EDZ** từ 10 archive của nhà sản xuất nằm trong bộ
> acceptance. Verify cấu trúc (parse XML, giải quyết reference, chặn path traversal,
> decode) đạt 100 %. Bản Linux x86-64 được CI chạy thử trên `ubuntu-latest`
> (unit tests + `selftest`) ở mọi lần push và mọi tag release; bản Windows x64 chạy
> tương tự trên `windows-latest`; máy phát triển này (Windows dev box) không chạy
> được bản Linux, nên CI là nơi thi hành bản Linux chính thức. **Chưa từng import
> một file EDZ vào EPLAN Parts Database thật.** Mọi khẳng định về tương thích import
> EPLAN do đó vẫn *chưa được chứng minh*. Đọc các báo cáo trước khi quyết định dùng.

## Công cụ này làm gì

File `.edz` của EPLAN là định dạng trao đổi part (article) với EPLAN Parts Database. Có hai dạng:

* **Trường hợp A** — một archive (ví dụ `Theben.7z`) đã chứa sẵn nhiều file `.edz` một part. Xử lý bằng `edzsplit case-a`.
* **Trường hợp B** — *một* file `.edz` chứa hàng nghìn part và tài nguyên (macro, hình ảnh, tài liệu). Đây là bài toán khó mà `edzsplit-rs` giải quyết: phân tích phụ thuộc + writer tạo ra một EDZ hợp lệ cho mỗi part.

## Tại sao dùng Rust (thay vì bản C# gốc)

* Binary độc lập, chỉ import DLL hệ thống Windows (`KERNEL32`, `msvcrt`, `ntdll`, `bcryptprimitives`); không cần .NET SDK/runtime.
* Engine đọc 7z thuần Rust (`sevenz-rust2`), tự động fallback sang CLI `7z`/`7za` khi cần.
* Đọc member lười (lazy) — `--only` / `--dry-run` không ép giải nén toàn bộ archive (bản C# luôn giải nén toàn bộ ra cache trước).
* Ghi file song song cho từng part (`rayon`, mặc định `--jobs auto`). Bản C# chạy tuần tự và bỏ qua tham số `--jobs`.
* `--verify-structural` giờ thật sự verify. Cờ này bên C# là no-op.
* Giữ nguyên CLI semantics, exit code và tên sự kiện JSONL.
* Nhanh hơn bản C# khoảng 5–12× trên cùng máy (xem [docs/BENCH.md](docs/BENCH.md)).

## Bằng chứng parity (bản release)

So file name + SHA-256 với bộ C# reference đã vá dưới `D:\Downloads\Brave\159`:

| Source archive | Parts | Trùng khớp |
|---|---|---|
| Hitachi-Industrial-Equipment → `HTC` | 116 | 116/116 |
| HMS | 7 | 7/7 |
| IDS | 97 | 97/97 |
| Jetter | 21 | 21/21 |
| Meltric | 90 | 90/90 |
| Secomea | 4 | 4/4 |
| SES-Sterling | 256 | 256/256 |
| SEW-EURODRIVE_1 (exit 3 — source thiếu asset) | 1.000 | 1.000/1.000 |
| SIKA | 91 | 91/91 |
| Wittenstein | 142 | 142/142 |
| **Tổng** | **1.824** | **1.824/1.824** |

Deep verify toàn bộ output: 0 lỗi (62 giây). `--compress lzma2` vượt qua `7z t`. `--strict-part` cho đúng một `part` package trên mỗi output.

Chạy lại kiểm chứng:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\parity.ps1
```

(Bộ parity corpus không nằm trong repository.)

## Tải về

Release mới nhất: **v0.2.2** — được build và self-test trong CI trên `ubuntu-latest` và `windows-latest`.

| Nền tảng | Asset | Ghi chú |
|---|---|---|
| Linux x86-64 | `edzsplit-v0.2.2-x86_64-unknown-linux` | **Bản musl tĩnh hoàn toàn; không cần glibc.** Chạy được trên mọi kernel Linux x86-64. |
| Windows x64 | `edzsplit-v0.2.2-x86_64-pc-windows.exe` | Bản MSVC với CRT tĩnh; chỉ import DLL hệ thống. |

```text
https://github.com/wuwenhou/edzsplit-rs/releases/tag/v0.2.2
```

Mỗi asset đều có file `.sha256` đi kèm trong cùng release.

Ghi chú cho Linux:

* Engine native chạy ngay trên mọi Linux x86-64 (CI chạy `selftest` + unit tests trên `ubuntu-latest`).
* Fallback `7z` CLI cần p7zip: `sudo apt install p7zip-full`
  (Debian/Ubuntu) / `sudo yum install p7zip` (RHEL). Mặc định `--backend auto` không cần tới nó.
* `EDZSPLIT_7Z` đổi đường dẫn tới binary `7z` của fallback.

## Build từ source

```powershell
cargo build --release
# hoặc trên máy không có linker MSVC hoạt động
powershell -ExecutionPolicy Bypass -File .\scripts\build-gnu.ps1 release
```

Yêu cầu Rust ≥ 1.93. Chạy test: `cargo test`. Smoke test đầu-cuối: `edzsplit selftest`.

## Sử dụng

```powershell
edzsplit inspect <archive> [--json] [--backend auto|native|cli]
edzsplit split <archive> --out <dir> [options]
edzsplit verify <file.edz>... [--strict] [--fast] [--backend auto|native|cli]
edzsplit case-a <archive> --out <dir>
edzsplit selftest
edzsplit version
```

### Tùy chọn `split`

| Tùy chọn | Mặc định | Ý nghĩa |
|---|---|---|
| `--only <p1,p2>` | tất cả part | Chỉ tách các part được liệt kê |
| `--from-file <file>` | — | Danh sách part, mỗi dòng một part (comment `#`) |
| `--closure <mode>` | `full` | `none` / `related` / `full` |
| `--fix-refs <mode>` | `auto` | `auto` sửa payload ref chỉ resolve được qua index phi-ASCII (sanitized); `never` tắt |
| `--verify-structural` | bật | Verify nhẹ từng output (parse + resolve reference) |
| `--verify-deep` | tắt | Đồng thời decode toàn bộ entry khi verify |
| `--no-verify` | — | Bỏ verify từng output |
| `--resume` | tắt | Bỏ qua part đã ghi `done` trong `.edzsplit-state.jsonl` |
| `--keep-cache` | tắt | Giữ cache trung gian |
| `--keep-going` | bật | Tiếp tục khi một part lỗi |
| `--fail-fast` | tắt | Dừng khi part đầu tiên lỗi |
| `--max-closure-ratio <r>` | `1.0` | Cảnh báo khi closure lớn hơn `r` × bytes của source |
| `--dry-run` | tắt | Chỉ tính toán, không ghi file |
| `--log <path>` | — | Ghi thêm sự kiện JSONL vào file |
| `--workdir <dir>` | `<out>/.work-<id>` | Đổi thư mục cache |
| `--backend <mode>` | `auto` | `auto` / `native` / `cli` (fallback 7z CLI) |
| `--jobs <n\|auto>` | `auto` | Ghi song song (`auto` = min(số core, 8)) |
| `--compress <mode>` | `copy` | `copy` / `lzma2[:level]` |
| `--strict-part` | tắt | Đúng một `part` package trên mỗi output |
| `--progress` | tắt | In tiến độ ra stderr |

Exit codes: `0` OK · `2` lỗi cú pháp · `3` xong nhưng có cảnh báo · `4` thất bại.

Mỗi lần chạy in các sự kiện JSONL ra stdout (pipe vào file để làm bằng chứng):
`split.begin`, `backend.selected`, `archive.listed`, `manifest.parsed`,
`cache.extracted`, `resume.state`, `part.written`, `part.failed`,
`part.not-found`, `part.duplicate`, `payload.rewritten`, `locator.repaired`,
`item.missing`, `item.dropped`, `part.plan` (chế độ `--dry-run`), `split.summary`, `warning`.

## Ghi chú bảo mật

* Đường dẫn trong archive bị từ chối nếu là absolute (`C:\…`, `\raw`) hoặc chứa `..` — hệ thống từ chối giải nén trước khi ghi đĩa.
* Ghi output qua file tạm `<file>.tmp-<id>` rồi rename atomic; `--resume` chỉ bỏ qua các part đã ghi `done` và file output vẫn tồn tại.
* `--verify-structural` thật sự kiểm tra; `--verify-deep` còn decode và kiểm tra CRC toàn bộ entry output.
* Gọi 7z CLI bằng danh sách đối số (không qua shell), `-spd` giữ tên nguyên văn, timeout 1h.

## Scripts

| File | Mục đích |
|---|---|
| `scripts/build-gnu.ps1` | Build/test/clippy/release trên máy không có linker MSVC hoạt động |
| `scripts/parity.ps1` | Chạy lại so sánh với reference, ghi `parity\_reports\parity.csv` |

## License

MIT — xem [LICENSE](LICENSE).

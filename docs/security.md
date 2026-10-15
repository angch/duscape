# Defenses Against Malicious and Corrupted Files

Duscape inspects arbitrary files across local and remote disks, previews their contents,
and expands archive formats (ZIP, JAR, APK, WHEEL, etc.) into navigable trees. Because any
folder on disk may host untrusted, corrupted, or adversarially crafted data, the scanner,
preview engine, and archive parser are designed with strict defense-in-depth bounds.

This document describes how Duscape defends against corrupted and malicious files,
including zip bombs, pixel/decompression bombs, terminal escape injection, and path traversal.

---

## 1. Threat Model & Boundaries

Duscape operates under the following threat model:
- **Untrusted file contents**: Any regular file may contain malformed headers, invalid compression streams, truncated blocks, or malicious exploits targeting image or decompression libraries.
- **Untrusted directory structures**: Archives may attempt path traversal (Zip Slip), declare cyclic paths, overlap compressed blocks, or declare billions of files to exhaust memory.
- **Read-only archive inspection**: Duscape **never unpacks archives to the physical disk**. Archives are represented as virtual folders in memory within the model (`FileTree::expand_archive`), and member files are unpacked on-demand into memory buffers solely for previews.
- **Protected deletions**: Deletions cannot target paths inside archives (`delete::cannot_delete` returns a refusal stating the file is inside an archive).

---

## 2. Preview Engine Defenses

File previews are driven by `libduscape::preview` on a dedicated background thread (`Reader` / `Previewer`). The main viewer thread is never blocked on disk reads or raster decoding.

### A. Overly Large Images and Decompression / Pixel Bombs
Adversarial images can attack via two vectors: excessive file size on disk, or small file size that expands into billions of pixels when decoded (decompression/pixel bombs).

1. **File Size on Disk Limits**:
   - **Terminal (TUI) and Windows Viewers**: Bound on-disk image size with `MAX_IMAGE_BYTES` (256 MiB). Files exceeding 256 MiB are never decoded. `describe_picture` reads only the header dimensions using `into_dimensions()` and displays a text caption (`JPEG image · 16000×12000`).
   - **Linux and macOS Viewers**: Bound on-disk image size with `MAX_PICTURE` (64 MiB). Files exceeding 64 MiB are rejected upfront with `"too large to preview"` without reading or decoding them.

2. **Decoded Dimension and Memory Limits (`image::Limits`)**:
   - Raster decoding via the Rust `image` crate (`decode_picture`) configures strict `image::Limits`:
     - `max_image_width`: 40,000 pixels.
     - `max_image_height`: 40,000 pixels.
     - `max_alloc`: 1 GiB (`1024 * 1024 * 1024` bytes) in TUI and Windows; 512 MiB in Linux.
   - If an image header declares dimensions greater than 40,000 pixels on a side or requires more than the configured allocation limit, `reader.decode()` immediately terminates with `ImageError::LimitsExhausted`. This error is caught and converted to text (`"<format> image, unreadable: <reason>"`), avoiding crashes or out-of-memory (OOM) aborts.
   - On **macOS**, `NSImage` representations are inspected prior to rendering: if `pixelsWide * pixelsHigh` exceeds `MAX_PICTURE_PIXELS` (268M pixels, ~1 GiB at 4 B/pixel), the image is rejected with `"<caption\>, too large to preview"`.
   - On **MS-DOS**, `PREVIEW.ASM` bounds decoding sides to `PV_MAXSIDE = 40000`.

3. **Immediate Viewport Downscaling**:
   - Once decoded, pictures are immediately scaled down to the viewport size (`fit(image, width, height)` or `imageops::resize`). Full-scale raster buffers are dropped immediately after thumbnail generation.

4. **Debounced Background Execution**:
   - Moving the cursor quickly across a folder of photos debounces requests (`IMAGE_DEBOUNCE = 100ms`, `DEBOUNCE = 60ms`). Stale queued requests are discarded unread, so rapid navigation decodes nothing until the selection rests on an entry.

### B. Corrupted Images and Format Sniffing
- **Magic-Byte Sniffing**: Files are classified by inspecting their first bytes (`sniff`), not their file extensions. A malicious binary or script named `photo.jpg` is detected as `Kind::Binary` (showing a hex dump) or `Kind::Text`, preventing image decoder fuzzing.
- **Header Plausibility**: Formats with short 2-byte magic signatures (such as BMP, PNM, and QOI) are cross-checked against their header fields (`plausible`, `pnm_header`) before handing them to `image`.
- **Decoding Errors**: Truncated streams, bit flips, or corrupt markers in JPEG, PNG, WebP, etc., return `ImageError`, which surfaces cleanly in the info panel without panics.

### C. Text Previews and Terminal Escape Injection
- **Bounded Reads**: Text preview reads at most `HEAD_BYTES` (64 KiB).
- **Control Character Sanitization**: In `text_lines`, all control characters (`c.is_control()`), including ANSI escape sequences (`\x1b`), are sanitized and replaced with `'?'`. This neutralizes terminal escape injection attacks (such as screen clears, palette reprogramming, title changes, or terminal emulator exploits).
- **Lossy UTF-8**: Invalid byte sequences are replaced lossily using `String::from_utf8_lossy`.

### D. Special and Non-Regular Files
- `head_of` requires `metadata.is_file()`. FIFOs, block/character devices (`/dev/zero`, `/dev/urandom`), and Unix domain sockets are rejected with `"not a regular file"`.
- On macOS, files stored only in iCloud (`SF_DATALESS` flag) are rejected with `"in iCloud, not downloaded"` to prevent blocking on network downloads.

---

## 3. Archive (`.zip`, `.jar`, etc.) Defenses

Zip archive handling in `libduscape::archive` lists central directories and reads entries for previews.

### A. Decompression Bombs (42.zip, High-Ratio Bombs)
1. **Never Decompressed on Scan**:
   - During filesystem scanning, archives are identified by extension (`is_archive_name`) and noted. When visible on screen, an idle pass reads only the central directory at the end of the file (`read_index`). Entries are never decompressed during scanning or treemap layout.
2. **Treemap Folder Clamping**:
   - In `FileTree::expand_archive`, archive members are added using their **compressed** sizes (`member.compressed`), not their uncompressed sizes.
   - The total size of the expanded folder is fixed to the archive's size on disk (`inside.sizes = sizes;`). A 42 KB zip bomb claiming petabytes of uncompressed data occupies only 42 KB in the treemap.
3. **Decompression Streaming Caps**:
   - When previewing text in an archive, decompression is strictly bounded by `.take(limit)` with `HEAD_BYTES` (64 KiB).
   - When previewing pictures in an archive, `read_member_whole` pre-checks declared uncompressed size against `MAX_MEMBER_PICTURE` (64 MiB) **before** decompression begins. If declared size is within bounds, decompression is still bounded by `.take(max_uncompressed + 1)`, catching archives with falsified headers before they can allocate excessive memory.

### B. High-Entry & Fifield Non-Recursive Bombs
Fifield zip bombs overlap compressed data across thousands or millions of directory entries, or provide massive central directories to cause CPU/RAM exhaustion.

1. **Central Directory Buffer Limit**:
   - `MAX_DIRECTORY` (256 MiB) limits the central directory buffer size. An archive whose central directory claims more than 256 MiB is rejected with `"zip index too large to read"`.
2. **Member Count Limit**:
   - `MAX_MEMBERS` (1,000,000) caps the number of members parsed from an archive. Once 1,000,000 entries are seen, `parse_index` breaks, sets `index.truncated = true`, and the viewers display a notice (`"archive listing cut at 1,000,000 entries"`).
3. **Path Deduplication**:
   - `FileTree::expand_archive` uses a `HashSet<&[String]>` to discard duplicate paths declared within a malformed or adversarial central directory.

### C. Recursive Zip Bombs (Nested Archives)
- The archive scanner notes archives only from entries found on the real filesystem.
- Entries inside an expanded archive that have archive extensions (`.zip`, `.jar`) are **not** added to the archive scan queue. They are treated as ordinary files, and selecting them in preview renders a hex dump of their header rather than recursing.

### D. Path Traversal & Zip Slip
- `member_path` inspects every path component:
  - Components named `..` return `None`, causing the entry to be discarded.
  - Non-normal path components (root slashes, Windows drive prefixes like `C:`) return `None`, discarding the entry.
- Because Duscape never unpacks archive entries to the filesystem, Zip Slip attacks cannot write or overwrite host files.

### E. Corrupted Archive Records & Arithmetic Safety
- **Bounds Checking**: Record slices check bounds (`end > bytes.len()`) before indexing.
- **Zip64 Extra Fields**: Handled by `zip64_sizes`, checking bounds for each ID and field length. Missing or truncated extra fields return `io::ErrorKind::InvalidData`.
- **Shift & Offset Arithmetic**: Offset and shift calculations use `saturating_add` and `saturating_sub` (e.g. `member.header.saturating_add(skip)` and `offset.saturating_add(shift).saturating_add(size) > len`), preventing integer overflow panics on crafted offsets.

---

## 4. Architectural Analysis: Direct Sub-Sampled Decoding & Binary Cost Trade-offs

When previewing images that are significantly larger than the preview panel (for example, a 48 MP photograph displayed in a 400×200 pixel panel), decoders can either decode full resolution and downscale, or perform sub-sampled decoding directly during decompression:

```
Pipeline A (Full Decode + Downsample):
Compressed File  ──►  Full-Resolution Decode  ──►  Downsample / Thumbnail  ──►  Render
(e.g. 5 MB JPEG)      (e.g. 192 MB RGBA buffer)     (e.g. 320 KB RGBA buffer)

Pipeline B (Direct Sub-Sampled Decode):
Compressed File  ──►  Sub-Sampled IDCT Decode (1/8)  ──►  Render
(e.g. 5 MB JPEG)      (e.g. 3 MB RGBA buffer)
```

### Format Constraints on Direct Downscaling
1. **JPEG (DCT Scaling)**:
   - JPEG stores data in 8×8 discrete cosine transform (DCT) blocks.
   - Decoders that expose IDCT scale options (such as `libjpeg-turbo`) compute only the top-left 1×1, 2×2, or 4×4 coefficients, natively producing 1/8, 1/4, or 1/2 scale images during IDCT.
   - At 1/8th scale, memory allocation is reduced by a factor of 64×, and decoding speed increases by 4–8×.
   - *Limitation in `image` crate (0.25)*: The general `image::ImageDecoder` trait does not expose scale options; its `read_image` requires a buffer sized to full native dimensions (`total_bytes()`).
2. **Lossless Formats (PNG, BMP, QOI)**:
   - PNG uses DEFLATE compression and scanline filtering (`Sub`, `Up`, `Average`, `Paeth`). Every row mathematically depends on the preceding row. Downscaling during decompression without unfiltering previous rows is not possible in standard PNG.

### Empirical Binary Size Benchmarks of Decoder Crates

To evaluate the feasibility of sub-sampled JPEG decoding in Duscape without relying on the generic `image` crate pipeline, we benchmarked potential configurations against the release configuration (`opt-level = "s"` vs `"z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, stripped on macOS aarch64):

| Decoder Configuration | Release Binary Size | Delta vs Baseline | Sub-Sampling Support | Notes / Optimization Details |
|-----------------------|---------------------|-------------------|----------------------|------------------------------|
| **Baseline (`duscape` with `image 0.25`, `-Os`)** | 2,596,160 bytes (~2.48 MB) | — | No (1/1 full decode) | Standard multi-format engine with bounded `image::Limits` |
| **`image` with `"jpeg"` feature removed** | 2,529,968 bytes | **-66 KiB (-2.55%)** | N/A | Drops `image`'s JPEG decoder and format parser |
| **`zune-jpeg` 0.5 (direct API)** | 2,596,176 bytes | +16 B (+0.00%) | **No** (8×8 IDCT only) | Already transitively pulled via `image 0.25` / `tiff`; direct API lacks fractional IDCT |
| **`libjpeg-turbo` (via `turbojpeg`)** | 2,832,640 bytes | **+231 KiB (+9.11%)** | **Yes** (1/1, 1/2, 1/4, 1/8) | Native C library; requires `cmake`, `cc`, and C toolchains |
| **`libjpeg-turbo-rs` (decode + encode)** | 2,860,608 bytes | **+258 KiB (+10.19%)** | **Yes** (1/1, 1/2, 1/4, 1/8) | Pure Rust with SIMD; with `image` `"jpeg"` removed |
| **`libjpeg-turbo-rs` (decode only, LTO)** | 2,761,456 bytes | **+161 KiB (+6.37%)** | **Yes** (1/1, 1/2, 1/4, 1/8) | Pure Rust with SIMD; LTO dead-code-eliminates unused encoder |
| **Aggressive Size Optimization (`opt-level = "z"`)** | **2,141,168 bytes (~2.04 MB)** | **-455 KiB (-17.53%)** | No (baseline formats) | Passes all tests; maintains ~917,000 entries/sec scan throughput |

### Detailed Symbol Breakdown: Where `libjpeg-turbo-rs`'s Size Goes

A symbol-level inspection via `llvm-nm` on unstripped Mach-O arm64 release binaries reveals where `libjpeg-turbo-rs`'s ~247.5 KiB footprint resides:

| Component / Submodule | Binary Footprint | % of Crate | Description / Contents |
|-----------------------|------------------|------------|------------------------|
| **`decode::pipeline` (Core)** | **108.3 KiB** | 43.8% | `decode_baseline_planes` (23.9 KiB), `decode_progressive_planes` (19.7 KiB), `decode_image_inner` (25.1 KiB), MCU dispatch |
| **Color Lookup Tables (`__const`)** | **41.5 KiB** | 16.8% | `CR_R_TAB`, `CB_B_TAB`, `RANGE_LIMIT` precomputed tables for branchless scalar color conversion |
| **`decode::marker`** | **20.3 KiB** | 8.2% | JPEG marker parser (`read_markers`: SOF, SOS, DQT, DHT, APP markers, EXIF, XMP, IPTC) |
| **`decode::arithmetic`** | **18.9 KiB** | 7.6% | Arithmetic entropy decoding (`decode_arithmetic_planes`, progressive arithmetic) |
| **`simd::aarch64` & `simd::scalar`** | **17.5 KiB** | 7.1% | Explicit NEON SIMD kernels (14.1 KiB) and scalar fallbacks (3.4 KiB) |
| **`decode::lossless`** | **13.5 KiB** | 5.5% | Lossless JPEG mode (SOF3 / SOF11 predictive coding) |
| **`decode::idct_extended`** | **13.3 KiB** | 5.4% | Extended DCT block sizes (9×9, 10×10, ..., 16×16 SmartScale) |
| **`api::precision` (12-bit)** | **12.8 KiB** | 5.2% | 12-bit sample precision decompression & 12-bit upsampling |
| **Metadata Sorting & Error Types** | **~10.0 KiB** | 4.0% | Multi-segment XMP/ICC chunk sorting, error string formatting |

#### Trimming Opportunities & Analysis
1. **Niche Modes Account for ~70 KiB (28%)**:
   - Arithmetic coding (18.9 KiB), Lossless JPEG (13.5 KiB), SmartScale extended IDCT (13.3 KiB), and 12-bit precision (12.8 KiB) represent over 34% of the decoder's executable code.
   - Because `Decoder::decode_image_inner` is a monolithic dispatcher referencing each path unconditionally, LLVM cannot dead-code-eliminate them.
   - Feature-gating these niche modes in upstream `libjpeg-turbo-rs` (e.g. `feature = "arithmetic"`, `feature = "12bit"`) would shrink its decode-only footprint from **~200 KiB down to ~130 KiB**.
2. **Replacing 41.5 KiB Tables on 64-bit Targets**:
   - Precomputed lookup tables (`CR_R_TAB`) were introduced to eliminate multiplies on weak scalar architectures (RISC-V/s390x). On modern x86_64 and AArch64 targets where integer multiplication takes a single cycle, calculating the transform directly in registers would reclaim up to ~40 KiB of static data.
3. **SIMD Efficiency**:
   - Disabling `libjpeg-turbo-rs`'s `simd` feature actually **increased** stripped binary size by **+472 bytes** (due to scalar loop unrolling and auto-vectorization expansion). SIMD should remain enabled.
4. **General Binary Space Saving in Duscape (`opt-level = "z"`)**:
   - Switching `opt-level = "s"` to `"z"` in `Cargo.toml` saves **455 KiB (-17.5%)** across the entire workspace binary with zero functional regressions and indistinguishable scan speed.

### Native macOS Pipeline: OS-Accelerated Zero-Cost Decoding

Crucially, **macOS desktop builds (`duscape-mac`) already bypass third-party raster decoders and downsamplers entirely**:

1. **Direct Compressed Data Pass-Through**:
   - `duscape_viewer::preview` hands raw compressed bytes (`Loaded::Picture { bytes, caption }`) directly to `viewers/macos/src/mac/view.rs`.
   - The macOS viewer wraps these bytes in `NSData` and instantiates `NSImage::initWithData(alloc, &data)`.
2. **Deferred On-Demand Native Decoding**:
   - `NSImage` retains the compressed data backed by macOS's ImageIO framework (`CGImageSourceRef`). Full-resolution uncompressed rasters (e.g. 192 MB for a 48 MP photo) are **never** allocated in the application's heap memory.
   - Header dimensions are validated via `pixels(&image)` before display: if dimensions exceed `MAX_PICTURE_PIXELS` (268M pixels, ~1 GiB), the image is rejected before drawing.
3. **Hardware-Accelerated Rendering Directly to Viewport**:
   - When `drawInRect_fromRect_operation_fraction_respectFlipped_hints` is invoked in `viewers/macos/src/mac/draw.rs`, AppKit and Quartz render directly into the destination viewport bounds (`ns_rect(at)`).
   - Under the hood, ImageIO and Apple Silicon's hardware JPEG decoders decode directly at screen/viewport resolution, avoiding the CPU decode-and-downsample bottleneck.
4. **Zero Binary Overhead**:
   - Because AppKit and ImageIO are built into macOS, this hardware-accelerated, direct-to-viewport pipeline adds **0 bytes** to Duscape's binary size.
   - It also natively supports Apple-specific and modern formats (`.heic`, `.heif`, `.avif`, Apple ProRAW) without external crate dependencies.

**Decision**: Third-party decoder crates like `libjpeg-turbo` were rejected because macOS already handles direct downscaled decoding natively via OS calls, without requiring additional dependencies or binary bloat.

### Platform Summary

| Viewer | Pipeline | Decompression Limits | Memory Footprint |
|--------|----------|----------------------|------------------|
| **macOS Desktop (`duscape-mac`)** | Native AppKit / ImageIO (`NSImage`) | `MAX_PICTURE_PIXELS` (268M px, ~1 GiB); `MAX_PICTURE` (64 MiB on disk) | **Near zero**; on-demand native decoding directly to viewport bounds |
| **Linux Desktop (`duscape-linux`)** | `libduscape` + `image` crate | `image::Limits` (512 MiB alloc, 40,000 px side); `MAX_PICTURE` (64 MiB) | Bounded by 512 MiB; immediate `fit` downscaling |
| **Windows Desktop (`duscape-windows`)** | `libduscape` + `image` crate + GDI DIB | `image::Limits` (1 GiB alloc, 40,000 px side); `MAX_IMAGE_BYTES` (256 MiB) | Bounded by 1 GiB; immediate `fit` downscaling |
| **Terminal (`viewers/tui`)** | `libduscape` + `image` crate | `image::Limits` (1 GiB alloc, 40,000 px side); `MAX_IMAGE_BYTES` (256 MiB) | Bounded by 1 GiB; immediate `fit` to cell matrix |

### Linux Considerations & The Strategic Path Forward: `zune-jpeg`

While macOS relies on operating-system frameworks for zero-cost image downscaling, the equation on Linux is distinct:

1. **Linux Software-Only Reality**:
   - `duscape-linux` purposefully avoids heavy GUI toolkits (GTK/Qt) to remain a lean, self-contained Wayland/X11 static binary (`make static`) with zero shared-library dependencies.
   - Consequently, all image previews on Linux must be software-rendered in user space via `libduscape` and the `image` crate.
   - On a Linux filesystem, high-resolution JPEGs (>1080p) represent roughly 1%–2% of image files (concentrated in user photo directories like `~/Pictures`). However, when previewing a 48 MP photograph (8000×6000 px), the current pipeline transiently allocates **~192 MB of heap memory** before resizing to `PICTURE_SIDE` (1024 px).

2. **The Core Objective: Memory Footprint Optimization**:
   - The primary goal of direct IDCT sub-sampling is **optimal memory usage (RAM footprint)**, rather than binary size or decoding speed.
   - By decoding only the top-left 1×1 or 2×2 DCT coefficients, peak memory drops from **~192 MB to ~3 MB (-98.4%)**, eliminating temporary memory spikes on RAM-constrained machines, single-board computers (Raspberry Pi), or containerized environments.

3. **`zune-jpeg` as the Architectural Future**:
   - `zune-jpeg` is already linked transitively into Duscape (via `image 0.25` / `tiff`) at **0 bytes of extra binary size**.
   - It is written in pure Rust, provides SIMD acceleration without C toolchains, and avoids the binary bloat of `libjpeg-turbo-rs`'s monolithic dispatchers.
   - **The Roadmap**:
     - The optimal path is to avoid adopting heavy third-party crates now, and instead **wait until `zune-jpeg` natively exposes IDCT fractional scaling (1/2, 1/4, 1/8) in its public API**, OR
     - **Fork / clone `zune-jpeg`** to add the fractional IDCT sub-sampling feature specifically needed for memory-optimal thumbnail generation in Duscape, contributing the enhancement upstream.



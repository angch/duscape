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

## 4. Architectural Analysis: Direct Sub-Sampled Decoding

When previewing images that are significantly larger than the preview panel (for example, a 48 MP photograph displayed in a 400×200 pixel panel), Duscape currently decodes the image into memory and then downscales it via `fit`:

```
Compressed File  ──►  Full-Resolution Decode  ──►  Downsample / Thumbnail  ──►  Render
(e.g. 5 MB JPEG)      (e.g. 192 MB RGBA buffer)     (e.g. 320 KB RGBA buffer)
```

### Format Constraints on Direct Downscaling
1. **JPEG (DCT Scaling)**:
   - JPEG stores data in 8×8 discrete cosine transform (DCT) blocks.
   - Decoders that expose IDCT scale options (such as `libjpeg-turbo` or `zune-jpeg`'s internal scale settings) can compute only the top-left 1×1, 2×2, or 4×4 coefficients, natively producing 1/8, 1/4, or 1/2 scale images during IDCT.
   - At 1/8th scale, memory allocation is reduced by a factor of 64×, and decoding speed increases by 4–8×.
   - *Current limitation*: The general `image::ImageDecoder` trait in `image` (0.25) does not expose scale options; its `read_image` requires a buffer sized to full native dimensions (`total_bytes()`).
2. **Lossless Formats (PNG, BMP, QOI)**:
   - PNG uses DEFLATE compression and scanline filtering (`Sub`, `Up`, `Average`, `Paeth`). Every row mathematically depends on the preceding row. Downscaling during decompression without unfiltering previous rows is not possible in standard PNG.
3. **Platform-Native Decoders**:
   - On macOS, `CGImageSourceCreateThumbnailAtIndex` with `kCGImageSourceThumbnailMaxPixelSize` can decode directly to thumbnail dimensions using Apple's ImageIO framework.

### Why Duscape Uses the Current Pipeline
- **Uniform Multi-Format Engine**: Duscape supports 11 image formats identically across terminal, Windows, Linux, and macOS without format-specific branching.
- **Strict Bounds Pre-Empt Resource Exhaustion**: The combination of `MAX_IMAGE_BYTES` (disk cap), `image::Limits` (dimension cap of 40,000 px and memory cap of 512 MiB–1 GiB), and thumbnail downscaling keeps memory usage strictly bounded while preserving simplicity and small binary sizes.

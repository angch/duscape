export {};

const routes = {
  linux: { label: "LINUX", summary: "getdents64 + statx, in parallel; optionally read ext4 metadata from the device as root.", scanner: "Linux walker", note: "directory batches from getdents64 + statx", viewer: "Linux window / TUI", lane: "Walker workers read directories. A focus hint moves the folder currently on screen to the front of the queue.", walkerWorkers: "WORKER POOL · 3/CORE, MAX 32", builderWorkers: "BUILDERS · 4 SHARDS", builderCount: 4 },
  macos: { label: "MACOS", summary: "getattrlistbulk returns directory entries in batches; a saved scan can stream, catch up through FSEvents, then fill.", scanner: "getattrlistbulk", note: "bulk entries; one shard where the walk dominates", viewer: "AppKit window / TUI", lane: "The bulk walker returns names and sizes together. A recorder streams a compact saved scan beside the walk.", walkerWorkers: "WORKER POOL · UP TO 6", builderWorkers: "BUILDERS · 1 SHARD", builderCount: 1 },
  windows: { label: "WINDOWS", summary: "Bulk directory handles for subtrees; a sampled NTFS MFT path for eligible whole-volume scans.", scanner: "Windows walker / MFT", note: "bulk directory handles; conditional table read", viewer: "Win32 / GDI window / TUI", lane: "Directory handles keep the general path. Elevated whole-volume scans can read the MFT sequentially when the sample predicts a win.", walkerWorkers: "WORKER POOL · 2/3 CORES, MAX 12", builderWorkers: "BUILDERS · 1 SHARD", builderCount: 1 }
};
const scanners = {
  linux: { index: "01", chip: "LINUX / GENERIC PATH", title: "A directory stream, then a stat per entry.", lead: "Linux has no call that returns names and sizes together. duscape reads names with getdents64 and asks statx for sizes, identity and link counts. A worker pool overlaps that per-entry work across directories.", fast: "Three workers per core keep cold reads in flight; directory batches feed sharded tree builders. A later FIEMAP refinement checks small-file shared extents on XFS and btrfs.", tradeoff: "The generic path pays one metadata call per entry. Raw ext4 reads can be stale by seconds, require privilege, and are not faster warm on every machine.", mechanism: "getdents64 → statx → DirEntries → parallel::build_tree", foot: "Filesystem checks prevent duplicate bind mounts, pseudo filesystems and network mounts from masquerading as local space." },
  macos: { index: "02", chip: "MACOS / APFS", title: "One bulk call returns the names and their useful facts.", lead: "getattrlistbulk reads a directory without crossing its mount points. The parser checks each returned-attribute bitmap before reading a field; a missing attribute can shift the record or silently erase a subtree.", fast: "Six workers are the measured default on APFS. Each directory is a batch, and the first scan records a trimmed stream while it walks; later launches show it immediately and catch up behind it.", tradeoff: "The walker is specific to macOS. APFS metadata reads dominate, and more threads contend in the kernel. A saved view may be stale until FSEvents catch-up completes.", mechanism: "getattrlistbulk → DirEntries → Recorder / FileTree", foot: "The root walk skips duplicate routes into the same data volume while preserving firmlinks, which are the intentional route to user files." },
  windows: { index: "03", chip: "WINDOWS / NTFS", title: "Handles for folders; the MFT when the whole volume fits.", lead: "The standard walker reads directory entries in bulk from one handle per directory. MFT records contribute each $FILE_NAME parent/name and unnamed $DATA size; hard links are already represented by their names.", fast: "On an elevated volume-root scan, a flushed NTFS MFT can replace per-directory kernel work. The sample gate is at most 8 entries per directory: the MFT costs every record, while a normal walk costs a handle per directory.", tradeoff: "The MFT path needs elevation and uses substantially more memory. It loses on subtrees and large-file data volumes, so the same machine may take a different path by scan root.", mechanism: "FileIdExtdDirectoryInfo → DirEntries   |   volume root → $MFT", foot: "A junction, symlink or mounted volume is not followed by the Windows walker; NTFS metadata files can be accounted at a volume root when elevated." },
  volume: { index: "04", chip: "LINUX EXT4 / WINDOWS NTFS", title: "Read the filesystem's own index when it pays.", lead: "On ext4, a root-only reader walks inode tables and directory blocks in device order. On Windows, the MFT reader streams NTFS records. Both still produce the ordinary per-directory scan protocol for the model.", fast: "Sequential metadata reads can replace millions of small kernel queries. The ext4 inode survey was 0.20 s versus a 1.77 s walk on the VM; NTFS MFT was 3.7 s versus 6.1 s warm on C:\\.", tradeoff: "Privileges, filesystem, root scope and device behavior all matter. The ext4 path can be briefly stale; the NTFS path can use about 600 MB for 2.5M records and is not attractive for a subtree.", mechanism: "device blocks / $MFT → parse → per-directory batches", foot: "Specialized paths are conditional accelerators, never a different accounting model." },
  portable: { index: "05", chip: "PORTABLE FALLBACK", title: "Keep the seam even where there is no native walker.", lead: "The fallback adapts the portable dua-core walk into the same per-directory batches that the shared tree consumes. The handoff keeps platform-specific scanning out of viewer code.", fast: "Directory grouping batches entries and lets the common model resolve a parent once per batch instead of once per file. It is the compatibility path for platforms without a native scanner.", tradeoff: "It cannot use Linux, macOS or Windows metadata shortcuts. Platform-gated code can compile without running, so tests call the fallback grouping logic directly on every platform.", mechanism: "dua-core walk → group_by_directory → DirEntries", foot: "A regression once dropped the first entry of each directory because an iterator accumulator reset between calls; tests now assert every item arrives exactly once." }
};

const platformButtons = document.querySelectorAll("[data-platform]");
const focusButtons = document.querySelectorAll("[data-focus]");
const scannerButtons = document.querySelectorAll("[data-scanner]");
const routeIndex = document.querySelector("#routeIndex");
const routeSummary = document.querySelector("#routeSummary");
const scannerNode = document.querySelector("#scannerNode");
const scannerNodeNote = document.querySelector("#scannerNodeNote");
const viewerNode = document.querySelector("#viewerNode");
const ioLane = document.querySelector("#ioLane");
const walkerWorkers = document.querySelector("#walkerWorkers");
const builderWorkers = document.querySelector("#builderWorkers");
const builderBank = document.querySelector("#builderBank");
const pipeline = document.querySelector("#pipeline");
const detail = document.querySelector("#scanner-detail");
const runButton = document.querySelector("#runTrace");

function setPlatform(platform) {
  const route = routes[platform];
  platformButtons.forEach(button => {
    const active = button.dataset.platform === platform;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  if (routeIndex) routeIndex.textContent = `ROUTE / ${route.label}`;
  if (routeSummary) routeSummary.textContent = route.summary;
  if (scannerNode) scannerNode.textContent = route.scanner;
  if (scannerNodeNote) scannerNodeNote.textContent = route.note;
  if (viewerNode) viewerNode.textContent = route.viewer;
  if (ioLane) ioLane.textContent = route.lane;
  if (walkerWorkers) walkerWorkers.textContent = route.walkerWorkers;
  if (builderWorkers) builderWorkers.textContent = route.builderWorkers;
  if (builderBank) {
    builderBank.dataset.count = String(route.builderCount);
    builderBank.querySelectorAll("[data-builder]").forEach(worker => {
      worker.hidden = Number(worker.dataset.builder) > route.builderCount;
    });
  }
  if (builderBank) {
    builderBank.dataset.count = String(route.builderCount);
    builderBank.querySelectorAll("[data-builder]").forEach(worker => {
      worker.hidden = Number(worker.dataset.builder) > route.builderCount;
    });
  }
  setScanner(platform);
}

function setFocus(focus) {
  focusButtons.forEach(button => {
    const active = button.dataset.focus === focus;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  pipeline?.setAttribute("data-focus", focus);
}

function setScanner(id) {
  const content = scanners[id];
  scannerButtons.forEach(button => {
    const active = button.dataset.scanner === id;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-selected", String(active));
    if (active && detail) detail.setAttribute("aria-labelledby", button.id);
  });
  if (!detail) return;
  detail.dataset.scanner = id;
  const bindings = {
    scannerChip: content.chip,
    scannerIndex: `SCANNERS · ${content.index}`,
    scannerTitle: content.title,
    scannerLead: content.lead,
    scannerFast: content.fast,
    scannerTradeoff: content.tradeoff,
    scannerMechanism: content.mechanism,
    scannerFoot: content.foot
  };
  Object.entries(bindings).forEach(([elementId, value]) => {
    const element = document.getElementById(elementId);
    if (element) element.textContent = value;
  });
}

platformButtons.forEach(button => button.addEventListener("click", () => setPlatform(button.dataset.platform)));
focusButtons.forEach(button => button.addEventListener("click", () => setFocus(button.dataset.focus)));
scannerButtons.forEach(button => button.addEventListener("click", () => setScanner(button.dataset.scanner)));
runButton?.addEventListener("click", () => {
  if (!pipeline || !runButton) return;
  pipeline.classList.remove("is-running");
  void pipeline.offsetWidth;
  pipeline.classList.add("is-running");
  runButton.classList.add("is-running");
  window.setTimeout(() => {
    pipeline.classList.remove("is-running");
    runButton.classList.remove("is-running");
  }, 3600);
});

const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
if (!reducedMotion.matches && "IntersectionObserver" in window) {
  document.documentElement.classList.add("has-reveal");
  const observer = new IntersectionObserver(entries => entries.forEach(entry => {
    if (entry.isIntersecting) {
      entry.target.classList.add("in-view");
      observer.unobserve(entry.target);
    }
  }), { threshold: .12 });
  document.querySelectorAll(".section-heading,.principle-strip,.scan-family-note").forEach(item => observer.observe(item));
}
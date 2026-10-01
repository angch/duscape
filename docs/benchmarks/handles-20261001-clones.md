# handles — 2026-10-01

| | |
| --- | --- |
| machine | Apple M4 Pro, 14 (10 performance) cores, 48 GiB, virtualisation: none |
| system | macOS 26.6.2 (25G83), kernel 25.6.0 |
| duscape | 521fc64 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| WizTreeMac | 1.0.0, timed in export mode (folders only, through `open`; an empty folder's export takes 0.25 s); no root row, its admin mode asks for a password |
| cold runs | no: cannot drop caches without root |
| root runs | no: sudo -n /Users/angch/project/duscape/target/release/duscape not allowed |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `/Users/angch/project` | APFS | /dev/disk3s5 | /dev/disk0 (APPLE SSD AP1024Z) SSD, Apple Fabric | 3201958, 28449 hard-linked | 121.3 GiB |

WizTreeMac's figures for the same trees (files and folders, allocated):

| tree | entries | allocated |
| --- | --- | --- |
| `/Users/angch/project` | 3201958 | 121.3 GiB (130237747200 B) |

## Timings

### warm: /Users/angch/project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 15.493 ± 0.144 | 15.327 | 15.582 | 1.54 ± 0.03 |
| `duscape sharded` | 10.058 ± 0.190 | 9.947 | 10.277 | 1.00 |
| `duscape refined` | 10.282 ± 0.133 | 10.161 | 10.424 | 1.02 ± 0.02 |
| `WizTreeMac export` | 40.134 ± 0.224 | 39.905 | 40.352 | 3.99 ± 0.08 |

## Build profile (tree-only, warm)

### /Users/angch/project

```
  build profile: 344973 directories, 3201971 entries, 0.190s of builder time
    resolve 0.066s  7.0 folders/dir  27.0 name compares/dir  2181 indexes built
    place   0.097s  30 ns/entry
    sizes   0.016s  5 ns/entry over the 327081 directories with no shared blocks
    ledger  0.012s  17892 directories, 386113 sightings  1.1 link comparisons and 3.5 ancestor steps each
tree-only     0.257s      3201971 entries    12457185 entries/s        0 unreadable   121.3 GiB (130238390272 B)  28449 hard-linked  147703 reflinked
```


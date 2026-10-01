# handles — 2026-10-01

| | |
| --- | --- |
| machine | Apple M4 Pro, 14 (10 performance) cores, 48 GiB, virtualisation: none |
| system | macOS 26.6.2 (25G83), kernel 25.6.0 |
| duscape | 521fc64 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| WizTreeMac | 1.0.0, timed in export mode (folders only, through `open`; an empty folder's export takes 0.29 s); no root row, its admin mode asks for a password |
| cold runs | no: cannot drop caches without root |
| root runs | no: sudo -n /private/tmp/claude-501/-Users-angch-project-duscape/7044b2a6-54e2-4ad9-a99b-02aad9f53818/scratchpad/duscape-before not allowed |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `/Users/angch/project` | APFS | /dev/disk3s5 | /dev/disk0 (APPLE SSD AP1024Z) SSD, Apple Fabric | 3201953, 28450 hard-linked | 122.6 GiB |

WizTreeMac's figures for the same trees (files and folders, allocated):

| tree | entries | allocated |
| --- | --- | --- |
| `/Users/angch/project` | 3201953 | 121.3 GiB (130237423616 B) |

## Timings

### warm: /Users/angch/project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 15.462 ± 0.084 | 15.372 | 15.538 | 1.70 ± 0.02 |
| `duscape sharded` | 9.103 ± 0.099 | 9.018 | 9.212 | 1.00 |
| `duscape refined` | 9.168 ± 0.089 | 9.068 | 9.240 | 1.01 ± 0.01 |
| `WizTreeMac export` | 39.914 ± 0.026 | 39.890 | 39.941 | 4.38 ± 0.05 |

## Build profile (tree-only, warm)

### /Users/angch/project

```
  build profile: 344967 directories, 3201957 entries, 0.180s of builder time
    resolve 0.065s  7.0 folders/dir  27.0 name compares/dir  2181 indexes built
    place   0.096s  30 ns/entry
    sizes   0.017s  5 ns/entry over the 344466 directories with no shared blocks
    ledger  0.003s  501 directories, 153934 sightings  1.8 link comparisons and 5.0 ancestor steps each
tree-only     0.248s      3201957 entries    12936460 entries/s        0 unreadable   122.6 GiB (131655221248 B)  28450 hard-linked
```


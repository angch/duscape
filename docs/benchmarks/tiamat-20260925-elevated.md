# tiamat — 2026-09-25

| | |
| --- | --- |
| machine | AMD Ryzen 9 7950X 16-Core Processor, 32 cores, 127 GiB, virtualisation: hypervisor present (a Hyper-V or WSL2 host, most likely) |
| system | Microsoft Windows 11 Home 10.0.26200 |
| diskonaut | 76d2d77 (clean), release profile |
| diskus | diskus 0.9.0 |
| WizTree | WizTree 4.32.0.0, timed in export mode (folders only) |
| cold runs | no: Windows has no way to drop the file cache from a script |
| elevated runs | yes: this shell is elevated (every row) |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `C:\Users\angch\project` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 132054, 7215 hard-linked | 69.0 GiB |
| `C:\Users\angch` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 1605003, 57019 hard-linked | 897.5 GiB |
| `C:\` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 2464212, 102823 hard-linked | 1.3 TiB |
| `D:\` | NTFS | D: | disk 1 (KINGSTON SKC3000D4096G) SSD, NVMe | 414601 | 3.0 TiB |

WizTree's figures for the same trees (files and folders, allocated):

| tree | entries | allocated |
| --- | --- | --- |
| `C:\Users\angch\project` | 132054 | 71.1 GiB (76377989120 B) |
| `C:\Users\angch` | 1605002 | 904.9 GiB (971610677248 B) |
| `C:\` | 2464240 | 1349.9 GiB (1449488379904 B) |
| `D:\` | 414613 | 3072.4 GiB (3298987806720 B) |

## Timings

### warm: C:\Users\angch\project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 1.957 ± 0.026 | 1.930 | 1.983 | 9.61 ± 0.23 |
| `diskonaut sharded` | 0.213 ± 0.014 | 0.200 | 0.228 | 1.04 ± 0.07 |
| `diskonaut refined` | 0.204 ± 0.004 | 0.200 | 0.208 | 1.00 |
| `WizTree export` | 0.861 ± 0.009 | 0.852 | 0.869 | 4.23 ± 0.09 |

### warm: C:\Users\angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 22.468 ± 0.091 | 22.403 | 22.572 | 8.58 ± 0.13 |
| `diskonaut sharded` | 2.620 ± 0.038 | 2.585 | 2.661 | 1.00 |
| `diskonaut refined` | 2.656 ± 0.165 | 2.545 | 2.845 | 1.01 ± 0.06 |
| `WizTree export` | 6.871 ± 1.049 | 6.077 | 8.060 | 2.62 ± 0.40 |

### warm: C:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 36.001 ± 0.104 | 35.924 | 36.119 | 6.60 ± 0.10 |
| `diskonaut sharded` | 5.495 ± 0.034 | 5.460 | 5.528 | 1.01 ± 0.02 |
| `diskonaut refined` | 5.455 ± 0.078 | 5.402 | 5.544 | 1.00 |
| `WizTree export` | 6.430 ± 0.107 | 6.309 | 6.510 | 1.18 ± 0.03 |

### warm: D:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 5.304 ± 0.141 | 5.156 | 5.436 | 34.40 ± 1.71 |
| `diskonaut sharded` | 0.157 ± 0.016 | 0.145 | 0.175 | 1.02 ± 0.11 |
| `diskonaut refined` | 0.154 ± 0.006 | 0.150 | 0.162 | 1.00 |
| `WizTree export` | 1.706 ± 0.021 | 1.689 | 1.730 | 11.07 ± 0.49 |

## Build profile (tree-only, warm)

### C:\Users\angch\project

```
  build profile: 15387 directories, 132054 entries, 0.024s of builder time
    resolve 0.005s  5.7 folders/dir  24.1 name compares/dir  137 indexes built
    place   0.008s  59 ns/entry
    sizes   0.000s  5 ns/entry over the 2308 directories with no shared blocks
    ledger  0.012s  13079 directories, 114966 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.030s       132054 entries     4450128 entries/s        0 unreadable    69.0 GiB (74138012056 B)  7215 hard-linked
```

### C:\Users\angch

```
  build profile: 206045 directories, 1605003 entries, 0.403s of builder time
    resolve 0.081s  8.3 folders/dir  19.1 name compares/dir  949 indexes built
    place   0.110s  68 ns/entry
    sizes   0.003s  6 ns/entry over the 63237 directories with no shared blocks
    ledger  0.209s  142808 directories, 1368572 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.478s      1605003 entries     3360093 entries/s        2 unreadable   897.5 GiB (963728417792 B)  57019 hard-linked
```

### C:\

```
  volume used: 1.3 TiB (1451578822656 B)
  build profile: 456047 directories, 2464214 entries, 0.738s of builder time
    resolve 0.197s  8.1 folders/dir  14.3 name compares/dir  1549 indexes built
    place   0.210s  85 ns/entry
    sizes   0.011s  8 ns/entry over the 215462 directories with no shared blocks
    ledger  0.320s  240585 directories, 1967703 sightings  0.1 link comparisons and 0.5 ancestor steps each
tree-only     0.879s      2464214 entries     2802080 entries/s        2 unreadable     1.3 TiB (1449546156936 B)  102823 hard-linked
```

### D:\

```
  volume used: 3.0 TiB (3299103612928 B)
  build profile: 25538 directories, 414601 entries, 0.075s of builder time
    resolve 0.008s  7.5 folders/dir  23.7 name compares/dir  420 indexes built
    place   0.023s  55 ns/entry
    sizes   0.000s  5 ns/entry over the 2953 directories with no shared blocks
    ledger  0.044s  22585 directories, 387708 sightings  0.0 link comparisons and 0.0 ancestor steps each
tree-only     0.082s       414601 entries     5073229 entries/s        0 unreadable     3.0 TiB (3298957672328 B)
```


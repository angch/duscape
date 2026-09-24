# tiamat — 2026-09-25

| | |
| --- | --- |
| machine | AMD Ryzen 9 7950X 16-Core Processor, 32 cores, 127 GiB, virtualisation: hypervisor present (a Hyper-V or WSL2 host, most likely) |
| system | Microsoft Windows 11 Home 10.0.26200 |
| diskonaut | 32f7935 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| WizTree | WizTree 4.32.0.0, timed in export mode (folders only) |
| cold runs | yes (the file cache emptied before each: drop-cache.ps1) |
| elevated runs | yes: this shell is elevated (every row) |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `C:\Users\angch\project` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 132067, 7215 hard-linked | 69.0 GiB |
| `C:\Users\angch` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 1605030, 57019 hard-linked | 897.5 GiB |
| `C:\` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 2464240, 102823 hard-linked | 1.3 TiB |
| `D:\` | NTFS | D: | disk 1 (KINGSTON SKC3000D4096G) SSD, NVMe | 414601 | 3.0 TiB |

WizTree's figures for the same trees (files and folders, allocated):

| tree | entries | allocated |
| --- | --- | --- |
| `C:\Users\angch\project` | 132067 | 71.1 GiB (76378091520 B) |
| `C:\Users\angch` | 1605030 | 904.9 GiB (971607564288 B) |
| `C:\` | 2464264 | 1350.2 GiB (1449735073792 B) |
| `D:\` | 414613 | 3072.4 GiB (3298987806720 B) |

## Timings

### warm: C:\Users\angch\project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 1.873 ± 0.022 | 1.855 | 1.898 | 10.18 ± 0.21 |
| `diskonaut sharded` | 0.187 ± 0.007 | 0.180 | 0.195 | 1.01 ± 0.04 |
| `diskonaut refined` | 0.184 ± 0.003 | 0.182 | 0.188 | 1.00 |
| `WizTree export` | 0.859 ± 0.070 | 0.812 | 0.940 | 4.67 ± 0.39 |

### cold: C:\Users\angch\project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 2.033 ± 0.084 | 1.949 | 2.117 | 6.06 ± 0.27 |
| `diskonaut sharded` | 0.341 ± 0.016 | 0.331 | 0.360 | 1.02 ± 0.05 |
| `diskonaut refined` | 0.336 ± 0.005 | 0.330 | 0.341 | 1.00 |
| `WizTree export` | 0.950 ± 0.011 | 0.941 | 0.963 | 2.83 ± 0.05 |

### warm: C:\Users\angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 22.485 ± 0.107 | 22.378 | 22.593 | 8.64 ± 0.09 |
| `diskonaut sharded` | 2.773 ± 0.270 | 2.590 | 3.083 | 1.07 ± 0.10 |
| `diskonaut refined` | 2.603 ± 0.025 | 2.576 | 2.623 | 1.00 |
| `WizTree export` | 6.677 ± 1.125 | 5.516 | 7.762 | 2.57 ± 0.43 |

### cold: C:\Users\angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 24.938 ± 0.381 | 24.639 | 25.367 | 6.35 ± 0.14 |
| `diskonaut sharded` | 3.928 ± 0.059 | 3.867 | 3.985 | 1.00 |
| `diskonaut refined` | 4.023 ± 0.256 | 3.870 | 4.319 | 1.02 ± 0.07 |
| `WizTree export` | 12.099 ± 2.790 | 9.103 | 14.623 | 3.08 ± 0.71 |

### warm: C:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 45.417 ± 0.821 | 44.477 | 45.997 | 8.10 ± 0.25 |
| `diskonaut sharded` | 5.604 ± 0.143 | 5.506 | 5.769 | 1.00 |
| `diskonaut refined` | 5.948 ± 0.369 | 5.603 | 6.338 | 1.06 ± 0.07 |
| `WizTree export` | 6.761 ± 0.347 | 6.393 | 7.082 | 1.21 ± 0.07 |

### cold: C:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 47.254 ± 0.727 | 46.544 | 47.997 | 7.58 ± 0.26 |
| `diskonaut sharded` | 8.332 ± 0.252 | 8.079 | 8.582 | 1.34 ± 0.06 |
| `diskonaut refined` | 8.261 ± 0.210 | 8.027 | 8.432 | 1.32 ± 0.05 |
| `WizTree export` | 6.236 ± 0.194 | 6.070 | 6.450 | 1.00 |

### warm: D:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 5.792 ± 0.261 | 5.503 | 6.011 | 36.64 ± 1.87 |
| `diskonaut sharded` | 0.161 ± 0.007 | 0.156 | 0.169 | 1.02 ± 0.05 |
| `diskonaut refined` | 0.158 ± 0.004 | 0.154 | 0.162 | 1.00 |
| `WizTree export` | 1.723 ± 0.039 | 1.691 | 1.766 | 10.90 ± 0.36 |

### cold: D:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 6.640 ± 0.136 | 6.510 | 6.783 | 16.08 ± 0.42 |
| `diskonaut sharded` | 0.413 ± 0.007 | 0.405 | 0.418 | 1.00 |
| `diskonaut refined` | 0.422 ± 0.004 | 0.418 | 0.425 | 1.02 ± 0.02 |
| `WizTree export` | 1.733 ± 0.010 | 1.727 | 1.744 | 4.20 ± 0.07 |

## Build profile (tree-only, warm)

### C:\Users\angch\project

```
  build profile: 15393 directories, 132067 entries, 0.027s of builder time
    resolve 0.005s  5.7 folders/dir  24.1 name compares/dir  137 indexes built
    place   0.008s  63 ns/entry
    sizes   0.000s  6 ns/entry over the 2308 directories with no shared blocks
    ledger  0.013s  13085 directories, 114973 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.034s       132067 entries     3918890 entries/s        0 unreadable    69.0 GiB (74138115048 B)  7215 hard-linked
```

### C:\Users\angch

```
  build profile: 206053 directories, 1605044 entries, 0.435s of builder time
    resolve 0.087s  8.3 folders/dir  19.0 name compares/dir  949 indexes built
    place   0.119s  74 ns/entry
    sizes   0.004s  6 ns/entry over the 63236 directories with no shared blocks
    ledger  0.225s  142817 directories, 1368605 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.516s      1605044 entries     3110230 entries/s        2 unreadable   897.5 GiB (963716486248 B)  57019 hard-linked
```

### C:\

```
  volume used: 1.3 TiB (1451172126720 B)
  build profile: 456169 directories, 2465225 entries, 1.008s of builder time
    resolve 0.258s  8.1 folders/dir  14.3 name compares/dir  1550 indexes built
    place   0.283s  115 ns/entry
    sizes   0.013s  10 ns/entry over the 215509 directories with no shared blocks
    ledger  0.453s  240660 directories, 1968592 sightings  0.1 link comparisons and 0.5 ancestor steps each
tree-only     1.195s      2465225 entries     2063560 entries/s        2 unreadable     1.3 TiB (1449692330688 B)  103546 hard-linked
```

### D:\

```
  volume used: 3.0 TiB (3299103612928 B)
  build profile: 25538 directories, 414601 entries, 0.074s of builder time
    resolve 0.008s  7.5 folders/dir  24.1 name compares/dir  420 indexes built
    place   0.023s  55 ns/entry
    sizes   0.000s  5 ns/entry over the 2953 directories with no shared blocks
    ledger  0.043s  22585 directories, 387708 sightings  0.0 link comparisons and 0.0 ancestor steps each
tree-only     0.081s       414601 entries     5127133 entries/s        0 unreadable     3.0 TiB (3298957672328 B)
```


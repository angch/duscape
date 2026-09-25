# tiamat — 2026-09-25

| | |
| --- | --- |
| machine | AMD Ryzen 9 7950X 16-Core Processor, 32 cores, 127 GiB, virtualisation: hypervisor present (a Hyper-V or WSL2 host, most likely) |
| system | Microsoft Windows 11 Home 10.0.26200 |
| diskonaut | 9ce6961 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| WizTree | WizTree 4.32.0.0, timed in export mode (folders only) |
| cold runs | yes (the file cache emptied before each: drop-cache.ps1) |
| elevated runs | yes: this shell is elevated (every row) |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `C:\Users\angch\project` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 132622, 7690 hard-linked | 69.0 GiB |
| `C:\Users\angch` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 1606000, 57149 hard-linked | 897.7 GiB |
| `C:\` | NTFS | C: | disk 0 (Samsung SSD 990 PRO 2TB) SSD, NVMe | 2466142, 104728 hard-linked | 1.3 TiB |
| `D:\` | NTFS | D: | disk 1 (KINGSTON SKC3000D4096G) SSD, NVMe | 414679 | 3.0 TiB |

WizTree's figures for the same trees (files and folders, allocated):

| tree | entries | allocated |
| --- | --- | --- |
| `C:\Users\angch\project` | 133010 | 71.3 GiB (76591157248 B) |
| `C:\Users\angch` | 1605744 | 905.1 GiB (971827073024 B) |
| `C:\` | 2466144 | 1353.5 GiB (1453311623168 B) |
| `D:\` | 414691 | 3073.9 GiB (3300538232832 B) |

## Timings

### warm: C:\Users\angch\project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 2.022 ± 0.033 | 1.998 | 2.060 | 10.19 ± 0.21 |
| `diskonaut sharded` | 0.199 ± 0.002 | 0.196 | 0.201 | 1.00 |
| `diskonaut refined` | 0.202 ± 0.004 | 0.198 | 0.207 | 1.02 ± 0.03 |
| `diskonaut sharded, kernel walk` | 0.212 ± 0.013 | 0.201 | 0.226 | 1.07 ± 0.07 |
| `WizTree export` | 0.985 ± 0.145 | 0.894 | 1.152 | 4.96 ± 0.73 |

### cold: C:\Users\angch\project

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 2.486 ± 0.348 | 2.252 | 2.886 | 6.90 ± 0.97 |
| `diskonaut sharded` | 0.423 ± 0.025 | 0.395 | 0.442 | 1.17 ± 0.07 |
| `diskonaut refined` | 0.360 ± 0.007 | 0.353 | 0.366 | 1.00 |
| `diskonaut sharded, kernel walk` | 0.363 ± 0.009 | 0.353 | 0.369 | 1.01 ± 0.03 |
| `WizTree export` | 1.012 ± 0.016 | 0.999 | 1.030 | 2.81 ± 0.07 |

### warm: C:\Users\angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 27.215 ± 1.103 | 26.463 | 28.482 | 9.51 ± 0.76 |
| `diskonaut sharded` | 3.193 ± 0.468 | 2.668 | 3.567 | 1.12 ± 0.18 |
| `diskonaut refined` | 2.862 ± 0.198 | 2.682 | 3.075 | 1.00 |
| `diskonaut sharded, kernel walk` | 2.973 ± 0.400 | 2.673 | 3.427 | 1.04 ± 0.16 |
| `WizTree export` | 6.624 ± 0.952 | 5.946 | 7.712 | 2.31 ± 0.37 |

### cold: C:\Users\angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 28.947 ± 0.994 | 28.097 | 30.040 | 6.90 ± 0.39 |
| `diskonaut sharded` | 4.294 ± 0.175 | 4.182 | 4.496 | 1.02 ± 0.06 |
| `diskonaut refined` | 4.198 ± 0.189 | 4.019 | 4.395 | 1.00 |
| `diskonaut sharded, kernel walk` | 4.246 ± 0.054 | 4.208 | 4.307 | 1.01 ± 0.05 |
| `WizTree export` | 11.827 ± 1.288 | 10.570 | 13.144 | 2.82 ± 0.33 |

### warm: C:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 43.333 ± 1.403 | 42.423 | 44.949 | 11.76 ± 0.48 |
| `diskonaut sharded` | 3.685 ± 0.092 | 3.580 | 3.748 | 1.00 |
| `diskonaut refined` | 3.760 ± 0.075 | 3.705 | 3.846 | 1.02 ± 0.03 |
| `diskonaut sharded, kernel walk` | 6.068 ± 0.193 | 5.926 | 6.288 | 1.65 ± 0.07 |
| `WizTree export` | 7.052 ± 0.293 | 6.881 | 7.390 | 1.91 ± 0.09 |

### cold: C:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 45.435 ± 1.960 | 43.888 | 47.640 | 15.07 ± 0.66 |
| `diskonaut sharded` | 3.497 ± 0.722 | 3.063 | 4.330 | 1.16 ± 0.24 |
| `diskonaut refined` | 3.014 ± 0.018 | 2.993 | 3.027 | 1.00 |
| `diskonaut sharded, kernel walk` | 9.118 ± 0.054 | 9.073 | 9.179 | 3.03 ± 0.03 |
| `WizTree export` | 6.726 ± 0.421 | 6.333 | 7.171 | 2.23 ± 0.14 |

### warm: D:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 5.952 ± 0.257 | 5.701 | 6.214 | 34.71 ± 1.69 |
| `diskonaut sharded` | 0.532 ± 0.138 | 0.443 | 0.691 | 3.10 ± 0.81 |
| `diskonaut refined` | 0.543 ± 0.145 | 0.458 | 0.711 | 3.16 ± 0.85 |
| `diskonaut sharded, kernel walk` | 0.171 ± 0.004 | 0.168 | 0.176 | 1.00 |
| `WizTree export` | 1.875 ± 0.033 | 1.840 | 1.905 | 10.93 ± 0.31 |

### cold: D:\

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 7.172 ± 0.286 | 6.939 | 7.492 | 15.12 ± 0.63 |
| `diskonaut sharded` | 0.497 ± 0.007 | 0.489 | 0.503 | 1.05 ± 0.02 |
| `diskonaut refined` | 0.494 ± 0.005 | 0.491 | 0.500 | 1.04 ± 0.02 |
| `diskonaut sharded, kernel walk` | 0.474 ± 0.006 | 0.469 | 0.481 | 1.00 |
| `WizTree export` | 1.905 ± 0.020 | 1.885 | 1.924 | 4.02 ± 0.07 |

## Build profile (tree-only, warm)

### C:\Users\angch\project

```
  build profile: 15426 directories, 132305 entries, 0.027s of builder time
    resolve 0.005s  5.7 folders/dir  24.2 name compares/dir  139 indexes built
    place   0.009s  68 ns/entry
    sizes   0.000s  6 ns/entry over the 2315 directories with no shared blocks
    ledger  0.013s  13111 directories, 115162 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.033s       132305 entries     3984766 entries/s        0 unreadable    69.1 GiB (74226542304 B)  7179 hard-linked
```

### C:\Users\angch

```
  build profile: 206127 directories, 1605757 entries, 0.444s of builder time
    resolve 0.081s  8.3 folders/dir  19.1 name compares/dir  951 indexes built
    place   0.127s  79 ns/entry
    sizes   0.003s  6 ns/entry over the 63251 directories with no shared blocks
    ledger  0.233s  142876 directories, 1369219 sightings  0.1 link comparisons and 0.3 ancestor steps each
tree-only     0.520s      1605757 entries     3090071 entries/s        2 unreadable   897.7 GiB (963846929872 B)  56983 hard-linked
```

### C:\

```
  volume used: 1.3 TiB (1454643388416 B)
  mft: 2.3G of table (flushed) read and parsed in 1.194s, 336780 directories assembled by 2.118s; 2.2 entries a directory in the sample
  build profile: 456253 directories, 2466153 entries, 0.570s of builder time
    resolve 0.261s  8.1 folders/dir  18.4 name compares/dir  1553 indexes built
    place   0.242s  98 ns/entry
    sizes   0.035s  15 ns/entry over the 419409 directories with no shared blocks
    ledger  0.033s  36844 directories, 253116 sightings  0.9 link comparisons and 3.9 ancestor steps each
tree-only     0.708s      2466153 entries     3485374 entries/s        0 unreadable     1.3 TiB (1453239488512 B)  104728 hard-linked
```

### D:\

```
  volume used: 3.0 TiB (3300654055424 B)
  build profile: 25555 directories, 414679 entries, 0.081s of builder time
    resolve 0.008s  7.5 folders/dir  24.1 name compares/dir  420 indexes built
    place   0.025s  60 ns/entry
    sizes   0.000s  5 ns/entry over the 2961 directories with no shared blocks
    ledger  0.048s  22594 directories, 387769 sightings  0.0 link comparisons and 0.0 ancestor steps each
tree-only     0.088s       414679 entries     4708500 entries/s        0 unreadable     3.0 TiB (3300508098608 B)
```


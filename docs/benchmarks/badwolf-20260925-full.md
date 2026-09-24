# badwolf — 2026-09-25

| | |
| --- | --- |
| machine | AMD Ryzen 7 7735HS with Radeon Graphics, 16 cores, 25 GiB, virtualisation: none |
| system | Ubuntu 26.04.1 LTS, kernel 7.0.0-29-generic |
| diskonaut | 3e0d905 (clean), release profile |
| diskus | diskus 0.9.0 |
| cold runs | yes (caches dropped before each) |
| root runs | yes |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `/home/angch/project` | ext4 | /dev/nvme0n1p2 | /dev/nvme0n1 (SAMSUNG MZAL81T0HDLB-00BL2) SSD, nvme | 20840, 1850 hard-linked | 3.8 GiB |
| `/home/angch/.cache` | ext4 | /dev/nvme0n1p2 | /dev/nvme0n1 (SAMSUNG MZAL81T0HDLB-00BL2) SSD, nvme | 103851, 30844 hard-linked | 11.1 GiB |
| `/home/angch` | ext4 | /dev/nvme0n1p2 | /dev/nvme0n1 (SAMSUNG MZAL81T0HDLB-00BL2) SSD, nvme | 673194, 49842 hard-linked | 94.2 GiB |
| `/usr` | ext4 | /dev/nvme0n1p2 | /dev/nvme0n1 (SAMSUNG MZAL81T0HDLB-00BL2) SSD, nvme | 289668, 15 hard-linked | 10.9 GiB |

## Timings

### warm: /home/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 12.1 ± 0.6 | 11.7 | 12.8 | 1.00 |
| `diskonaut sharded` | 12.4 ± 0.8 | 11.6 | 13.1 | 1.03 ± 0.08 |
| `diskonaut refined` | 13.2 ± 1.5 | 11.9 | 14.8 | 1.09 ± 0.13 |
| `diskonaut sharded, as root` | 28.0 ± 0.2 | 27.8 | 28.1 | 2.31 ± 0.12 |
| `diskonaut sharded, as root, kernel walk` | 20.9 ± 1.0 | 19.9 | 21.9 | 1.72 ± 0.12 |

### cold: /home/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 38.2 ± 0.4 | 37.7 | 38.6 | 1.00 |
| `diskonaut sharded` | 47.0 ± 2.2 | 45.2 | 49.5 | 1.23 ± 0.06 |
| `diskonaut refined` | 48.0 ± 3.3 | 44.9 | 51.4 | 1.26 ± 0.09 |
| `diskonaut sharded, as root` | 73.6 ± 2.1 | 71.8 | 75.9 | 1.93 ± 0.06 |
| `diskonaut sharded, as root, kernel walk` | 77.4 ± 1.1 | 76.1 | 78.1 | 2.03 ± 0.04 |

### warm: /home/angch/.cache

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 42.0 ± 2.3 | 39.5 | 44.0 | 1.00 |
| `diskonaut sharded` | 49.5 ± 4.3 | 45.3 | 54.0 | 1.18 ± 0.12 |
| `diskonaut refined` | 52.6 ± 1.5 | 50.9 | 53.7 | 1.25 ± 0.08 |
| `diskonaut sharded, as root` | 73.9 ± 0.5 | 73.5 | 74.5 | 1.76 ± 0.10 |
| `diskonaut sharded, as root, kernel walk` | 62.3 ± 3.1 | 60.3 | 65.9 | 1.48 ± 0.11 |

### cold: /home/angch/.cache

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 132.8 ± 3.3 | 129.0 | 134.7 | 1.00 |
| `diskonaut sharded` | 165.7 ± 3.4 | 162.1 | 168.8 | 1.25 ± 0.04 |
| `diskonaut refined` | 171.5 ± 5.6 | 166.7 | 177.7 | 1.29 ± 0.05 |
| `diskonaut sharded, as root` | 156.2 ± 6.3 | 152.1 | 163.5 | 1.18 ± 0.06 |
| `diskonaut sharded, as root, kernel walk` | 218.5 ± 3.1 | 215.7 | 221.8 | 1.64 ± 0.05 |

### warm: /home/angch

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 260.2 ± 3.9 | 256.2 | 264.0 | 1.03 ± 0.02 |
| `diskonaut sharded` | 253.8 ± 3.7 | 251.0 | 258.0 | 1.00 |
| `diskonaut refined` | 255.1 ± 9.9 | 247.8 | 266.3 | 1.01 ± 0.04 |
| `diskonaut sharded, as root` | 343.0 ± 2.5 | 340.9 | 345.7 | 1.35 ± 0.02 |
| `diskonaut sharded, as root, kernel walk` | 280.9 ± 2.0 | 278.9 | 282.9 | 1.11 ± 0.02 |

### cold: /home/angch

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 650.8 ± 21.5 | 634.3 | 675.1 | 1.00 |
| `diskonaut sharded` | 718.8 ± 6.1 | 714.7 | 725.8 | 1.10 ± 0.04 |
| `diskonaut refined` | 724.8 ± 12.1 | 716.0 | 738.6 | 1.11 ± 0.04 |
| `diskonaut sharded, as root` | 691.9 ± 5.2 | 686.7 | 697.0 | 1.06 ± 0.04 |
| `diskonaut sharded, as root, kernel walk` | 753.3 ± 19.4 | 731.1 | 766.8 | 1.16 ± 0.05 |

### warm: /usr

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 110.3 ± 5.3 | 106.6 | 116.4 | 1.00 |
| `diskonaut sharded` | 114.2 ± 1.0 | 113.5 | 115.3 | 1.04 ± 0.05 |
| `diskonaut refined` | 112.6 ± 0.3 | 112.3 | 112.9 | 1.02 ± 0.05 |
| `diskonaut sharded, as root` | 156.0 ± 0.4 | 155.7 | 156.5 | 1.41 ± 0.07 |
| `diskonaut sharded, as root, kernel walk` | 129.0 ± 1.5 | 127.3 | 129.9 | 1.17 ± 0.06 |

### cold: /usr

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 289.8 ± 6.7 | 283.6 | 296.8 | 1.00 |
| `diskonaut sharded` | 316.8 ± 4.4 | 311.7 | 319.5 | 1.09 ± 0.03 |
| `diskonaut refined` | 324.2 ± 6.2 | 317.4 | 329.5 | 1.12 ± 0.03 |
| `diskonaut sharded, as root` | 327.0 ± 3.7 | 324.0 | 331.1 | 1.13 ± 0.03 |
| `diskonaut sharded, as root, kernel walk` | 352.0 ± 12.0 | 339.9 | 363.8 | 1.21 ± 0.05 |

## Build profile (tree-only, warm)

### /home/angch/project

```
  build profile: 2299 directories, 20840 entries, 0.013s of builder time
    resolve 0.004s  5.0 folders/dir  40.8 name compares/dir  33 indexes built
    place   0.005s  244 ns/entry
    sizes   0.003s  164 ns/entry over the 2170 directories with no shared blocks
    ledger  0.001s  129 directories, 3700 sightings  0.5 link comparisons and 3.1 ancestor steps each
tree-only     0.017s        20840 entries     1207446 entries/s        0 unreadable     3.8 GiB (4077875200 B)  1850 hard-linked
```

### /home/angch/.cache

```
  build profile: 7907 directories, 103851 entries, 0.049s of builder time
    resolve 0.014s  7.0 folders/dir  36.7 name compares/dir  24 indexes built
    place   0.018s  171 ns/entry
    sizes   0.004s  111 ns/entry over the 2489 directories with no shared blocks
    ledger  0.014s  5418 directories, 61688 sightings  0.5 link comparisons and 1.5 ancestor steps each
tree-only     0.063s       103851 entries     1651231 entries/s        0 unreadable    11.1 GiB (11920216064 B)  30844 hard-linked
```

### /home/angch

```
  build profile: 76977 directories, 673194 entries, 0.422s of builder time
    resolve 0.137s  8.8 folders/dir  25.8 name compares/dir  531 indexes built
    place   0.164s  244 ns/entry
    sizes   0.095s  160 ns/entry over the 66826 directories with no shared blocks
    ledger  0.026s  10151 directories, 99712 sightings  0.5 link comparisons and 2.6 ancestor steps each
tree-only     0.554s       673194 entries     1215448 entries/s        0 unreadable    94.2 GiB (101199732736 B)  49842 hard-linked
```

### /usr

```
  build profile: 31466 directories, 289668 entries, 0.164s of builder time
    resolve 0.054s  5.4 folders/dir  24.1 name compares/dir  284 indexes built
    place   0.066s  226 ns/entry
    sizes   0.045s  156 ns/entry over the 31458 directories with no shared blocks
    ledger  0.000s  8 directories, 146 sightings  1.7 link comparisons and 0.9 ancestor steps each
tree-only     0.218s       289668 entries     1331659 entries/s        0 unreadable    10.9 GiB (11725815808 B)  15 hard-linked
```


# angch-noble — 2026-09-24

| | |
| --- | --- |
| machine | AMD Ryzen 7 5825U with Radeon Graphics, 8 cores, 27 GiB, virtualisation: kvm |
| system | Ubuntu 24.04.5 LTS, kernel 6.8.0-137-generic |
| diskonaut | 1b918b3 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| cold runs | yes (caches dropped before each) |
| root runs | yes |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `/data/angch/project` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 399426, 6771 hard-linked | 36.9 GiB |
| `/data/angch` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 2246393, 175927 hard-linked | 164.4 GiB |
| `/data/angch/.cache` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 1050436, 169103 hard-linked | 45.4 GiB |

## Timings

### warm: /data/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 238.8 ± 5.5 | 233.0 | 244.1 | 1.06 ± 0.03 |
| `diskonaut sharded` | 228.3 ± 2.1 | 226.0 | 230.1 | 1.01 ± 0.01 |
| `diskonaut refined` | 249.8 ± 25.4 | 231.3 | 278.8 | 1.11 ± 0.11 |
| `diskonaut sharded, as root` | 225.1 ± 2.5 | 222.6 | 227.6 | 1.00 |
| `diskonaut sharded, as root, kernel walk` | 247.7 ± 5.4 | 242.6 | 253.4 | 1.10 ± 0.03 |

### cold: /data/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 777.1 ± 16.2 | 763.4 | 795.0 | 1.95 ± 0.05 |
| `diskonaut sharded` | 767.8 ± 13.1 | 753.2 | 778.5 | 1.93 ± 0.04 |
| `diskonaut refined` | 772.9 ± 18.2 | 756.8 | 792.7 | 1.94 ± 0.05 |
| `diskonaut sharded, as root` | 398.1 ± 4.2 | 395.3 | 402.9 | 1.00 |
| `diskonaut sharded, as root, kernel walk` | 708.3 ± 20.1 | 690.0 | 729.8 | 1.78 ± 0.05 |

### warm: /data/angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 1.467 ± 0.018 | 1.453 | 1.488 | 1.00 |
| `diskonaut sharded` | 1.485 ± 0.006 | 1.479 | 1.490 | 1.01 ± 0.01 |
| `diskonaut refined` | 1.510 ± 0.011 | 1.498 | 1.519 | 1.03 ± 0.01 |
| `diskonaut sharded, as root` | 1.510 ± 0.013 | 1.502 | 1.526 | 1.03 ± 0.02 |
| `diskonaut sharded, as root, kernel walk` | 1.609 ± 0.030 | 1.581 | 1.640 | 1.10 ± 0.02 |

### cold: /data/angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 4.924 ± 0.016 | 4.906 | 4.936 | 2.18 ± 0.02 |
| `diskonaut sharded` | 4.870 ± 0.074 | 4.786 | 4.924 | 2.16 ± 0.04 |
| `diskonaut refined` | 4.921 ± 0.038 | 4.887 | 4.962 | 2.18 ± 0.02 |
| `diskonaut sharded, as root` | 2.254 ± 0.017 | 2.237 | 2.270 | 1.00 |
| `diskonaut sharded, as root, kernel walk` | 3.879 ± 0.051 | 3.835 | 3.935 | 1.72 ± 0.03 |

### warm: /data/angch/.cache

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 740.7 ± 12.7 | 730.9 | 755.1 | 1.00 |
| `diskonaut sharded` | 835.6 ± 14.0 | 824.4 | 851.2 | 1.13 ± 0.03 |
| `diskonaut refined` | 834.6 ± 6.4 | 830.9 | 842.0 | 1.13 ± 0.02 |
| `diskonaut sharded, as root` | 827.3 ± 13.5 | 812.3 | 838.6 | 1.12 ± 0.03 |
| `diskonaut sharded, as root, kernel walk` | 905.4 ± 8.0 | 896.7 | 912.4 | 1.22 ± 0.02 |

### cold: /data/angch/.cache

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 2.565 ± 0.029 | 2.532 | 2.583 | 2.18 ± 0.03 |
| `diskonaut sharded` | 2.529 ± 0.026 | 2.514 | 2.560 | 2.15 ± 0.03 |
| `diskonaut refined` | 2.584 ± 0.037 | 2.545 | 2.620 | 2.20 ± 0.04 |
| `diskonaut sharded, as root` | 1.176 ± 0.010 | 1.164 | 1.183 | 1.00 |
| `diskonaut sharded, as root, kernel walk` | 1.955 ± 0.026 | 1.926 | 1.976 | 1.66 ± 0.03 |

## Build profile (tree-only, warm)

### /data/angch/project

```
  build profile: 31819 directories, 399426 entries, 0.048s of builder time
    resolve 0.009s  6.9 folders/dir  57.0 name compares/dir  303 indexes built
    place   0.033s  84 ns/entry
    sizes   0.004s  9 ns/entry over the 31388 directories with no shared blocks
    ledger  0.001s  431 directories, 13545 sightings  0.5 link comparisons and 3.0 ancestor steps each
tree-only     0.057s       399426 entries     7030005 entries/s        1 unreadable    36.9 GiB (39639519232 B)  6771 hard-linked
```

### /data/angch

```
  build profile: 311387 directories, 2246393 entries, 0.509s of builder time
    resolve 0.138s  10.5 folders/dir  124.7 name compares/dir  1138 indexes built
    place   0.168s  75 ns/entry
    sizes   0.016s  14 ns/entry over the 155951 directories with no shared blocks
    ledger  0.187s  155436 directories, 740588 sightings  6.9 link comparisons and 27.6 ancestor steps each
tree-only     0.606s      2246393 entries     3703966 entries/s       14 unreadable   164.4 GiB (176537931776 B)  175927 hard-linked
```

### /data/angch/.cache

```
  build profile: 174068 directories, 1050436 entries, 0.320s of builder time
    resolve 0.078s  10.8 folders/dir  122.3 name compares/dir  444 indexes built
    place   0.077s  73 ns/entry
    sizes   0.002s  18 ns/entry over the 19116 directories with no shared blocks
    ledger  0.163s  154952 directories, 726937 sightings  7.0 link comparisons and 21.0 ancestor steps each
tree-only     0.384s      1050436 entries     2738457 entries/s        0 unreadable    45.4 GiB (48701509632 B)  169103 hard-linked
```


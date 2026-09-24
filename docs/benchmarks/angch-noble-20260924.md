# angch-noble — 2026-09-24

| | |
| --- | --- |
| machine | AMD Ryzen 7 5825U with Radeon Graphics, 8 cores, 27 GiB, virtualisation: kvm |
| system | Ubuntu 24.04.5 LTS, kernel 6.8.0-137-generic |
| diskonaut | 3d1cdc3 (with local changes), release profile |
| diskus | diskus 0.9.0 |
| cold runs | yes (caches dropped before each) |
| root runs | yes |
| runs per cell | 3 |

## Trees

| tree | filesystem | device | disk | entries | size |
| --- | --- | --- | --- | --- | --- |
| `/data/angch/project` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 393410, 5019 hard-linked | 35.5 GiB |
| `/data/angch` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 2240377, 174175 hard-linked | 163.0 GiB |
| `/data/angch/.cache` | ext4 | /dev/sdb | /dev/sdb (QEMU HARDDISK) SSD | 1050436, 169103 hard-linked | 45.4 GiB |

## Timings

### warm: /data/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 233.6 ± 11.0 | 225.0 | 246.0 | 1.03 ± 0.05 |
| `diskonaut sharded` | 227.7 ± 3.5 | 225.3 | 231.6 | 1.00 |
| `diskonaut refined` | 227.9 ± 2.0 | 226.2 | 230.1 | 1.00 ± 0.02 |

### cold: /data/angch/project

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 756.3 ± 9.7 | 748.4 | 767.1 | 1.02 ± 0.02 |
| `diskonaut sharded` | 759.6 ± 24.0 | 745.4 | 787.3 | 1.03 ± 0.04 |
| `diskonaut refined` | 769.4 ± 21.9 | 755.0 | 794.7 | 1.04 ± 0.03 |
| `diskonaut sharded, as root` | 739.7 ± 12.9 | 725.6 | 750.8 | 1.00 |

### warm: /data/angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 1.458 ± 0.013 | 1.444 | 1.470 | 1.00 |
| `diskonaut sharded` | 1.501 ± 0.011 | 1.489 | 1.508 | 1.03 ± 0.01 |
| `diskonaut refined` | 1.496 ± 0.011 | 1.488 | 1.508 | 1.03 ± 0.01 |

### cold: /data/angch

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 4.974 ± 0.038 | 4.933 | 5.010 | 1.30 ± 0.01 |
| `diskonaut sharded` | 4.957 ± 0.040 | 4.913 | 4.993 | 1.29 ± 0.01 |
| `diskonaut refined` | 4.871 ± 0.071 | 4.799 | 4.940 | 1.27 ± 0.02 |
| `diskonaut sharded, as root` | 3.838 ± 0.023 | 3.819 | 3.864 | 1.00 |

### warm: /data/angch/.cache

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 748.1 ± 9.4 | 738.9 | 757.7 | 1.00 |
| `diskonaut sharded` | 867.2 ± 35.7 | 832.2 | 903.5 | 1.16 ± 0.05 |
| `diskonaut refined` | 909.4 ± 14.4 | 892.8 | 918.9 | 1.22 ± 0.02 |

### cold: /data/angch/.cache

| Command | Mean [s] | Min [s] | Max [s] | Relative |
|:---|---:|---:|---:|---:|
| `diskus` | 2.437 ± 0.105 | 2.372 | 2.559 | 1.26 ± 0.06 |
| `diskonaut sharded` | 2.494 ± 0.019 | 2.473 | 2.509 | 1.29 ± 0.02 |
| `diskonaut refined` | 2.638 ± 0.063 | 2.565 | 2.677 | 1.36 ± 0.04 |
| `diskonaut sharded, as root` | 1.938 ± 0.029 | 1.912 | 1.970 | 1.00 |

## Build profile (tree-only, warm)

### /data/angch/project

```
  build profile: 31668 directories, 393410 entries, 0.048s of builder time
    resolve 0.010s  6.9 folders/dir  64.8 name compares/dir  280 indexes built
    place   0.034s  85 ns/entry
    sizes   0.004s  9 ns/entry over the 31265 directories with no shared blocks
    ledger  0.001s  403 directories, 10041 sightings  0.5 link comparisons and 3.0 ancestor steps each
tree-only     0.057s       393410 entries     6880825 entries/s        1 unreadable    35.5 GiB (38090235904 B)  5019 hard-linked
```

### /data/angch

```
  build profile: 311236 directories, 2240377 entries, 0.510s of builder time
    resolve 0.144s  10.5 folders/dir  147.9 name compares/dir  1115 indexes built
    place   0.172s  77 ns/entry
    sizes   0.016s  14 ns/entry over the 155828 directories with no shared blocks
    ledger  0.177s  155408 directories, 737084 sightings  6.9 link comparisons and 27.7 ancestor steps each
tree-only     0.609s      2240377 entries     3680835 entries/s       14 unreadable   163.0 GiB (174988644352 B)  174175 hard-linked
```

### /data/angch/.cache

```
  build profile: 174068 directories, 1050436 entries, 0.335s of builder time
    resolve 0.087s  10.8 folders/dir  119.7 name compares/dir  444 indexes built
    place   0.077s  73 ns/entry
    sizes   0.002s  19 ns/entry over the 19116 directories with no shared blocks
    ledger  0.170s  154952 directories, 726937 sightings  7.0 link comparisons and 21.0 ancestor steps each
tree-only     0.402s      1050436 entries     2615889 entries/s        0 unreadable    45.4 GiB (48701509632 B)  169103 hard-linked
```


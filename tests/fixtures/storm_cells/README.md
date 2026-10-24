# Storm cell corpus

Decimated reflectivity sweeps used by the storm cell tests in
`src/storm_cells/corpus_tests.rs` (issue #164). Each fixture is the lowest
reflectivity surveillance cut (elevation number 1, 0.5°) of one NEXRAD
Level II volume from the public NOAA archive, reduced so they stay small
enough to check in:

- every second radial (1° spacing instead of 0.5°),
- every second gate (0.5 km instead of 0.25 km),
- cropped to 300 km,
- stored as the raw 8-bit reflectivity codes with their scale and offset,
  zlib-compressed.

| Fixture | Source volume | Case (from the storm cell study) | Size |
|---|---|---|---|
| `KDMX20240521_201727_V06_el1.bin` | [KDMX 2024-05-21 20:17:27Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/21/KDMX/KDMX20240521_201727_V06) | Greenfield, Iowa supercell line | 95 KB |
| `KFWS20240528_102649_V06_el1.bin` | [KFWS 2024-05-28 10:26:49Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/28/KFWS/KFWS20240528_102649_V06) | MCS with trailing stratiform rain | 102 KB |
| `KTBW20241009_195926_V06_el1.bin` | [KTBW 2024-10-09 19:59:26Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/10/09/KTBW/KTBW20241009_195926_V06) | Hurricane Milton rain shield | 139 KB |
| `KDMX20240521_200338_V06_el1.bin` | [KDMX 2024-05-21 20:03:38Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/21/KDMX/KDMX20240521_200338_V06) | Greenfield, two volumes before (tracking history) | 95 KB |
| `KDMX20240521_201032_V06_el1.bin` | [KDMX 2024-05-21 20:10:32Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/21/KDMX/KDMX20240521_201032_V06) | Greenfield, the volume before (tracking history) | 95 KB |
| `KDMX20240521_202422_V06_el1.bin` | [KDMX 2024-05-21 20:24:22Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/21/KDMX/KDMX20240521_202422_V06) | Greenfield, the next volume (tracking) | 95 KB |
| `KDMX20240521_201727_V06_vol.bin` | [KDMX 2024-05-21 20:17:27Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/05/21/KDMX/KDMX20240521_201727_V06) | Greenfield, the volume's 13 upper reflectivity tilts over the line's sector (columns, #178) | 118 KB |
| `KDMX20240715_115714_V06_el1.bin` | [KDMX 2024-07-15 11:57:14Z](https://unidata-nexrad-level2.s3.amazonaws.com/2024/07/15/KDMX/KDMX20240715_115714_V06) | Clear-air control (wind-farm clutter, Minnesota storms at range) | 89 KB |
| `KABR20261023_160330_V06_el1.bin` | [KABR 2026-10-23 16:03:30Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KABR/KABR20261023_160330_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 41 KB |
| `KABR20261023_160801_V06_el1.bin` | [KABR 2026-10-23 16:08:01Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KABR/KABR20261023_160801_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 41 KB |
| `KABR20261023_161233_V06_el1.bin` | [KABR 2026-10-23 16:12:33Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KABR/KABR20261023_161233_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 41 KB |
| `KDMX20261023_160448_V06_el1.bin` | [KDMX 2026-10-23 16:04:48Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KDMX/KDMX20261023_160448_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 44 KB |
| `KDMX20261023_160912_V06_el1.bin` | [KDMX 2026-10-23 16:09:12Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KDMX/KDMX20261023_160912_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 44 KB |
| `KDMX20261023_161332_V06_el1.bin` | [KDMX 2026-10-23 16:13:32Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KDMX/KDMX20261023_161332_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 45 KB |
| `KFSD20261023_160127_V06_el1.bin` | [KFSD 2026-10-23 16:01:27Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KFSD/KFSD20261023_160127_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 42 KB |
| `KFSD20261023_160556_V06_el1.bin` | [KFSD 2026-10-23 16:05:56Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KFSD/KFSD20261023_160556_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 41 KB |
| `KFSD20261023_161026_V06_el1.bin` | [KFSD 2026-10-23 16:10:26Z](https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/23/KFSD/KFSD20261023_161026_V06) | Three sites on one composite, 16:01-16:13Z (multi-site tracking) | 41 KB |

Source: NOAA NEXRAD Level II archive, served by the NOAA Open Data
Dissemination program at `https://unidata-nexrad-level2.s3.amazonaws.com/`
(`YYYY/MM/DD/SITE/SITEYYYYMMDD_HHMMSS_V06`). The Milton fixture is the
largest because the rain shield fills most of the 300 km disc; it is kept
lossless rather than zeroing weak echo, which would change the median
filter's input.

SHA-256 of the source volumes the fixtures were built from:

```
9f728bbd3d8e40cf60855c99843d469787f6e6a2573fd261a7abd7c3378fdc85  KDMX20240521_200338_V06
906d27f26aabae6c391e087734f36fc65ce5b83212518cb5e4ef27755639f814  KDMX20240521_201032_V06
d947278e0dc475e702f3dd1a11444bfb3d4c41bc64cfcb01fc03cb9a798450fc  KDMX20240521_201727_V06
a81a8f0e38ec983f6b91e85d4ac6809f2565e38fea341a429e3a4e06f9b84ba3  KDMX20240521_202422_V06
2a9c31a4d85490cdadae3eeb10f23956d61a0d2b9a5f448280c5cd29187f4e7f  KFWS20240528_102649_V06
ee2227be58c2c6536af2b4d9386feeb0b407ac43d9e7d70379f6cbbabea5f90e  KTBW20241009_195926_V06
2651944305c05630f88bd02ea967ef257a4aa956f0157d0adc21e4114b0500b6  KDMX20240715_115714_V06
33696df1395af312bca4a408ab52e75c3bc72bc996a04d417cbecb1ad826b377  KABR20261023_160330_V06
c81afba4e3b9e1f7af142f3e587afd1a228ea1934f53c23be2b8ae5a15eacff8  KABR20261023_160801_V06
cd95dfa56b4793b80d8edf357aea7003ac18c5f282e4711adfc4f0c6c1dcc8cf  KABR20261023_161233_V06
eb2fb9f6a9d0984d0dce0c9d8503a3e746cc1a540814f7ffd39e4905cf8b18be  KDMX20261023_160448_V06
728bd183016acba39d7d39c8d9dd7aebd011d06ab7cd576d0901aacc934fe6f0  KDMX20261023_160912_V06
f65f76b4fb0904c7b8f190d2d368af4828019100c5b639ff65a4b6e653f3e6cb  KDMX20261023_161332_V06
07e713e98926e906981c6a8168a7962b2359914615f9df04be4b8bec98b9b3e1  KFSD20261023_160127_V06
6375d9ced72abb58cea94e9c617eccc4fda696b706dd27e477fb723fd1d76416  KFSD20261023_160556_V06
91156e2c0576102b4fe3443f7b45f3715bdbcd72e37af880b261ff459ad7cc4c  KFSD20261023_161026_V06
```

## Format

zlib stream of (little-endian):

```
magic        4  "NXSC"
version      u8 1
site         4  ASCII ICAO
radar_lat    f64
radar_lon    f64
elevation    f32 degrees
first_gate   f32 km (as the decoder reports it)
gate_step    f32 km (after decimation)
scale        f32 physical = (raw - offset) / scale
offset       f32
az_count     u16
gate_count   u16
azimuths     f32 × az_count, ascending degrees
gates        u8  × az_count × gate_count, row-major by azimuth
```

Raw codes 0 (below threshold) and 1 (range folded) keep their sentinel
meaning.

### Volume fixtures (`_vol.bin`)

The other reflectivity tilts of a volume, for the cells' columns: one cut
per distinct angle above the lowest (the first collected), each cropped to
an azimuth sector and slant range window, 1° radials (every second radial
of a 0.5° cut) and every second gate. zlib stream of (little-endian):

```
magic        4  "NXSV"
version      u8 1
site         4  ASCII ICAO
tilt_count   u8
per tilt:
  elevation_number u8
  elevation    f32 degrees
  first_gate   f32 km (range of the first kept gate)
  gate_step    f32 km (after decimation)
  scale        f32
  offset       f32
  az_count     u16
  gate_count   u16
  azimuths     f32 × az_count
  gates        u8  × az_count × gate_count
```

The Greenfield one covers azimuths 212-318° and 55-155 km, the line from
Adams to Calhoun County.

## Regenerating

The generator is a small native program in `tools/corpus/` (its own Cargo
project, outside the app build, using the same `nexrad-data` and
`nexrad-model` releases as the app). The app's default target is wasm32, so
build it for the host explicitly:

```bash
mkdir -p /tmp/volumes && cd /tmp/volumes
for p in 2024/05/21/KDMX/KDMX20240521_201727_V06 \
         2024/05/21/KDMX/KDMX20240521_202422_V06 \
         2024/05/28/KFWS/KFWS20240528_102649_V06 \
         2024/10/09/KTBW/KTBW20241009_195926_V06 \
         2024/07/15/KDMX/KDMX20240715_115714_V06 \
         2026/10/23/KABR/KABR20261023_160330_V06 \
         2026/10/23/KABR/KABR20261023_160801_V06 \
         2026/10/23/KABR/KABR20261023_161233_V06 \
         2026/10/23/KDMX/KDMX20261023_160448_V06 \
         2026/10/23/KDMX/KDMX20261023_160912_V06 \
         2026/10/23/KDMX/KDMX20261023_161332_V06 \
         2026/10/23/KFSD/KFSD20261023_160127_V06 \
         2026/10/23/KFSD/KFSD20261023_160556_V06 \
         2026/10/23/KFSD/KFSD20261023_161026_V06; do
  curl -sSfO "https://unidata-nexrad-level2.s3.amazonaws.com/$p"
done

cd tools/corpus
cargo run --release --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  ../../tests/fixtures/storm_cells /tmp/volumes/*_V06
```

The volume fixture:

```bash
cargo run --release --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  --volume 212 318 55 155 ../../tests/fixtures/storm_cells \
  /tmp/volumes/KDMX20240521_201727_V06
```

`--max-range-km N` before the output directory changes the crop. The output
is deterministic: rerunning on the same volumes reproduces the files byte for
byte.

## What the tests expect

The expectations encode the study's observations
(`corpus_tests.rs` has the details and the boxes):

- Greenfield: the supercell line (one 270 km cell at a single 35 dBZ
  threshold before) resolves into 5-8 cells, one each for the Calhoun,
  Carroll, Audubon, Cass/Adair and Adams storms, all in one group. A
  snapshot of the full cell list makes any segmentation change visible.
- KFWS: no cell weaker than 50 dBZ in the trailing stratiform region; the
  leading edge is a group of cores rather than one 250 km cell.
- Milton: the shield is groups of band cells, no cell near the radar is
  shield-sized, and no peak exceeds 70 dBZ (the single-gate 73 is gone).
- Greenfield 20:24Z, tracked from 20:17Z (`track.rs`): the Calhoun,
  Carroll, Cass/Adair and Adams storms keep their tracks and move
  north-east; every match stays within the search radius.
- Greenfield columns (`_vol.bin`): every storm on the line has an echo
  top of 12-15.5 km and echo on most tilts; the Greenfield storm has the
  strongest column, 63 dBZ about 3 km up.
- Three sites (KABR, KDMX, KFSD, 2026-10-23), detected on the composite
  at 22 dBZ in the seven scenes from 16:04:48Z (one at each volume start):
  a KABR cut leaves every storm in a group KABR does not reach unchanged
  (the grid's fixed lattice); each cell carries the start of the cut that
  shaped it, and the 50+ dBZ core on the Iowa-Minnesota line keeps KDMX's
  16:04:48Z through the KFSD and KABR frames; tracked with the composite's
  13 minute coasting floor, a storm KDMX shapes gains no point between
  KDMX's cuts and no track ends, where counting frames alone ends some.
- Clear air: cells only at the Minnesota complex plus exactly one clutter
  cluster (Greene County). A correlation coefficient filter in a later
  change should bring the clutter count to zero.

Peaks after the 3×3 median at this resolution run a few dB below the
full-resolution labels quoted in the study.

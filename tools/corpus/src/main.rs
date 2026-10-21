//! Build storm cell corpus fixtures from NEXRAD Archive II volumes.
//!
//! For each volume: take the lowest reflectivity surveillance cut
//! (elevation number 1), sort its radials by azimuth, keep every second
//! radial and every second gate, crop to `--max-range-km` (default 300), and
//! write the raw u8 reflectivity codes with their scale/offset and azimuths.
//!
//! Format (all little-endian), zlib-compressed as a whole:
//!
//! ```text
//! magic        4  b"NXSC"
//! version      u8 1
//! site         4  ASCII ICAO
//! radar_lat    f64
//! radar_lon    f64
//! elevation    f32 degrees
//! first_gate   f32 km (range of the first gate, as the decoder reports it)
//! gate_step    f32 km (after decimation)
//! scale        f32 physical = (raw - offset) / scale
//! offset       f32
//! az_count     u16
//! gate_count   u16
//! azimuths     f32 × az_count, ascending degrees
//! gates        u8  × az_count × gate_count, row-major by azimuth
//! ```
//!
//! Usage: `storm-cell-corpus [--max-range-km 300] OUT_DIR VOLUME...`
//! (see `tests/fixtures/storm_cells/README.md` for the full recipe).

use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use nexrad_model::data::DataMoment;

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut max_range_km = 300.0_f64;
    if args.peek().map(String::as_str) == Some("--max-range-km") {
        args.next();
        max_range_km = args
            .next()
            .and_then(|v| v.parse().ok())
            .expect("--max-range-km takes a number");
    }
    let out_dir = PathBuf::from(args.next().expect("usage: OUT_DIR VOLUME..."));
    let volumes: Vec<String> = args.collect();
    assert!(!volumes.is_empty(), "usage: OUT_DIR VOLUME...");
    std::fs::create_dir_all(&out_dir).expect("create OUT_DIR");

    for volume in volumes {
        let path = Path::new(&volume);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = build_fixture(path, max_range_km);
        let out = out_dir.join(format!("{name}_el1.bin"));
        std::fs::write(&out, &bytes).expect("write fixture");
        println!("{} ({} bytes)", out.display(), bytes.len());
    }
}

fn build_fixture(path: &Path, max_range_km: f64) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read volume");
    let scan = nexrad_data::volume::File::new(bytes)
        .decompress()
        .expect("decompress volume")
        .scan()
        .expect("decode volume");
    let site = scan.site().expect("site metadata");
    let sweep = scan
        .sweeps()
        .iter()
        .find(|s| {
            s.elevation_number() == 1 && s.radials().iter().any(|r| r.reflectivity().is_some())
        })
        .expect("elevation 1 with reflectivity");

    let mut radials: Vec<_> = sweep
        .radials()
        .iter()
        .filter(|r| r.reflectivity().is_some())
        .collect();
    radials.sort_by(|a, b| {
        a.azimuth_angle_degrees()
            .total_cmp(&b.azimuth_angle_degrees())
    });

    let first = radials[0].reflectivity().unwrap();
    assert_eq!(first.data_word_size(), 8, "8-bit reflectivity expected");
    let scale = first.scale();
    let offset = first.offset();
    let first_gate_km = first.first_gate_range_km();
    let gate_km = first.gate_interval_km();
    let native_gates = first.gate_count() as usize;
    let keep_native =
        (((max_range_km - first_gate_km) / gate_km).floor() as usize + 1).min(native_gates);
    let gate_count = keep_native.div_ceil(2);

    let kept: Vec<_> = radials.iter().step_by(2).collect();
    let mut azimuths = Vec::with_capacity(kept.len());
    let mut gates = Vec::with_capacity(kept.len() * gate_count);
    for r in &kept {
        let m = r.reflectivity().unwrap();
        assert_eq!(m.scale(), scale, "scale changes within the sweep");
        assert_eq!(m.offset(), offset, "offset changes within the sweep");
        azimuths.push(r.azimuth_angle_degrees());
        let raw = m.raw_values();
        for g in 0..gate_count {
            gates.push(raw.get(g * 2).copied().unwrap_or(0));
        }
    }
    let elevation = sweep.elevation_angle_degrees().unwrap_or(0.0);

    let mut payload = Vec::new();
    payload.extend_from_slice(b"NXSC");
    payload.push(1);
    payload.extend_from_slice(site.identifier());
    payload.extend_from_slice(&(site.latitude() as f64).to_le_bytes());
    payload.extend_from_slice(&(site.longitude() as f64).to_le_bytes());
    payload.extend_from_slice(&elevation.to_le_bytes());
    payload.extend_from_slice(&(first_gate_km as f32).to_le_bytes());
    payload.extend_from_slice(&((gate_km * 2.0) as f32).to_le_bytes());
    payload.extend_from_slice(&scale.to_le_bytes());
    payload.extend_from_slice(&offset.to_le_bytes());
    payload.extend_from_slice(&(azimuths.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(gate_count as u16).to_le_bytes());
    for a in &azimuths {
        payload.extend_from_slice(&a.to_le_bytes());
    }
    payload.extend_from_slice(&gates);

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&payload).unwrap();
    enc.finish().unwrap()
}

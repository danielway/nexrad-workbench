//! Test-only synthesizer for a parseable `volume_coverage_pattern::Message`.
//!
//! The timing functions (`project_scan_timing_with_next`,
//! `estimate_chunk_processing_*`) take a real `&volume_coverage_pattern::Message`
//! and the `nexrad-decode` crate exposes no public constructor for one — the
//! only way in is `decode_messages(&[u8])`. So we hand-assemble a single-segment
//! Type-5 (Volume Coverage Pattern) message frame and feed it through the public
//! decoder. The raw struct layouts are fixed `#[repr(C)]` big-endian, so the
//! byte offsets below are stable; the `vcp_roundtrips_through_decoder` test pins
//! that our assembly still parses to the expected elevation values.

#![cfg(test)]
// Not every fixture helper is exercised by every test module that pulls in
// this builder; keep them all for legible, self-documenting test setup.
#![allow(dead_code)]

use nexrad_decode::messages::volume_coverage_pattern::{
    ChannelConfiguration, Message, WaveformType,
};
use nexrad_decode::messages::{decode_messages, MessageContents};

/// One fixed message frame is 2432 bytes (header + content + padding).
const SEGMENT_FRAME_SIZE: usize = 2432;
/// `MessageHeader` is 28 bytes (12 rpg + 2 + 1 + 1 + 2 + 2 + 4 + 2 + 2).
const MESSAGE_HEADER_SIZE: usize = 28;
/// The VCP `raw::Header` is 22 bytes (msg_size 2 + pattern_type 2 +
/// pattern_number 2 + num_cuts 2 + version 1 + clutter 1 + doppler_res 1 +
/// pulse_width 1 + reserved_1 4 + vcp_sequencing 2 + vcp_supplemental 2 +
/// reserved_2 2). Pinned by `vcp_roundtrips_through_decoder`.
const VCP_HEADER_SIZE: usize = 22;
/// Each `raw::ElevationDataBlock` is 46 bytes.
const ELEVATION_BLOCK_SIZE: usize = 46;

/// A test elevation cut. Raw fields are chosen by the caller so decoded values
/// are predictable (see `decoded_angle` / `decoded_azimuth_rate`).
#[derive(Clone, Copy)]
pub(super) struct TestElevation {
    /// 0.5-degree azimuth super-resolution (6 chunks/sweep) vs standard (3).
    pub super_res: bool,
    /// Raw encoded elevation angle (decoded via `decode_angle`).
    pub elevation_angle_raw: u16,
    /// Raw encoded azimuth rate (decoded via `decode_angular_velocity`).
    pub azimuth_rate_raw: u16,
    /// Raw waveform-type code (1=CS, 2=CDW, 3=CDWO, 4=B, 5=SPP).
    pub waveform_raw: u8,
    /// Raw channel-configuration code (0=ConstantPhase, 1=RandomPhase, 2=SZ2Phase).
    pub channel_raw: u8,
}

impl TestElevation {
    /// Convenience constructor for a standard (3-chunk) CS / ConstantPhase cut.
    pub(super) fn standard_cs(elevation_angle_raw: u16, azimuth_rate_raw: u16) -> Self {
        Self {
            super_res: false,
            elevation_angle_raw,
            azimuth_rate_raw,
            waveform_raw: 1, // CS
            channel_raw: 0,  // ConstantPhase
        }
    }
}

/// Encode an elevation angle so `decode_angle` returns `n * (180/4096)` degrees.
/// `decode_angle` sums `180 * 2^(i-15)` for set bits i in 3..16, i.e. the value
/// is `(raw >> 3) as f64 * 180.0 / 4096.0` for the low 13 angle bits.
pub(super) fn decoded_angle(raw: u16) -> f64 {
    let mut angle = 0.0;
    for i in 3..16u32 {
        if (raw >> i) & 1 == 1 {
            angle += 180.0 * 2f64.powi(i as i32 - 15);
        }
    }
    angle
}

/// Encode an azimuth rate so `decode_angular_velocity` returns the expected dps.
/// Sums `22.5 * 2^(i-14)` for set bits i in 3..15 (bit 15 = sign).
pub(super) fn decoded_azimuth_rate(raw: u16) -> f64 {
    let mut v = 0.0;
    for i in 3..15u32 {
        if (raw >> i) & 1 == 1 {
            v += 22.5 * 2f64.powi(i as i32 - 14);
        }
    }
    if (raw >> 15) & 1 == 1 {
        v = -v;
    }
    v
}

fn be16(buf: &mut [u8], off: usize, v: u16) {
    buf[off] = (v >> 8) as u8;
    buf[off + 1] = (v & 0xff) as u8;
}

/// Build a parseable Type-5 VCP `Message` from the given elevation cuts.
pub(super) fn build_vcp(elevations: &[TestElevation]) -> Message<'static> {
    let mut frame = vec![0u8; SEGMENT_FRAME_SIZE];

    // ── Message header (28 bytes) ─────────────────────────────────────────
    // segment_size (halfwords) must be < 0xFFFF so the frame is treated as a
    // single fixed segment; the exact value isn't read by the VCP parser.
    be16(&mut frame, 12, 100); // segment_size
    frame[14] = 0; // redundant_channel
    frame[15] = 5; // message_type = RDAVolumeCoveragePattern
    be16(&mut frame, 16, 1); // sequence_number
    be16(&mut frame, 18, 0); // date
                             // time (Integer4) at 20..24 left zero.
    be16(&mut frame, 24, 1); // segment_count = 1
    be16(&mut frame, 26, 1); // segment_number = 1

    // ── VCP header (22 bytes), begins right after the message header ──────
    let vcp = MESSAGE_HEADER_SIZE;
    be16(&mut frame, vcp, 100); // message_size (halfwords, unused by us)
    be16(&mut frame, vcp + 2, 2); // pattern_type
    be16(&mut frame, vcp + 4, 212); // pattern_number
    be16(&mut frame, vcp + 6, elevations.len() as u16); // number_of_elevation_cuts
                                                        // remaining header bytes left zero.

    // ── Elevation blocks (46 bytes each) ─────────────────────────────────
    let mut off = vcp + VCP_HEADER_SIZE;
    for e in elevations {
        be16(&mut frame, off, e.elevation_angle_raw); // elevation_angle
        frame[off + 2] = e.channel_raw; // channel_configuration
        frame[off + 3] = e.waveform_raw; // waveform_type
        frame[off + 4] = if e.super_res { 1 } else { 0 }; // super_resolution_control bit 0
                                                          // surveillance_prf_number (off+5) = 0
                                                          // surveillance_prf_pulse_count_radial (off+6..8) = 0
        be16(&mut frame, off + 8, e.azimuth_rate_raw); // azimuth_rate
                                                       // remaining block bytes left zero.
        off += ELEVATION_BLOCK_SIZE;
    }

    let messages = decode_messages(&frame).expect("VCP frame decodes");
    for m in messages {
        if let MessageContents::VolumeCoveragePattern(v) = m.contents() {
            return v.clone().into_owned();
        }
    }
    panic!("no VCP message decoded from synthesized frame");
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn vcp_roundtrips_through_decoder() {
        // Two cuts: a super-res CS / ConstantPhase at 22.5 dps and a standard
        // CDWO / RandomPhase at 33.75 dps. Azimuth raws use the documented
        // weights: bit 14 = 22.5 dps, bit 13 = 11.25 dps.
        let el0 = TestElevation {
            super_res: true,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 1,           // CS
            channel_raw: 0,            // ConstantPhase
        };
        let el1 = TestElevation {
            super_res: false,
            elevation_angle_raw: 0,
            azimuth_rate_raw: (1 << 14) | (1 << 13), // 33.75 dps
            waveform_raw: 3,                         // CDWO
            channel_raw: 1,                          // RandomPhase
        };
        let vcp = build_vcp(&[el0, el1]);
        let elevs = vcp.elevations();
        assert_eq!(elevs.len(), 2);

        assert!(elevs[0].super_resolution_half_degree_azimuth());
        assert_eq!(elevs[0].waveform_type(), WaveformType::CS);
        assert_eq!(
            elevs[0].channel_configuration(),
            ChannelConfiguration::ConstantPhase
        );
        assert!((elevs[0].azimuth_rate() - 22.5).abs() < 1e-9);

        assert!(!elevs[1].super_resolution_half_degree_azimuth());
        assert_eq!(elevs[1].waveform_type(), WaveformType::CDWO);
        assert_eq!(
            elevs[1].channel_configuration(),
            ChannelConfiguration::RandomPhase
        );
        assert!((elevs[1].azimuth_rate() - 33.75).abs() < 1e-9);

        // The angle/azimuth-rate decode helpers in this module agree with the
        // decoder, so tests can hand-pick raw values for known decoded results.
        assert!((decoded_azimuth_rate(1 << 14) - 22.5).abs() < 1e-9);
        assert!((decoded_azimuth_rate((1 << 14) | (1 << 13)) - 33.75).abs() < 1e-9);
    }
}

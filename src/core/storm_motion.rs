//! Storm-relative velocity: the storm motion it subtracts, and when that
//! motion is taken.
//!
//! Storm-relative velocity (SRV) is base velocity with the storm motion's
//! component along each beam removed:
//! `srv = v - (east * sin(az) + north * cos(az))`. The shaders and the probe
//! do the subtraction ([`storm_relative`] is the CPU half); this module
//! decides the motion.
//!
//! # The lock
//!
//! The motion comes from the storm tracker: the confidence-weighted median
//! of the lettered tracks' motions on the shown volume ([`scene_motion`]).
//! It is taken once, when SRV is picked, and then held ([`MotionLock`]), so
//! the colors do not shift from volume to volume while a loop plays. Until
//! the tracker has a confident motion the lock waits and SRV draws plain
//! velocity, with a note saying so. "Update" takes the motion again from the
//! tracks on screen. A link carries a locked motion (`srm`), so a shared
//! SRV view opens with the same colors.
//!
//! While the lock waits, storm cells are detected and tracked even when
//! their layers are off ([`wants_tracks`]); nothing of them is drawn.

use crate::core::storm_tracks::TracksView;
use crate::core::RadarProduct;

/// A storm motion: the compass direction it moves toward and its speed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StormMotion {
    /// Compass degrees toward which the storms move, in [0, 360).
    pub direction_deg: f64,
    pub speed_kmh: f64,
}

impl StormMotion {
    /// The motion as (east, north) components in m/s, the units velocity
    /// is drawn in.
    pub(crate) fn east_north_ms(&self) -> [f32; 2] {
        let rad = self.direction_deg.to_radians();
        let ms = self.speed_kmh / 3.6;
        [(ms * rad.sin()) as f32, (ms * rad.cos()) as f32]
    }

    /// Rounded to whole degrees and km/h: what is locked, shown and linked,
    /// so a link reproduces the colors exactly.
    fn rounded(self) -> StormMotion {
        StormMotion {
            direction_deg: self.direction_deg.round().rem_euclid(360.0),
            speed_kmh: self.speed_kmh.round(),
        }
    }
}

/// The motion SRV subtracts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum MotionLock {
    /// No motion yet: taken from the tracks as soon as they have one.
    #[default]
    Waiting,
    Locked(StormMotion),
}

/// The scene's storm motion: the confidence-weighted median, per component,
/// of the motions of the tracks with a confident motion (the ones that draw
/// a forecast arrow), coasting ones left out. `None` when there is none.
pub(crate) fn scene_motion(view: &TracksView) -> Option<StormMotion> {
    let samples: Vec<(f64, f64, f64)> = view
        .tracks
        .iter()
        .filter(|t| t.forecast.is_some() && !t.coasting)
        .map(|t| {
            let rad = t.direction_deg.to_radians();
            (
                t.speed_kmh * rad.sin(),
                t.speed_kmh * rad.cos(),
                t.confidence,
            )
        })
        .collect();
    let east = weighted_median(samples.iter().map(|s| (s.0, s.2)).collect())?;
    let north = weighted_median(samples.iter().map(|s| (s.1, s.2)).collect())?;
    Some(StormMotion {
        direction_deg: east.atan2(north).to_degrees().rem_euclid(360.0),
        speed_kmh: east.hypot(north),
    })
}

/// The value at which half the weight lies on either side; the mean of the
/// two middle values when the halves split exactly between them.
fn weighted_median(mut values: Vec<(f64, f64)>) -> Option<f64> {
    values.retain(|(v, w)| v.is_finite() && *w > 0.0);
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = values.iter().map(|(_, w)| w).sum();
    let mut below = 0.0;
    for (i, (v, w)) in values.iter().enumerate() {
        below += w;
        if below > total / 2.0 + 1e-9 {
            return Some(*v);
        }
        if (below - total / 2.0).abs() <= 1e-9 {
            return Some((v + values.get(i + 1).map_or(*v, |n| n.0)) / 2.0);
        }
    }
    values.last().map(|(v, _)| *v)
}

/// The lock after the product changed from `before` to `after`: picking SRV
/// starts a new wait; anything else leaves it.
pub(crate) fn on_product_change(lock: &mut MotionLock, before: RadarProduct, after: RadarProduct) {
    if after == RadarProduct::StormRelativeVelocity && before != after {
        *lock = MotionLock::Waiting;
    }
}

/// Lock the motion once the tracks have one, while SRV is shown and the
/// lock waits. Returns whether it locked.
pub(crate) fn settle(
    lock: &mut MotionLock,
    product: RadarProduct,
    view: Option<&TracksView>,
) -> bool {
    if !wants_tracks(product, lock) {
        return false;
    }
    match view.and_then(scene_motion) {
        Some(motion) => {
            *lock = MotionLock::Locked(motion.rounded());
            true
        }
        None => false,
    }
}

/// Whether the tracker should run for SRV: it is shown and the lock waits.
pub(crate) fn wants_tracks(product: RadarProduct, lock: &MotionLock) -> bool {
    product == RadarProduct::StormRelativeVelocity && *lock == MotionLock::Waiting
}

/// The (east, north) motion in m/s the renderers subtract: the locked
/// motion while SRV is shown, else none (plain values).
pub(crate) fn shader_motion(product: RadarProduct, lock: &MotionLock) -> Option<[f32; 2]> {
    match lock {
        MotionLock::Locked(m) if product == RadarProduct::StormRelativeVelocity => {
            Some(m.east_north_ms())
        }
        _ => None,
    }
}

/// `value` (m/s, away from the radar positive) at compass azimuth
/// `azimuth_deg`, with the `motion` (east, north m/s) component along the
/// beam removed. The shaders do the same per pixel.
pub(crate) fn storm_relative(value: f32, azimuth_deg: f32, motion: Option<[f32; 2]>) -> f32 {
    match motion {
        Some([east, north]) => {
            let az = azimuth_deg.to_radians();
            value - (east * az.sin() + north * az.cos())
        }
        None => value,
    }
}

/// The product panel's line under the SRV chip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MotionVm {
    /// "Storm motion NE 47 km/h, locked" or the waiting note.
    pub text: String,
    /// Offer "Update" (take the motion again from the tracks on screen).
    pub can_update: bool,
}

/// The waiting note: SRV shows plain velocity until storms are tracked.
pub(crate) const WAITING_NOTE: &str = "No tracked storm motion yet, so this is plain velocity";

/// The hover over "Update".
pub(crate) const UPDATE_HOVER: &str = "Take the storm motion again from the storms on screen";

/// The panel line for SRV, `None` for any other product.
pub(crate) fn motion_vm(product: RadarProduct, lock: &MotionLock) -> Option<MotionVm> {
    if product != RadarProduct::StormRelativeVelocity {
        return None;
    }
    Some(match lock {
        MotionLock::Waiting => MotionVm {
            text: WAITING_NOTE.to_string(),
            can_update: false,
        },
        MotionLock::Locked(m) => MotionVm {
            text: format!(
                "Storm motion {} {:.0} km/h, locked",
                crate::core::storm_cell_view::compass(m.direction_deg),
                m.speed_kmh
            ),
            can_update: true,
        },
    })
}

/// The link's `srm=<toward deg>,<km/h>`.
pub(crate) fn link_value(m: &StormMotion) -> String {
    format!("{:.0},{:.0}", m.direction_deg, m.speed_kmh)
}

/// Parse `srm=<toward deg>,<km/h>`; malformed or negative values are
/// dropped.
pub(crate) fn parse_link_value(value: &str) -> Option<StormMotion> {
    let (dir, speed) = value.split_once(',')?;
    let direction_deg: f64 = dir.trim().parse().ok().filter(|d: &f64| d.is_finite())?;
    let speed_kmh: f64 = speed
        .trim()
        .parse()
        .ok()
        .filter(|s: &f64| s.is_finite() && *s >= 0.0 && *s < 400.0)?;
    Some(
        StormMotion {
            direction_deg,
            speed_kmh,
        }
        .rounded(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storm_tracks::TrackVm;
    use wasm_bindgen_test::wasm_bindgen_test;

    const SRV: RadarProduct = RadarProduct::StormRelativeVelocity;

    fn track(direction_deg: f64, speed_kmh: f64, confidence: f64) -> TrackVm {
        TrackVm {
            id: Default::default(),
            label: "A".into(),
            cell_id: Some(1),
            lat: 41.0,
            lon: -94.0,
            trail: vec![],
            split_origin: None,
            forecast: (confidence >= crate::storm_cells::track::MIN_FORECAST_CONFIDENCE)
                .then_some((41.1, -93.9)),
            speed_kmh,
            direction_deg,
            confidence,
            age_volumes: 3,
            age_minutes: 15.0,
            peak_trend_db: None,
            stationary: false,
            coasting: false,
            from_reference: None,
            approach: None,
        }
    }

    fn view(tracks: Vec<TrackVm>) -> TracksView {
        TracksView {
            provisional: false,
            tracks,
        }
    }

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[wasm_bindgen_test]
    #[test]
    fn one_track_gives_its_own_motion() {
        let m = scene_motion(&view(vec![track(45.0, 50.0, 0.9)])).unwrap();
        assert!(near(m.direction_deg, 45.0, 1e-6), "{m:?}");
        assert!(near(m.speed_kmh, 50.0, 1e-6), "{m:?}");
    }

    #[wasm_bindgen_test]
    #[test]
    fn the_median_ignores_an_outlier_and_weak_or_coasting_tracks() {
        let mut coasting = track(270.0, 90.0, 0.9);
        coasting.coasting = true;
        let m = scene_motion(&view(vec![
            track(60.0, 40.0, 0.9),
            track(60.0, 42.0, 0.8),
            track(60.0, 44.0, 0.9),
            // A hop between fragments: one wild motion.
            track(200.0, 120.0, 0.5),
            // Below the forecast confidence: no say.
            track(180.0, 80.0, 0.2),
            coasting,
        ]))
        .unwrap();
        assert!(near(m.direction_deg, 60.0, 1.0), "{m:?}");
        assert!(near(m.speed_kmh, 42.0, 1.5), "{m:?}");
    }

    #[wasm_bindgen_test]
    #[test]
    fn no_confident_track_gives_no_motion() {
        assert_eq!(scene_motion(&view(vec![])), None);
        assert_eq!(scene_motion(&view(vec![track(90.0, 30.0, 0.1)])), None);
    }

    #[wasm_bindgen_test]
    #[test]
    fn weighted_median_splits_an_even_tie() {
        assert_eq!(weighted_median(vec![(1.0, 1.0), (3.0, 1.0)]), Some(2.0));
        assert_eq!(weighted_median(vec![(1.0, 1.0), (3.0, 2.0)]), Some(3.0));
        assert_eq!(weighted_median(vec![]), None);
    }

    #[wasm_bindgen_test]
    #[test]
    fn picking_srv_waits_then_locks_and_holds() {
        let mut lock = MotionLock::Locked(StormMotion {
            direction_deg: 10.0,
            speed_kmh: 10.0,
        });
        on_product_change(&mut lock, RadarProduct::Velocity, SRV);
        assert_eq!(lock, MotionLock::Waiting);
        assert!(wants_tracks(SRV, &lock));
        assert_eq!(shader_motion(SRV, &lock), None);

        // Nothing tracked yet: still waiting, plain velocity.
        assert!(!settle(&mut lock, SRV, None));
        assert!(!settle(&mut lock, SRV, Some(&view(vec![]))));
        assert_eq!(lock, MotionLock::Waiting);

        assert!(settle(
            &mut lock,
            SRV,
            Some(&view(vec![track(44.6, 49.7, 0.9)]))
        ));
        let locked = MotionLock::Locked(StormMotion {
            direction_deg: 45.0,
            speed_kmh: 50.0,
        });
        assert_eq!(lock, locked);
        assert!(!wants_tracks(SRV, &lock));

        // Later volumes move differently: the lock holds.
        assert!(!settle(
            &mut lock,
            SRV,
            Some(&view(vec![track(90.0, 20.0, 0.9)]))
        ));
        assert_eq!(lock, locked);
        // Re-picking SRV while shown changes nothing; other products leave it.
        on_product_change(&mut lock, SRV, SRV);
        on_product_change(&mut lock, SRV, RadarProduct::Reflectivity);
        assert_eq!(lock, locked);
    }

    #[wasm_bindgen_test]
    #[test]
    fn only_srv_subtracts_and_waits() {
        let lock = MotionLock::Locked(StormMotion {
            direction_deg: 90.0,
            speed_kmh: 36.0,
        });
        assert_eq!(shader_motion(RadarProduct::Velocity, &lock), None);
        let [east, north] = shader_motion(SRV, &lock).unwrap();
        assert!((east - 10.0).abs() < 1e-4 && north.abs() < 1e-4);
        assert!(!wants_tracks(RadarProduct::Velocity, &MotionLock::Waiting));
        let mut waiting = MotionLock::Waiting;
        assert!(!settle(
            &mut waiting,
            RadarProduct::Velocity,
            Some(&view(vec![track(0.0, 30.0, 0.9)]))
        ));
    }

    #[wasm_bindgen_test]
    #[test]
    fn storm_relative_removes_the_motion_along_the_beam() {
        // Storms moving east at 10 m/s.
        let m = Some([10.0, 0.0]);
        // East beam: a 25 m/s outbound gate is 15 m/s relative to the storm.
        assert!((storm_relative(25.0, 90.0, m) - 15.0).abs() < 1e-4);
        // West beam: the storm comes toward the radar; -25 becomes -15.
        assert!((storm_relative(-25.0, 270.0, m) + 15.0).abs() < 1e-4);
        // North beam: across the motion, unchanged.
        assert!((storm_relative(7.0, 0.0, m) - 7.0).abs() < 1e-4);
        assert_eq!(storm_relative(7.0, 90.0, None), 7.0);
    }

    #[wasm_bindgen_test]
    #[test]
    fn the_panel_line_says_the_motion_or_the_wait() {
        assert_eq!(
            motion_vm(RadarProduct::Velocity, &MotionLock::Waiting),
            None
        );
        assert_eq!(
            motion_vm(SRV, &MotionLock::Waiting),
            Some(MotionVm {
                text: WAITING_NOTE.to_string(),
                can_update: false
            })
        );
        let vm = motion_vm(
            SRV,
            &MotionLock::Locked(StormMotion {
                direction_deg: 45.0,
                speed_kmh: 47.0,
            }),
        )
        .unwrap();
        assert_eq!(vm.text, "Storm motion NE 47 km/h, locked");
        assert!(vm.can_update);
    }

    #[wasm_bindgen_test]
    #[test]
    fn the_link_value_round_trips() {
        let m = StormMotion {
            direction_deg: 45.0,
            speed_kmh: 47.0,
        };
        assert_eq!(link_value(&m), "45,47");
        assert_eq!(parse_link_value("45,47"), Some(m));
        assert_eq!(
            parse_link_value("405.4,47.4"),
            Some(StormMotion {
                direction_deg: 45.0,
                speed_kmh: 47.0
            })
        );
        for bad in ["", "45", "45,", "x,3", "45,-3", "45,NaN", "45,900"] {
            assert_eq!(parse_link_value(bad), None, "{bad}");
        }
    }
}

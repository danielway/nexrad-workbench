//! Storm cell detection: when it runs, what result is kept, and what the
//! Layers panel shows for it.
//!
//! The detector itself (`crate::nexrad::detection`) is a pure function of
//! one sweep. Running it is an effect the shell executes against the GPU
//! renderer's CPU shadow of that sweep; this module decides *whether* to run
//! it. A result is valid for exactly the sweep and threshold it was computed
//! for ([`StormCellKey`]), so an empty result is as final as a non-empty one
//! and nothing re-runs on a repaint, a drag, or a 3D frame.

use crate::core::{DisplayedSweep, RadarProduct, StormCellInfo};
use crate::geo::ViewMode;

/// Lowest threshold the Layers slider offers, in dBZ.
pub(crate) const THRESHOLD_MIN_DBZ: f32 = 20.0;
/// Highest threshold the Layers slider offers, in dBZ.
pub(crate) const THRESHOLD_MAX_DBZ: f32 = 60.0;
/// Threshold used when a stored value is unusable.
pub(crate) const THRESHOLD_DEFAULT_DBZ: f32 = 35.0;

/// Clamp a threshold (e.g. one read back from stored preferences) to the
/// slider's range. NaN falls back to the default.
pub(crate) fn clamp_threshold(dbz: f32) -> f32 {
    if dbz.is_nan() {
        THRESHOLD_DEFAULT_DBZ
    } else {
        dbz.clamp(THRESHOLD_MIN_DBZ, THRESHOLD_MAX_DBZ)
    }
}

/// What a detection result was computed for: the on-GPU sweep (identity
/// plus its time span, which advances with every live chunk) and the
/// threshold. A result is reused only while this is unchanged.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct StormCellKey {
    pub sweep: DisplayedSweep,
    pub threshold_dbz: f32,
}

/// The current detection result and what it was computed for.
#[derive(Clone, Debug, Default)]
pub(crate) struct StormCells {
    /// Cells from the last run. Empty is a valid result.
    pub cells: Vec<StormCellInfo>,
    /// The sweep and threshold `cells` belongs to; `None` when nothing has
    /// been computed (or the result was cleared).
    pub computed_for: Option<StormCellKey>,
}

impl StormCells {
    /// Drop the result and its key.
    pub(crate) fn clear(&mut self) {
        self.cells.clear();
        self.computed_for = None;
    }

    /// Record a finished run.
    pub(crate) fn store(&mut self, key: StormCellKey, cells: Vec<StormCellInfo>) {
        self.cells = cells;
        self.computed_for = Some(key);
    }

    fn is_clear(&self) -> bool {
        self.cells.is_empty() && self.computed_for.is_none()
    }
}

/// Read-only inputs to [`decide`], shell-assembled each time it asks.
pub(crate) struct StormCellEnv<'a> {
    /// `viz_state.storm_cells_visible`.
    pub visible: bool,
    /// `viz_state.view_mode()`.
    pub view_mode: ViewMode,
    /// `viz_state.product` — the selected product.
    pub product: RadarProduct,
    /// `viz_state.displayed` — what is on the GPU main slot.
    pub displayed: Option<&'a DisplayedSweep>,
    /// `viz_state.storm_cell_threshold_dbz`.
    pub threshold_dbz: f32,
}

/// What the shell does with the storm cell result.
#[derive(Debug, PartialEq)]
pub(crate) enum StormCellAction {
    /// Leave the result as it is.
    Keep,
    /// Drop the result ([`StormCells::clear`]).
    Clear,
    /// Run detection on the on-GPU sweep and [`StormCells::store`] the
    /// result under this key.
    Run(StormCellKey),
}

/// Decide whether detection runs, the result is kept, or it is cleared.
///
/// - Off, nothing displayed, or not reflectivity (selected or on the GPU):
///   clear. Nothing runs on velocity or dual-pol data.
/// - Globe3D: keep. Cells are not drawn there, so a changed sweep waits for
///   the return to 2D.
/// - Otherwise run only when the sweep or threshold differs from what the
///   current result was computed for.
pub(crate) fn decide(env: &StormCellEnv<'_>, current: &StormCells) -> StormCellAction {
    let clear = || {
        if current.is_clear() {
            StormCellAction::Keep
        } else {
            StormCellAction::Clear
        }
    };
    if !env.visible || env.product != RadarProduct::Reflectivity {
        return clear();
    }
    let Some(displayed) = env.displayed else {
        return clear();
    };
    if displayed.identity.product != RadarProduct::Reflectivity.to_worker_string() {
        return clear();
    }
    if env.view_mode == ViewMode::Globe3D {
        return StormCellAction::Keep;
    }
    let key = StormCellKey {
        sweep: displayed.clone(),
        threshold_dbz: env.threshold_dbz,
    };
    if current.computed_for.as_ref() == Some(&key) {
        StormCellAction::Keep
    } else {
        StormCellAction::Run(key)
    }
}

/// View-model for the threshold slider under the Storm cells checkbox.
#[derive(Debug, PartialEq)]
pub(crate) struct ThresholdSliderVm {
    /// The slider accepts input.
    pub enabled: bool,
    /// Short note shown beside a disabled slider.
    pub hint: Option<&'static str>,
}

/// Detection works on reflectivity only; for any other product the slider
/// stays visible but disabled with a hint.
pub(crate) fn threshold_slider(product: RadarProduct) -> ThresholdSliderVm {
    if product == RadarProduct::Reflectivity {
        ThresholdSliderVm {
            enabled: true,
            hint: None,
        }
    } else {
        ThresholdSliderVm {
            enabled: false,
            hint: Some("Reflectivity only"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SweepIdentity;
    use crate::data::keys::UnixMillis;
    use crate::data::ScanKey;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev: u8, product: &str, end: f64) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(
                ScanKey::new("KFWS", UnixMillis(1_716_883_200_000)),
                elev,
                product,
            ),
            start_time: 1_716_883_200.0,
            end_time: end,
            elevation_deg: 0.5,
        }
    }

    fn env(displayed: Option<&DisplayedSweep>) -> StormCellEnv<'_> {
        StormCellEnv {
            visible: true,
            view_mode: ViewMode::Flat2D,
            product: RadarProduct::Reflectivity,
            displayed,
            threshold_dbz: 35.0,
        }
    }

    fn cell() -> StormCellInfo {
        StormCellInfo {
            lat: 32.0,
            lon: -97.0,
            max_dbz: 50.0,
            mean_dbz: 42.0,
            area_km2: 30.0,
            bounds: (31.9, -97.1, 32.1, -96.9),
            bearing_from_radar_deg: 0.0,
            range_from_radar_km: 10.0,
            orientation_deg: 0.0,
            elongation: 1.0,
            gate_count: 20,
        }
    }

    /// Apply an action the way the shell does, with `cells` as the
    /// detector's output.
    fn execute(state: &mut StormCells, action: StormCellAction, cells: Vec<StormCellInfo>) {
        match action {
            StormCellAction::Keep => {}
            StormCellAction::Clear => state.clear(),
            StormCellAction::Run(key) => state.store(key, cells),
        }
    }

    fn run_key(action: StormCellAction) -> StormCellKey {
        match action {
            StormCellAction::Run(key) => key,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn empty_result_is_final_for_its_sweep() {
        let s = sweep(9, "reflectivity", 1_716_883_220.0);
        let mut state = StormCells::default();

        let first = decide(&env(Some(&s)), &state);
        assert!(matches!(first, StormCellAction::Run(_)));
        execute(&mut state, first, Vec::new());

        // Every later frame (repaint, drag) over the same sweep keeps it.
        for _ in 0..300 {
            assert_eq!(decide(&env(Some(&s)), &state), StormCellAction::Keep);
        }
    }

    #[wasm_bindgen_test]
    fn new_sweep_or_threshold_reruns() {
        let s = sweep(1, "reflectivity", 1_716_883_220.0);
        let mut state = StormCells::default();
        let a = decide(&env(Some(&s)), &state);
        execute(&mut state, a, vec![cell()]);

        // Different elevation.
        let s2 = sweep(2, "reflectivity", 1_716_883_220.0);
        assert!(matches!(
            decide(&env(Some(&s2)), &state),
            StormCellAction::Run(_)
        ));

        // Same sweep identity, later end time (a new live chunk).
        let s3 = sweep(1, "reflectivity", 1_716_883_225.0);
        assert!(matches!(
            decide(&env(Some(&s3)), &state),
            StormCellAction::Run(_)
        ));

        // Same sweep, new threshold.
        let mut e = env(Some(&s));
        e.threshold_dbz = 45.0;
        let key = run_key(decide(&e, &state));
        assert_eq!(key.threshold_dbz, 45.0);
        assert_eq!(key.sweep, s);
    }

    #[wasm_bindgen_test]
    fn non_reflectivity_clears_and_never_runs() {
        let refl = sweep(1, "reflectivity", 1_716_883_220.0);
        let vel = sweep(1, "velocity", 1_716_883_220.0);
        let mut state = StormCells::default();
        let a = decide(&env(Some(&refl)), &state);
        execute(&mut state, a, vec![cell()]);

        // Selected velocity, reflectivity still on the GPU: clear.
        let mut e = env(Some(&refl));
        e.product = RadarProduct::Velocity;
        assert_eq!(decide(&e, &state), StormCellAction::Clear);
        execute(&mut state, StormCellAction::Clear, Vec::new());
        assert!(state.cells.is_empty() && state.computed_for.is_none());

        // Velocity on the GPU: nothing to do, and no run.
        e.displayed = Some(&vel);
        assert_eq!(decide(&e, &state), StormCellAction::Keep);

        // Back to reflectivity selected while velocity is still displayed:
        // still no run on velocity data.
        e.product = RadarProduct::Reflectivity;
        assert_eq!(decide(&e, &state), StormCellAction::Keep);

        // Clutter filter power shares the reflectivity worker string but is
        // not reflectivity.
        let mut e = env(Some(&refl));
        e.product = RadarProduct::ClutterFilterPower;
        assert_eq!(decide(&e, &state), StormCellAction::Keep);

        // Reflectivity arrives: run.
        assert!(matches!(
            decide(&env(Some(&refl)), &state),
            StormCellAction::Run(_)
        ));
    }

    #[wasm_bindgen_test]
    fn globe_defers_until_back_in_2d() {
        let s1 = sweep(1, "reflectivity", 1_716_883_220.0);
        let s2 = sweep(1, "reflectivity", 1_716_883_520.0);
        let mut state = StormCells::default();
        let a = decide(&env(Some(&s1)), &state);
        execute(&mut state, a, vec![cell()]);

        let mut e = env(Some(&s2));
        e.view_mode = ViewMode::Globe3D;
        assert_eq!(decide(&e, &state), StormCellAction::Keep);

        // Fresh state in 3D: still no run.
        assert_eq!(decide(&e, &StormCells::default()), StormCellAction::Keep);

        // Back in 2D with a changed sweep: run once.
        e.view_mode = ViewMode::Flat2D;
        let key = run_key(decide(&e, &state));
        assert_eq!(key.sweep, s2);

        // Back in 2D with the same sweep: no run.
        assert_eq!(decide(&env(Some(&s1)), &state), StormCellAction::Keep);
    }

    #[wasm_bindgen_test]
    fn off_or_blank_canvas_clears() {
        let s = sweep(1, "reflectivity", 1_716_883_220.0);
        let mut state = StormCells::default();
        let a = decide(&env(Some(&s)), &state);
        execute(&mut state, a, vec![cell()]);

        let mut off = env(Some(&s));
        off.visible = false;
        assert_eq!(decide(&off, &state), StormCellAction::Clear);
        assert_eq!(decide(&env(None), &state), StormCellAction::Clear);

        // Already clear: nothing to do.
        assert_eq!(
            decide(&env(None), &StormCells::default()),
            StormCellAction::Keep
        );
    }

    #[wasm_bindgen_test]
    fn threshold_is_clamped_to_slider_range() {
        assert_eq!(clamp_threshold(35.0), 35.0);
        assert_eq!(clamp_threshold(5.0), THRESHOLD_MIN_DBZ);
        assert_eq!(clamp_threshold(90.0), THRESHOLD_MAX_DBZ);
        assert_eq!(clamp_threshold(f32::NAN), THRESHOLD_DEFAULT_DBZ);
        assert_eq!(clamp_threshold(f32::INFINITY), THRESHOLD_MAX_DBZ);
    }

    #[wasm_bindgen_test]
    fn slider_is_disabled_with_hint_off_reflectivity() {
        assert_eq!(
            threshold_slider(RadarProduct::Reflectivity),
            ThresholdSliderVm {
                enabled: true,
                hint: None
            }
        );
        for &p in RadarProduct::all() {
            if p != RadarProduct::Reflectivity {
                let vm = threshold_slider(p);
                assert!(!vm.enabled);
                assert_eq!(vm.hint, Some("Reflectivity only"));
            }
        }
    }
}

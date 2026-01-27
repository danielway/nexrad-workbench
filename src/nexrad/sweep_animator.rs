//! Sweep animation controller for radial-accurate playback.
//!
//! Manages sweep-by-sweep animation synchronized with actual radar scanning
//! patterns, enabling accurate representation of data collection timing.

use crate::state::radar_data::{Scan, Sweep};

/// Current animation state returned by the SweepAnimator.
///
/// Contains all information needed to render the correct portion of radar data
/// at the current playback position.
#[derive(Clone, Debug)]
pub struct AnimationState {
    /// Index of the current sweep being displayed within the scan.
    pub sweep_index: usize,

    /// Current elevation angle (degrees) from the sweep.
    pub elevation: f32,

    /// Animated azimuth position (0-360 degrees) based on animation progress.
    /// Represents where the radar beam would be at this moment.
    pub azimuth: f32,

    /// Progress through the current sweep (0.0 - 1.0).
    pub sweep_progress: f32,

    /// Progress through the entire scan (0.0 - 1.0).
    pub scan_progress: f32,

    /// Azimuth range to render for partial sweep display.
    /// When `None`, render the entire sweep.
    /// When `Some(range)`, only render radials within this azimuth range.
    pub render_azimuth_range: Option<(f32, f32)>,

    /// Whether this animation state is valid (scan data available).
    pub is_valid: bool,
}

impl Default for AnimationState {
    fn default() -> Self {
        Self {
            sweep_index: 0,
            elevation: 0.5,
            azimuth: 0.0,
            sweep_progress: 0.0,
            scan_progress: 0.0,
            render_azimuth_range: None,
            is_valid: false,
        }
    }
}

/// Manages sweep-by-sweep animation for radial-accurate playback.
///
/// The animator tracks playback position relative to scan timing data
/// and calculates which radials should be visible at any given moment.
#[derive(Default)]
pub struct SweepAnimator {
    /// Cached scan start time to detect scan changes.
    last_scan_start: Option<f64>,

    /// Whether partial sweep animation is enabled.
    /// When disabled, always shows complete sweeps.
    partial_sweep_enabled: bool,
}

impl SweepAnimator {
    /// Create a new SweepAnimator.
    pub fn new() -> Self {
        Self {
            last_scan_start: None,
            partial_sweep_enabled: false,
        }
    }

    /// Enable or disable partial sweep animation.
    ///
    /// When enabled, the animator calculates azimuth ranges for partial sweep
    /// display based on playback position. When disabled (default), complete
    /// sweeps are always rendered.
    pub fn set_partial_sweep_enabled(&mut self, enabled: bool) {
        self.partial_sweep_enabled = enabled;
    }

    /// Update animation state based on playback position and current scan.
    ///
    /// # Arguments
    /// * `playback_position` - Current playback timestamp (Unix seconds)
    /// * `scan` - The scan containing timing information (may be `None`)
    ///
    /// # Returns
    /// `AnimationState` with current sweep index, azimuth, and render range
    pub fn update(&mut self, playback_position: f64, scan: Option<&Scan>) -> AnimationState {
        let Some(scan) = scan else {
            self.last_scan_start = None;
            return AnimationState::default();
        };

        // Check if we've switched to a different scan
        let scan_changed = self.last_scan_start != Some(scan.start_time);
        if scan_changed {
            self.last_scan_start = Some(scan.start_time);
        }

        // Calculate overall scan progress
        let scan_duration = scan.end_time - scan.start_time;
        let time_into_scan = (playback_position - scan.start_time).clamp(0.0, scan_duration);
        let scan_progress = if scan_duration > 0.0 {
            (time_into_scan / scan_duration) as f32
        } else {
            0.0
        };

        // Find the current sweep based on playback position
        let (sweep_index, sweep) = match scan.find_sweep_at_timestamp(playback_position) {
            Some((idx, sweep)) => (idx, sweep),
            None => {
                // Position is before first sweep or after last - find closest
                if playback_position < scan.start_time {
                    // Before scan start - use first sweep
                    if let Some(first) = scan.sweeps.first() {
                        (0, first)
                    } else {
                        return AnimationState::default();
                    }
                } else {
                    // After scan end - use last sweep
                    if let Some(last) = scan.sweeps.last() {
                        (scan.sweeps.len() - 1, last)
                    } else {
                        return AnimationState::default();
                    }
                }
            }
        };

        let (azimuth, sweep_progress, render_range) =
            self.calculate_sweep_animation(playback_position, sweep);

        AnimationState {
            sweep_index,
            elevation: sweep.elevation,
            azimuth,
            sweep_progress,
            scan_progress,
            render_azimuth_range: render_range,
            is_valid: true,
        }
    }

    /// Calculate animation parameters within a single sweep.
    fn calculate_sweep_animation(
        &self,
        playback_position: f64,
        sweep: &Sweep,
    ) -> (f32, f32, Option<(f32, f32)>) {
        let sweep_duration = sweep.end_time - sweep.start_time;
        let time_into_sweep = (playback_position - sweep.start_time).clamp(0.0, sweep_duration);

        let sweep_progress = if sweep_duration > 0.0 {
            (time_into_sweep / sweep_duration) as f32
        } else {
            1.0
        };

        // Calculate azimuth based on progress (assuming 360 degree rotation)
        // Standard radar scan direction is clockwise from north
        let azimuth = (sweep_progress * 360.0) % 360.0;

        // Calculate render range if partial sweep animation is enabled
        let render_range = if self.partial_sweep_enabled && sweep_progress < 1.0 {
            // Render from 0 to current azimuth
            Some((0.0, azimuth))
        } else {
            // Render entire sweep
            None
        };

        (azimuth, sweep_progress, render_range)
    }

    /// Get animation state for real-time mode.
    ///
    /// In real-time mode, we show what data has been received so far,
    /// with a shaded region indicating expected future data.
    pub fn realtime_state(
        &self,
        current_time: f64,
        last_radial_time: Option<f64>,
        scan: Option<&Scan>,
    ) -> AnimationState {
        let Some(scan) = scan else {
            return AnimationState::default();
        };

        // Calculate how far into the scan we are based on last received radial
        let last_time = last_radial_time.unwrap_or(current_time);

        // Find which sweep the last radial belongs to
        let (sweep_index, sweep) = match scan.find_sweep_at_timestamp(last_time) {
            Some((idx, sweep)) => (idx, sweep),
            None => {
                if let Some(first) = scan.sweeps.first() {
                    (0, first)
                } else {
                    return AnimationState::default();
                }
            }
        };

        // Calculate progress based on last received data
        let sweep_duration = sweep.end_time - sweep.start_time;
        let time_into_sweep = (last_time - sweep.start_time).clamp(0.0, sweep_duration);
        let sweep_progress = if sweep_duration > 0.0 {
            (time_into_sweep / sweep_duration) as f32
        } else {
            0.0
        };

        let azimuth = (sweep_progress * 360.0) % 360.0;

        let scan_duration = scan.end_time - scan.start_time;
        let time_into_scan = (last_time - scan.start_time).clamp(0.0, scan_duration);
        let scan_progress = if scan_duration > 0.0 {
            (time_into_scan / scan_duration) as f32
        } else {
            0.0
        };

        AnimationState {
            sweep_index,
            elevation: sweep.elevation,
            azimuth,
            sweep_progress,
            scan_progress,
            render_azimuth_range: Some((0.0, azimuth)), // Show only received data
            is_valid: true,
        }
    }
}

/// Estimate azimuth from radials list based on sweep progress.
///
/// If actual radial data is available, uses the closest radial's azimuth.
/// Otherwise falls back to linear interpolation.
pub fn estimate_azimuth_from_radials(
    radials: &[crate::state::radar_data::Radial],
    sweep_progress: f32,
) -> f32 {
    if radials.is_empty() {
        return sweep_progress * 360.0;
    }

    // Find radial closest to our progress position
    let target_idx = ((radials.len() as f32) * sweep_progress) as usize;
    let idx = target_idx.min(radials.len() - 1);

    radials[idx].azimuth
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_scan() -> Scan {
        use crate::state::radar_data::{Radial, Sweep};

        let sweeps = vec![
            Sweep {
                start_time: 0.0,
                end_time: 20.0,
                elevation: 0.5,
                radials: (0..360)
                    .map(|i| Radial {
                        start_time: (i as f64) * (20.0 / 360.0),
                        duration: 20.0 / 360.0,
                        azimuth: i as f32,
                    })
                    .collect(),
            },
            Sweep {
                start_time: 20.0,
                end_time: 40.0,
                elevation: 1.0,
                radials: vec![],
            },
        ];

        Scan {
            start_time: 0.0,
            end_time: 40.0,
            vcp: 215,
            sweeps,
            completeness: None,
            present_records: None,
            expected_records: None,
        }
    }

    #[test]
    fn test_animation_at_scan_start() {
        let mut animator = SweepAnimator::new();
        let scan = create_test_scan();

        let state = animator.update(0.0, Some(&scan));

        assert!(state.is_valid);
        assert_eq!(state.sweep_index, 0);
        assert!((state.elevation - 0.5).abs() < 0.01);
        assert!((state.azimuth - 0.0).abs() < 1.0);
        assert!((state.sweep_progress - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_animation_mid_sweep() {
        let mut animator = SweepAnimator::new();
        let scan = create_test_scan();

        let state = animator.update(10.0, Some(&scan));

        assert!(state.is_valid);
        assert_eq!(state.sweep_index, 0);
        assert!((state.sweep_progress - 0.5).abs() < 0.01);
        assert!((state.azimuth - 180.0).abs() < 1.0);
    }

    #[test]
    fn test_animation_second_sweep() {
        let mut animator = SweepAnimator::new();
        let scan = create_test_scan();

        let state = animator.update(30.0, Some(&scan));

        assert!(state.is_valid);
        assert_eq!(state.sweep_index, 1);
        assert!((state.elevation - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_no_scan() {
        let mut animator = SweepAnimator::new();
        let state = animator.update(10.0, None);

        assert!(!state.is_valid);
    }
}

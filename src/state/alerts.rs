//! NWS weather alert data structures.

use eframe::egui::Color32;

/// Severity level for NWS alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    /// Lowest priority - general information
    Statement,
    /// Advisory - be aware
    Advisory,
    /// Watch - conditions favorable
    Watch,
    /// Warning - imminent or occurring
    Warning,
}

impl AlertSeverity {
    /// Display label for the severity level.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Statement => "Statement",
            Self::Advisory => "Advisory",
            Self::Watch => "Watch",
            Self::Warning => "Warning",
        }
    }

    /// Color for rendering this severity level (fill color).
    pub fn fill_color(&self) -> Color32 {
        match self {
            Self::Statement => Color32::from_rgba_unmultiplied(100, 100, 180, 40),
            Self::Advisory => Color32::from_rgba_unmultiplied(180, 180, 80, 50),
            Self::Watch => Color32::from_rgba_unmultiplied(255, 180, 0, 60),
            Self::Warning => Color32::from_rgba_unmultiplied(255, 60, 60, 70),
        }
    }

    /// Color for rendering this severity level (stroke color).
    pub fn stroke_color(&self) -> Color32 {
        match self {
            Self::Statement => Color32::from_rgb(120, 120, 200),
            Self::Advisory => Color32::from_rgb(200, 200, 100),
            Self::Watch => Color32::from_rgb(255, 200, 50),
            Self::Warning => Color32::from_rgb(255, 80, 80),
        }
    }
}

/// Type of NWS alert product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertType {
    /// Tornado Warning
    TornadoWarning,
    /// Severe Thunderstorm Warning
    SevereThunderstormWarning,
    /// Tornado Watch
    TornadoWatch,
    /// Severe Thunderstorm Watch
    SevereThunderstormWatch,
    /// Flash Flood Warning
    FlashFloodWarning,
    /// Flash Flood Watch
    FlashFloodWatch,
    /// Special Weather Statement
    SpecialWeatherStatement,
    /// Mesoscale Discussion
    MesoscaleDiscussion,
}

impl AlertType {
    /// Display label for the alert type.
    pub fn label(&self) -> &'static str {
        match self {
            Self::TornadoWarning => "Tornado Warning",
            Self::SevereThunderstormWarning => "Severe T-Storm Warning",
            Self::TornadoWatch => "Tornado Watch",
            Self::SevereThunderstormWatch => "Severe T-Storm Watch",
            Self::FlashFloodWarning => "Flash Flood Warning",
            Self::FlashFloodWatch => "Flash Flood Watch",
            Self::SpecialWeatherStatement => "Special Weather Statement",
            Self::MesoscaleDiscussion => "Mesoscale Discussion",
        }
    }

    /// Short label for compact display.
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::TornadoWarning => "TOR",
            Self::SevereThunderstormWarning => "SVR",
            Self::TornadoWatch => "TOA",
            Self::SevereThunderstormWatch => "SVA",
            Self::FlashFloodWarning => "FFW",
            Self::FlashFloodWatch => "FFA",
            Self::SpecialWeatherStatement => "SPS",
            Self::MesoscaleDiscussion => "MCD",
        }
    }

    /// Get the severity level for this alert type.
    pub fn severity(&self) -> AlertSeverity {
        match self {
            Self::TornadoWarning | Self::SevereThunderstormWarning | Self::FlashFloodWarning => {
                AlertSeverity::Warning
            }
            Self::TornadoWatch | Self::SevereThunderstormWatch | Self::FlashFloodWatch => {
                AlertSeverity::Watch
            }
            Self::SpecialWeatherStatement => AlertSeverity::Advisory,
            Self::MesoscaleDiscussion => AlertSeverity::Statement,
        }
    }
}

/// A single NWS weather alert.
#[derive(Debug, Clone)]
pub struct NwsAlert {
    /// Unique identifier for the alert.
    pub id: String,
    /// Type of alert.
    pub alert_type: AlertType,
    /// Headline/title text.
    pub headline: String,
    /// Issuing Weather Forecast Office.
    pub wfo: String,
    /// Start time (Unix timestamp).
    pub start_time: f64,
    /// End time (Unix timestamp).
    pub end_time: f64,
    /// Polygon vertices as (lat, lon) pairs.
    pub polygon: Vec<(f64, f64)>,
}

impl NwsAlert {
    /// Get the severity of this alert.
    pub fn severity(&self) -> AlertSeverity {
        self.alert_type.severity()
    }

    /// Check if the alert is active at the given timestamp.
    pub fn is_active_at(&self, timestamp: f64) -> bool {
        timestamp >= self.start_time && timestamp <= self.end_time
    }
}

/// Collection of NWS alerts with summary statistics.
#[derive(Default, Clone)]
pub struct AlertsState {
    /// All current alerts.
    pub alerts: Vec<NwsAlert>,
}

impl AlertsState {
    /// Create alerts state with dummy data for UI testing.
    pub fn with_dummy_data() -> Self {
        let now = 1714564800.0_f64; // 2024-05-01 12:00:00 UTC (matches AppState demo time)

        // KDMX (Des Moines) is at approximately 41.73°N, 93.72°W
        Self {
            alerts: vec![
                // Tornado Warning - storm-relative polygon tracking NE motion
                NwsAlert {
                    id: "TOR-001".to_string(),
                    alert_type: AlertType::TornadoWarning,
                    headline: "Tornado Warning for Polk County".to_string(),
                    wfo: "DMX".to_string(),
                    start_time: now - 600.0, // Started 10 min ago
                    end_time: now + 1800.0,  // Expires in 30 min
                    polygon: vec![
                        (41.62, -93.78),
                        (41.68, -93.72),
                        (41.78, -93.58),
                        (41.82, -93.52),
                        (41.79, -93.48),
                        (41.72, -93.51),
                        (41.65, -93.62),
                        (41.58, -93.71),
                    ],
                },
                // Severe Thunderstorm Warning - larger irregular polygon
                NwsAlert {
                    id: "SVR-001".to_string(),
                    alert_type: AlertType::SevereThunderstormWarning,
                    headline: "Severe Thunderstorm Warning".to_string(),
                    wfo: "DMX".to_string(),
                    start_time: now - 1200.0, // Started 20 min ago
                    end_time: now + 2400.0,   // Expires in 40 min
                    polygon: vec![
                        (41.52, -94.08),
                        (41.58, -93.95),
                        (41.72, -93.82),
                        (41.88, -93.68),
                        (41.92, -93.72),
                        (41.85, -93.88),
                        (41.75, -94.02),
                        (41.62, -94.12),
                    ],
                },
                // Tornado Watch - parallelogram shape (typical SPC watch box)
                NwsAlert {
                    id: "TOA-001".to_string(),
                    alert_type: AlertType::TornadoWatch,
                    headline: "Tornado Watch 123".to_string(),
                    wfo: "SPC".to_string(),
                    start_time: now - 7200.0, // Started 2 hours ago
                    end_time: now + 14400.0,  // Expires in 4 hours
                    polygon: vec![
                        (42.45, -94.20),
                        (42.60, -92.80),
                        (41.15, -92.35),
                        (41.00, -93.75),
                    ],
                },
                // Mesoscale Discussion - irregular oval-ish shape
                NwsAlert {
                    id: "MCD-001".to_string(),
                    alert_type: AlertType::MesoscaleDiscussion,
                    headline: "Mesoscale Discussion 0542".to_string(),
                    wfo: "SPC".to_string(),
                    start_time: now - 3600.0, // Started 1 hour ago
                    end_time: now + 3600.0,   // Expires in 1 hour
                    polygon: vec![
                        (42.15, -93.95),
                        (42.22, -93.55),
                        (42.10, -93.18),
                        (41.85, -93.02),
                        (41.55, -93.12),
                        (41.42, -93.48),
                        (41.50, -93.88),
                        (41.72, -94.08),
                        (41.98, -94.05),
                    ],
                },
            ],
        }
    }

    /// Get alerts active at the given timestamp.
    pub fn active_alerts(&self, timestamp: f64) -> Vec<&NwsAlert> {
        self.alerts
            .iter()
            .filter(|a| a.is_active_at(timestamp))
            .collect()
    }

    /// Count alerts by severity level.
    pub fn count_by_severity(&self, timestamp: f64) -> AlertSummary {
        let active = self.active_alerts(timestamp);
        AlertSummary {
            warnings: active
                .iter()
                .filter(|a| a.severity() == AlertSeverity::Warning)
                .count(),
            watches: active
                .iter()
                .filter(|a| a.severity() == AlertSeverity::Watch)
                .count(),
            advisories: active
                .iter()
                .filter(|a| a.severity() == AlertSeverity::Advisory)
                .count(),
            statements: active
                .iter()
                .filter(|a| a.severity() == AlertSeverity::Statement)
                .count(),
        }
    }
}

/// Summary counts of active alerts by severity.
#[derive(Default, Clone, Copy)]
pub struct AlertSummary {
    pub warnings: usize,
    pub watches: usize,
    pub advisories: usize,
    pub statements: usize,
}

impl AlertSummary {
    /// Total number of active alerts.
    pub fn total(&self) -> usize {
        self.warnings + self.watches + self.advisories + self.statements
    }

    /// Check if there are any active alerts.
    pub fn has_alerts(&self) -> bool {
        self.total() > 0
    }
}

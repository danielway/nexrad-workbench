//! Acquisition drawer: expandable panel showing the download queue and network activity.
//!
//! Renders above the existing bottom panel controls when expanded. Contains two tabs:
//! - Queue: shows acquisition operations with status, progress, and controls
//! - Network: shows network requests grouped by operation

use super::colors::{acquisition as acq_colors, ui as ui_colors};
use crate::state::{
    format_bytes, AcquisitionState, AppCommand, AppState, DrawerTab, OperationId, OperationStatus,
    QueueState,
};
use eframe::egui::{self, Color32, RichText, ScrollArea};
use egui_phosphor::regular as icons;

/// Render the acquisition drawer content inside the bottom panel.
pub fn render_acquisition_drawer(ui: &mut egui::Ui, state: &mut AppState, height: f32) {
    let dark = state.is_dark;

    // Drawer container
    ui.allocate_ui(egui::Vec2::new(ui.available_width(), height), |ui| {
        // Tab bar + controls
        ui.horizontal(|ui| {
            // Tab buttons
            let queue_label = format!("{} Queue", icons::QUEUE);
            if ui
                .selectable_label(
                    state.acquisition.active_tab == DrawerTab::Queue,
                    RichText::new(queue_label).size(10.0).strong(),
                )
                .clicked()
            {
                state.acquisition.active_tab = DrawerTab::Queue;
            }

            let net_label = format!("{} Network", icons::WIFI_HIGH);
            if ui
                .selectable_label(
                    state.acquisition.active_tab == DrawerTab::Network,
                    RichText::new(net_label).size(10.0).strong(),
                )
                .clicked()
            {
                state.acquisition.active_tab = DrawerTab::Network;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Queue controls (only when Queue tab is active)
                if state.acquisition.active_tab == DrawerTab::Queue {
                    if state.acquisition.is_paused() {
                        if ui.small_button(format!("{} Resume", icons::PLAY)).clicked() {
                            state.push_command(AppCommand::ResumeQueue);
                        }
                    } else if state.acquisition.has_active_operations()
                        && ui.small_button(format!("{} Pause", icons::PAUSE)).clicked()
                    {
                        state.push_command(AppCommand::PauseQueue);
                    }
                } else {
                    // Network tab: link to full log
                    if ui.small_button("Full Log").clicked() {
                        state.network_log_open = true;
                    }
                }
            });
        });

        // Error-pause banner
        if state.acquisition.queue_state == QueueState::ErrorPaused {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} Queue paused due to error", icons::WARNING))
                        .size(10.0)
                        .strong()
                        .color(acq_colors::FAILED),
                );

                if let Some(err_op_id) = state.acquisition.error_pause_operation_id {
                    if ui.small_button("Retry").clicked() {
                        state.push_command(AppCommand::RetryFailed(err_op_id));
                    }
                    if ui.small_button("Skip").clicked() {
                        state.push_command(AppCommand::SkipFailed(err_op_id));
                    }
                }
                if ui.small_button("Resume").clicked() {
                    state.push_command(AppCommand::ResumeQueue);
                }
            });
            ui.separator();
        }

        // Tab content
        match state.acquisition.active_tab {
            DrawerTab::Queue => render_queue_tab(ui, state, dark),
            DrawerTab::Network => render_network_tab(ui, state, dark),
        }
    });
}

/// Render the Queue tab content.
fn render_queue_tab(ui: &mut egui::Ui, state: &mut AppState, dark: bool) {
    let label_color = ui_colors::label(dark);

    // Streaming latency section (shown in live mode)
    if state.live_mode_state.is_active() {
        if let Some(summary) = state.acquisition.latency_summary() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Chunk Latency:")
                        .size(10.0)
                        .color(label_color),
                );
                ui.label(
                    RichText::new(format!(
                        "avg {:.0}ms \u{00b7} p95 {:.0}ms",
                        summary.avg_fetch_ms, summary.p95_fetch_ms
                    ))
                    .size(10.0)
                    .color(ui_colors::value(dark)),
                );
                if let Some(e2e) = summary.avg_e2e_ms {
                    ui.label(
                        RichText::new(format!("\u{00b7} e2e {:.1}s", e2e / 1000.0))
                            .size(10.0)
                            .color(ui_colors::value(dark)),
                    );
                }
            });

            // Sparkline of last 20 chunk latencies
            let latencies: Vec<f64> = state
                .acquisition
                .chunk_latencies
                .iter()
                .rev()
                .take(20)
                .map(|c| c.fetch_latency_ms)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            if latencies.len() >= 2 {
                render_sparkline(ui, &latencies, dark);
            }
            ui.separator();
        }
    }

    // Operation list
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.acquisition.operations.is_empty() {
                ui.label(
                    RichText::new("No acquisition operations.")
                        .size(10.0)
                        .italics()
                        .color(label_color),
                );
                return;
            }

            // Collect operations for display (newest at top for active/queued, then completed)
            let mut commands_to_push: Vec<AppCommand> = Vec::new();

            // Display operations in reverse order (most recent first)
            let ops: Vec<_> = state.acquisition.operations.iter().rev().cloned().collect();
            for op in &ops {
                ui.horizontal(|ui| {
                    // Status icon
                    let (icon, color) = match &op.status {
                        OperationStatus::Active => (icons::SPINNER, acq_colors::ACTIVE),
                        OperationStatus::Queued => (icons::CLOCK, acq_colors::QUEUED),
                        OperationStatus::Completed { .. } => {
                            (icons::CHECK_CIRCLE, acq_colors::COMPLETED)
                        }
                        OperationStatus::Failed { .. } => (icons::X_CIRCLE, acq_colors::FAILED),
                        OperationStatus::Cancelled => (icons::MINUS_CIRCLE, acq_colors::CANCELLED),
                    };
                    ui.label(RichText::new(icon).size(10.0).color(color));

                    // Description
                    let desc = AcquisitionState::operation_description(&op.kind);
                    ui.label(
                        RichText::new(&desc)
                            .size(10.0)
                            .color(ui_colors::value(dark)),
                    );

                    // Status text
                    match &op.status {
                        OperationStatus::Active => {
                            ui.label(
                                RichText::new("Downloading...")
                                    .size(10.0)
                                    .italics()
                                    .color(acq_colors::ACTIVE),
                            );
                        }
                        OperationStatus::Queued => {
                            ui.label(RichText::new("Queued").size(10.0).color(label_color));
                        }
                        OperationStatus::Completed { duration_ms, bytes } => {
                            ui.label(
                                RichText::new(format!(
                                    "{:.1}s  {}",
                                    duration_ms / 1000.0,
                                    format_bytes(*bytes)
                                ))
                                .size(10.0)
                                .color(label_color),
                            );
                        }
                        OperationStatus::Failed { error } => {
                            ui.label(RichText::new(error).size(10.0).color(acq_colors::FAILED))
                                .on_hover_text(error);
                        }
                        OperationStatus::Cancelled => {
                            ui.label(
                                RichText::new("Cancelled")
                                    .size(10.0)
                                    .color(acq_colors::CANCELLED),
                            );
                        }
                    }

                    // Action buttons (right-aligned)
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| match &op.status {
                            OperationStatus::Queued => {
                                if ui.small_button(icons::X).on_hover_text("Cancel").clicked() {
                                    commands_to_push.push(AppCommand::CancelOperation(op.id));
                                }
                                if ui
                                    .small_button(icons::CARET_DOWN)
                                    .on_hover_text("Move down")
                                    .clicked()
                                {
                                    commands_to_push.push(AppCommand::ReorderOperation(op.id, 1));
                                }
                                if ui
                                    .small_button(icons::CARET_UP)
                                    .on_hover_text("Move up")
                                    .clicked()
                                {
                                    commands_to_push.push(AppCommand::ReorderOperation(op.id, -1));
                                }
                            }
                            OperationStatus::Failed { .. } => {
                                if ui
                                    .small_button(format!("{} Retry", icons::ARROW_CLOCKWISE))
                                    .clicked()
                                {
                                    commands_to_push.push(AppCommand::RetryFailed(op.id));
                                }
                                if ui
                                    .small_button(format!("{} Skip", icons::SKIP_FORWARD))
                                    .clicked()
                                {
                                    commands_to_push.push(AppCommand::SkipFailed(op.id));
                                }
                            }
                            _ => {}
                        },
                    );
                });
            }

            // Push any deferred commands
            for cmd in commands_to_push {
                state.push_command(cmd);
            }
        });
}

/// Render the Network tab content with operations grouped by operation_id.
fn render_network_tab(ui: &mut egui::Ui, state: &mut AppState, dark: bool) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);

    // Group requests by operation_id
    let mut grouped: std::collections::HashMap<Option<OperationId>, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, req) in state.recent_network_requests.iter().enumerate() {
        grouped.entry(req.operation_id).or_default().push(idx);
    }

    // Aggregate summary
    let total_reqs = state.recent_network_requests.len();
    let total_bytes: u64 = state.recent_network_requests.iter().map(|r| r.bytes).sum();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} requests \u{00b7} {}",
                total_reqs,
                format_bytes(total_bytes)
            ))
            .size(10.0)
            .color(value_color),
        );
    });
    ui.separator();

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.recent_network_requests.is_empty() {
                ui.label(
                    RichText::new("No network requests captured yet.")
                        .size(10.0)
                        .italics()
                        .color(label_color),
                );
                return;
            }

            // Render grouped operations first (sorted by most recent request)
            let mut op_groups: Vec<(Option<OperationId>, Vec<usize>)> =
                grouped.into_iter().collect();
            op_groups.sort_by(|a, b| {
                // Groups with operation IDs first, then ungrouped
                match (a.0, b.0) {
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    _ => {
                        // Sort by most recent request timestamp (descending)
                        let a_max =
                            a.1.iter()
                                .filter_map(|&i| {
                                    state.recent_network_requests.get(i).map(|r| r.timestamp_ms)
                                })
                                .fold(0.0f64, f64::max);
                        let b_max =
                            b.1.iter()
                                .filter_map(|&i| {
                                    state.recent_network_requests.get(i).map(|r| r.timestamp_ms)
                                })
                                .fold(0.0f64, f64::max);
                        b_max
                            .partial_cmp(&a_max)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }
                }
            });

            // Track which groups to toggle (can't mutate during iteration)
            let mut toggle_group: Option<Option<OperationId>> = None;

            for (op_id, indices) in &op_groups {
                let group_bytes: u64 = indices
                    .iter()
                    .filter_map(|&i| state.recent_network_requests.get(i).map(|r| r.bytes))
                    .sum();
                let group_duration: f64 = indices
                    .iter()
                    .filter_map(|&i| state.recent_network_requests.get(i).map(|r| r.duration_ms))
                    .sum();

                let is_expanded = state.acquisition.expanded_network_groups.contains(op_id);
                let arrow = if is_expanded {
                    icons::CARET_DOWN
                } else {
                    icons::CARET_RIGHT
                };

                // Group header
                let group_name = match op_id {
                    Some(id) => state
                        .acquisition
                        .find(*id)
                        .map(|op| AcquisitionState::operation_description(&op.kind))
                        .unwrap_or_else(|| format!("Op #{}", id)),
                    None => "Ungrouped".to_string(),
                };

                let header_text = format!(
                    "{} {}  ({} req, {}, {:.0}ms)",
                    arrow,
                    group_name,
                    indices.len(),
                    format_bytes(group_bytes),
                    group_duration,
                );

                if ui
                    .add(
                        egui::Label::new(
                            RichText::new(header_text)
                                .size(10.0)
                                .strong()
                                .color(ui_colors::ACTIVE),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .clicked()
                {
                    toggle_group = Some(*op_id);
                }

                // Expanded: show individual requests
                if is_expanded {
                    for &idx in indices {
                        if let Some(req) = state.recent_network_requests.get(idx) {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);

                                let short_url = shorten_url(&req.url);
                                let status_color = http_status_color(req.status, req.ok);

                                ui.label(
                                    RichText::new(&short_url)
                                        .size(9.0)
                                        .monospace()
                                        .color(value_color),
                                )
                                .on_hover_text(&req.url);

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(format!("{:.0}ms", req.duration_ms))
                                                .size(9.0)
                                                .monospace()
                                                .color(value_color),
                                        );
                                        ui.add_space(6.0);
                                        let size_str = if req.bytes > 0 {
                                            format_bytes(req.bytes)
                                        } else {
                                            "--".to_string()
                                        };
                                        ui.label(
                                            RichText::new(size_str)
                                                .size(9.0)
                                                .monospace()
                                                .color(value_color),
                                        );
                                        ui.add_space(6.0);
                                        let status_str = if req.status > 0 {
                                            format!("{}", req.status)
                                        } else {
                                            "ERR".to_string()
                                        };
                                        ui.label(
                                            RichText::new(status_str)
                                                .size(9.0)
                                                .monospace()
                                                .color(status_color),
                                        );
                                    },
                                );
                            });
                        }
                    }
                }
            }

            // Apply group toggle
            if let Some(group_key) = toggle_group {
                if state
                    .acquisition
                    .expanded_network_groups
                    .contains(&group_key)
                {
                    state.acquisition.expanded_network_groups.remove(&group_key);
                } else {
                    state.acquisition.expanded_network_groups.insert(group_key);
                }
            }
        });
}

/// Render a simple sparkline chart of latency values.
fn render_sparkline(ui: &mut egui::Ui, values: &[f64], _dark: bool) {
    let (response, painter) = ui.allocate_painter(
        egui::Vec2::new(ui.available_width().min(200.0), 16.0),
        egui::Sense::hover(),
    );
    let rect = response.rect;

    if values.is_empty() {
        return;
    }

    let max_val = values.iter().cloned().fold(f64::MIN, f64::max).max(1.0);
    let bar_width = rect.width() / values.len() as f32;

    for (i, &val) in values.iter().enumerate() {
        let height_frac = (val / max_val) as f32;
        let bar_height = height_frac * rect.height();
        let x = rect.left() + i as f32 * bar_width;
        let bar_rect = egui::Rect::from_min_max(
            egui::Pos2::new(x, rect.bottom() - bar_height),
            egui::Pos2::new(x + bar_width - 1.0, rect.bottom()),
        );

        let color = if val > max_val * 0.8 {
            acq_colors::FAILED.linear_multiply(0.6)
        } else {
            acq_colors::ACTIVE.linear_multiply(0.5)
        };
        painter.rect_filled(bar_rect, 1.0, color);
    }
}

/// Shorten a URL for compact display.
fn shorten_url(url: &str) -> String {
    if let Some(after_scheme) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let parts: Vec<&str> = after_scheme.splitn(2, '/').collect();
        let host = parts[0];
        let path = parts.get(1).unwrap_or(&"");
        let last_segment = path.rsplit('/').next().unwrap_or("");

        if last_segment.is_empty() {
            host.to_string()
        } else if last_segment.len() > 35 {
            format!(".../{}", &last_segment[last_segment.len() - 30..])
        } else {
            format!(".../{}", last_segment)
        }
    } else if url.len() > 50 {
        format!("...{}", &url[url.len() - 47..])
    } else {
        url.to_string()
    }
}

/// Color code by HTTP status range.
fn http_status_color(status: u16, ok: bool) -> Color32 {
    if !ok || status == 0 || status >= 400 {
        Color32::from_rgb(255, 100, 100)
    } else if status >= 300 {
        Color32::from_rgb(255, 200, 80)
    } else {
        Color32::from_rgb(100, 200, 100)
    }
}

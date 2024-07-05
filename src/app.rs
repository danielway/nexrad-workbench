use std::sync::{Arc, Mutex};
use chrono::{NaiveDate, NaiveTime};
use log::info;
use wasm_bindgen_futures::spawn_local;

pub struct NEXRADWorkbench {
    state: Arc<Mutex<WorkbenchState>>,
}

struct WorkbenchState {
    site_string: String,
    date_string: String,
    time_string: String,
}

impl Default for NEXRADWorkbench {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkbenchState {
                site_string: String::from("KDMX"),
                date_string: String::from("03/05/2022"),
                time_string: String::from("23:30"),
            })),
        }
    }
}

impl NEXRADWorkbench {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Default::default()
    }
}

impl eframe::App for NEXRADWorkbench {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                let is_web = cfg!(target_arch = "wasm32");
                if !is_web {
                    ui.menu_button("File", |ui| {
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.add_space(16.0);
                }
            });
        });

        egui::SidePanel::left("side_panel")
            .exact_width(200.0)
            .resizable(false)
            .show(ctx, |ui| {
                let mut state = self.state.lock().unwrap();

                ui.label("Site");
                ui.text_edit_singleline(&mut state.site_string);

                ui.label("Date");
                ui.text_edit_singleline(&mut state.date_string);

                ui.label("Time");
                ui.text_edit_singleline(&mut state.time_string);

                let date_valid = NaiveDate::parse_from_str(&state.date_string, "%m/%d/%Y").is_ok();
                let time_valid = NaiveTime::parse_from_str(&state.time_string, "%H:%M").is_ok();

                if ui.add_enabled(date_valid && time_valid, egui::Button::new("Load")).clicked() {
                    info!("Loading data for site: {}, date: {}, time: {}", state.site_string, state.date_string, state.time_string);
                    
                    spawn_local(async move {
                        info!("Data loaded");
                    })
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("NEXRAD Workbench");

            ui.separator();

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                egui::warn_if_debug_build(ui);
            });
        });
    }
}

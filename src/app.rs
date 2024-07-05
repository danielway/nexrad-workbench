use std::sync::{Arc, Mutex};
use chrono::{NaiveDate, NaiveTime};
use log::info;

#[cfg(target_arch = "wasm32")]
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

                let date = NaiveDate::parse_from_str(&state.date_string, "%m/%d/%Y");
                let time = NaiveTime::parse_from_str(&state.time_string, "%H:%M");

                let input_valid = date.is_ok() && time.is_ok();

                if ui.add_enabled(input_valid, egui::Button::new("Load")).clicked() {
                    info!("Loading data for site: {}, date: {}, time: {}", state.site_string, state.date_string, state.time_string);
                    load_nexrad_data(state.site_string.to_string(), date.unwrap(), time.unwrap());
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

#[cfg(not(target_arch = "wasm32"))]
fn load_nexrad_data(site: String, date: NaiveDate, time: NaiveTime) {
    info!("Data loaded: site: {}, date: {}, time: {}", site, date, time);
}

#[cfg(target_arch = "wasm32")]
fn load_nexrad_data(site: String, date: NaiveDate, time: NaiveTime) {
    spawn_local(async move {
        info!("Data loaded: site: {}, date: {}, time: {}", site, date, time);
    })
}

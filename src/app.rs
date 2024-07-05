use chrono::{NaiveDate, NaiveTime};

pub struct NEXRADWorkbench {
    state: WorkbenchState,
}

struct WorkbenchState {
    site_string: String,
    date_string: String,
    time_string: String,
}

impl Default for NEXRADWorkbench {
    fn default() -> Self {
        Self {
            state: WorkbenchState {
                site_string: String::from("KDMX"),
                date_string: String::from("03/05/2022"),
                time_string: String::from("23:30"),
            }
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
                let state = &mut self.state;
                
                ui.label("Site");
                ui.text_edit_singleline(&mut state.site_string);

                ui.label("Date");
                ui.text_edit_singleline(&mut state.date_string);

                ui.label("Time");
                ui.text_edit_singleline(&mut state.time_string);

                let date_valid = NaiveDate::parse_from_str(&state.date_string, "%m/%d/%Y").is_ok();
                let time_valid = NaiveTime::parse_from_str(&state.time_string, "%H:%M").is_ok();

                ui.add_enabled(date_valid && time_valid, egui::Button::new("Load"));
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

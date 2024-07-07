use std::sync::{Arc, Mutex};
use chrono::{NaiveDate, NaiveTime};
use log::info;

use nexrad::download::{list_files, download_file};
use nexrad::file::FileMetadata;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local;

struct WorkbenchState {
    site_string: String,
    date_string: String,
    time_string: String,
}

impl Default for WorkbenchState {
    fn default() -> Self {
        Self {
            site_string: String::from("KDMX"),
            date_string: String::from("03/05/2022"),
            time_string: String::from("23:30"),
        }
    }
}

pub struct NEXRADWorkbench {
    state: Arc<Mutex<WorkbenchState>>,
    
    #[cfg(not(target_arch = "wasm32"))]
    runtime: tokio::runtime::Runtime,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for NEXRADWorkbench {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkbenchState::default())),
            runtime: tokio::runtime::Runtime::new().expect("runtime is created"),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for NEXRADWorkbench {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkbenchState::default())),
        }
    }
}

impl NEXRADWorkbench {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Default::default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn get_nexrad_file(&self, site: String, date: NaiveDate, time: NaiveTime) {
        self.runtime.spawn(async move {
            info!("Loading data for site: {}, date: {}, time: {}...", site, date, time);

            let files = list_files(&site, &date).await.expect("files are listed");
            info!("Found {} files.", files.len());
            
            let nearest_index = get_nearest_metadata_index(&files, time);
            let nearest_metadata = &files[nearest_index];
            info!("Nearest metadata: {}", nearest_metadata.identifier());
            
            let file_bytes = download_file(nearest_metadata).await.expect("file is downloaded");
            info!("File downloaded: {} bytes", file_bytes.len());
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn get_nexrad_file(&self, site: String, date: NaiveDate, time: NaiveTime) {
        spawn_local(async move {
            info!("Loading data for site: {}, date: {}, time: {}...", site, date, time);

            let files = list_files(&site, &date).await.expect("files are listed");
            info!("Found {} files.", files.len());

            let nearest_index = get_nearest_metadata_index(&files, time);
            let nearest_metadata = &files[nearest_index];
            info!("Nearest metadata: {}", nearest_metadata.identifier());

            let file_bytes = download_file(nearest_metadata).await.expect("file is downloaded");
            info!("File downloaded: {} bytes", file_bytes.len());
        })
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
                    self.get_nexrad_file(state.site_string.to_string(), date.unwrap(), time.unwrap());
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

/// Returns the index of the metadata with the nearest time to the provided start time.
fn get_nearest_metadata_index(metas: &Vec<FileMetadata>, start_time: NaiveTime) -> usize {
    let first_metadata_time = get_metadata_time(metas.first().expect("found at least one meta"));
    let mut min_diff = first_metadata_time
        .signed_duration_since(start_time)
        .num_seconds()
        .abs();
    let mut min_index = 0;

    for (index, metadata) in metas.iter().skip(1).enumerate() {
        let metadata_time = get_metadata_time(metadata);
        let diff = metadata_time
            .signed_duration_since(start_time)
            .num_seconds()
            .abs();

        if diff < min_diff {
            min_diff = diff;
            min_index = index;
        }
    }

    min_index
}

/// Returns the time from the metadata identifier.
fn get_metadata_time(metadata: &FileMetadata) -> NaiveTime {
    let identifier_parts = metadata.identifier().split('_');
    let identifier_time = identifier_parts.collect::<Vec<_>>()[1];
    NaiveTime::parse_from_str(identifier_time, "%H%M%S").expect("is valid time")
}

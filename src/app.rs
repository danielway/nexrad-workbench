use eframe::egui;

#[derive(Default)]
pub struct App;

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("NEXRAD Workbench");

            ui.collapsing("Backend Info", |ui| {
                 ctx.inspection_ui(ui);
            });
        });
    }
}

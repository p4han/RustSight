use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 480.0])
            .with_title("RustSight Demo App"),
        ..Default::default()
    };

    eframe::run_native(
        "RustSight Demo App",
        options,
        Box::new(|_cc| Ok(Box::new(DemoApp::default()))),
    )
}

struct DemoApp {
    counter: i32,
    text: String,
    animate: bool,
    phase: f32,
    last_action: String,
}

impl Default for DemoApp {
    fn default() -> Self {
        Self {
            counter: 0,
            text: String::from("Type here..."),
            animate: true,
            phase: 0.0,
            last_action: String::from("Ready"),
        }
    }
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.animate {
            self.phase += ui.input(|i| i.stable_dt) * 2.5;
            ui.request_repaint();
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("RustSight Demo App");
            ui.label(format!("Status: {}", self.last_action));
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button(format!("Increment ({})", self.counter)).clicked() {
                    self.counter += 1;
                    self.last_action = format!("Clicked Increment -> {}", self.counter);
                }
                if ui.button("Reset").clicked() {
                    self.counter = 0;
                    self.last_action = "Reset counter".to_string();
                }
                if ui
                    .button(if self.animate { "Pause Anim" } else { "Play Anim" })
                    .clicked()
                {
                    self.animate = !self.animate;
                    self.last_action = format!("Animation: {}", self.animate);
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Input:");
                let resp = ui.text_edit_singleline(&mut self.text);
                if resp.changed() {
                    self.last_action = format!("Text: {}", self.text);
                }
            });

            ui.add_space(8.0);
            // Animated indicator bar + bouncing circle
            let (rect, _resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(30, 34, 42));
            let t = (self.phase.sin() + 1.0) * 0.5;
            let cx = rect.left() + 24.0 + t * (rect.width() - 48.0);
            let cy = rect.center().y;
            painter.circle_filled(
                egui::pos2(cx, cy),
                16.0,
                egui::Color32::from_rgb(80, 180, 255),
            );

            ui.add_space(8.0);
            ui.label("Scrollable List:");
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    for i in 1..=50 {
                        if ui.button(format!("List Item #{i:02}")).clicked() {
                            self.last_action = format!("Clicked Item #{i:02}");
                        }
                    }
                });
        });
    }
}

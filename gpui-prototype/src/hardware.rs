use gpui::{
    Bounds, IntoElement, PathBuilder, Pixels, Window, canvas, div, img, point, prelude::*, px, rgb,
};

use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE};
use crate::dashboard::{Dashboard, control};
use crate::data::reading;

impl Dashboard {
    pub fn hardware(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let speeds = [
            self.capture.fan("0", self.inspection.selected),
            self.capture.fan("1", self.inspection.selected),
        ];
        let stage = hardware_stage(
            speeds,
            self.performance.started.elapsed().as_secs_f32(),
            self.animate,
        );
        div()
            .p(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child("MacBook · captured assembly reference")
                    .child(
                        control(if self.animate {
                            "Pause animation"
                        } else {
                            "Animate fans"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.animate = !this.animate;
                            cx.notify();
                        })),
                    ),
            )
            .child(self.hardware_values())
            .child(div().flex().justify_center().child(stage))
            .child(div().text_color(rgb(MUTED)).text_size(px(11.0)).child(
                "Assembly reference; channel sides unverified. Airflow particles are not ported.",
            ))
    }

    fn hardware_values(&self) -> impl IntoElement {
        let selected = self.inspection.selected;
        let mut values = div().flex().flex_wrap().gap(px(12.0));
        for (label, value, tint) in [
            (
                "CPU",
                reading(self.capture.value("cpu0", selected), "%"),
                BLUE,
            ),
            (
                "GPU",
                reading(self.capture.value("gpu0", selected), "%"),
                PURPLE,
            ),
            (
                "Memory",
                reading(
                    self.capture
                        .value("memoryUsed0", selected)
                        .map(|value| value / 1_073_741_824.0),
                    "GiB",
                ),
                GREEN,
            ),
            (
                "Fan 0",
                reading(self.capture.fan("0", selected), "rpm"),
                BLUE,
            ),
            (
                "Fan 1",
                reading(self.capture.fan("1", selected), "rpm"),
                BLUE,
            ),
            (
                "Battery",
                reading(self.capture.value("battery0", selected), "%"),
                ORANGE,
            ),
        ] {
            values = values.child(
                div()
                    .flex_1()
                    .p(px(10.0))
                    .child(label)
                    .child(div().text_size(px(20.0)).text_color(rgb(tint)).child(value)),
            );
        }
        values
    }
}

fn hardware_stage(speeds: [Option<f32>; 2], elapsed: f32, animate: bool) -> impl IntoElement {
    div()
        .relative()
        .w(px(840.0))
        .h(px(600.0))
        .child(
            img(hardware_chassis_path())
                .absolute()
                .left_0()
                .top_0()
                .w(px(840.0))
                .h(px(600.0)),
        )
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    for (index, x) in [183.0, 657.0].into_iter().enumerate() {
                        let rpm = speeds[index].unwrap_or(0.0);
                        let angle = if animate {
                            elapsed * rpm / 60.0 * 0.12
                        } else {
                            0.0
                        };
                        paint_rotor(bounds, x, angle, window);
                    }
                    if animate {
                        window.request_animation_frame();
                    }
                },
            )
            .absolute()
            .size_full(),
        )
}

fn hardware_chassis_path() -> std::path::PathBuf {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(hardware_chassis_path_load).clone()
}

fn hardware_chassis_path_load() -> std::path::PathBuf {
    match std::env::current_exe() {
        Ok(executable) => {
            if let Some(parent) = executable.parent() {
                if parent.file_name().is_some_and(|name| name == "MacOS") {
                    let bundled = parent.join("../Resources/schematic_chassis.png");
                    if bundled.is_file() {
                        return bundled;
                    }
                    eprintln!(
                        "bundled chassis resource unavailable: {}",
                        bundled.display()
                    );
                }
            }
        }
        Err(error) => eprintln!("locating chassis resource executable: {error}"),
    }
    std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/schematic_chassis.png"
    ))
}

fn paint_rotor(bounds: Bounds<Pixels>, x: f32, angle: f32, window: &mut Window) {
    let center = point(bounds.origin.x + px(x), bounds.origin.y + px(221.0));
    for blade in 0..36 {
        let angle = angle + crate::charts::number(blade) * std::f32::consts::TAU / 36.0;
        let mut path = PathBuilder::fill();
        for (index, (radius, offset)) in [(18.0, 0.0), (49.0, 0.18), (46.0, 0.28), (19.0, 0.10)]
            .into_iter()
            .enumerate()
        {
            let position = point(
                center.x + px(radius * (angle + offset).cos()),
                center.y + px(radius * (angle + offset).sin()),
            );
            if index == 0 {
                path.move_to(position);
            } else {
                path.line_to(position);
            }
        }
        path.close();
        match path.build() {
            Ok(path) => window.paint_path(path, rgb(0x0067_6c71)),
            Err(error) => eprintln!("fan blade: {error}"),
        }
    }
}

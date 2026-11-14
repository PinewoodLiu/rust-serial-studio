use eframe::egui::{self, Color32, Rect, Sense, Stroke};

#[derive(Clone, Copy)]
pub enum Axis {
    Horizontal,
    Vertical,
}

pub struct Splitter {
    pub rect: Rect,
    pub axis: Axis,
    pub origin: f32,
    pub reverse: bool,
    pub min: f32,
    pub max: f32,
    pub reset: f32,
}
impl Splitter {
    pub fn show(&self, ui: &mut egui::Ui, id: &'static str, value: &mut f32) {
        let cursor = match self.axis {
            Axis::Horizontal => egui::CursorIcon::ResizeHorizontal,
            Axis::Vertical => egui::CursorIcon::ResizeVertical,
        };
        // Keep the handles above all pane contents so scroll areas and text edits
        // cannot capture their pointer input. Dialogs shown later remain above them.
        let response = egui::Area::new(egui::Id::new(id))
            .order(egui::Order::Middle)
            .fixed_pos(self.rect.min)
            .movable(false)
            .constrain(false)
            .show(ui.ctx(), |handle_ui| {
                let (rect, response) =
                    handle_ui.allocate_exact_size(self.rect.size(), Sense::click_and_drag());
                let color = if response.dragged() {
                    Color32::from_rgb(27, 107, 224)
                } else if response.hovered() {
                    Color32::from_rgb(167, 192, 225)
                } else {
                    Color32::from_rgb(224, 231, 240)
                };
                handle_ui.painter().rect_filled(rect, 0.0, color);
                let center = rect.center();
                let (a, b) = match self.axis {
                    Axis::Horizontal => (
                        center - egui::vec2(0.0, 22.0),
                        center + egui::vec2(0.0, 22.0),
                    ),
                    Axis::Vertical => (
                        center - egui::vec2(22.0, 0.0),
                        center + egui::vec2(22.0, 0.0),
                    ),
                };
                handle_ui.painter().line_segment(
                    [a, b],
                    Stroke::new(2.0_f32, Color32::from_rgb(135, 155, 180)),
                );
                response.on_hover_cursor(cursor)
            })
            .inner;
        // Latch the press in a generous grab strip, independently of child-widget
        // drag ownership. Keep resizing when the pointer leaves the strip.
        let latch_id = egui::Id::new(id).with("pointer_latch");
        let (pressed_at, down, released, pos) = ui.ctx().input(|i| {
            let pressed_at = i.raw.events.iter().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => Some(*pos),
                _ => None,
            });
            (
                pressed_at,
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.latest_pos(),
            )
        });
        let mut start = ui.ctx().data(|data| data.get_temp::<egui::Pos2>(latch_id));
        if let Some(press) = pressed_at
            && self.rect.expand(8.0).contains(press)
        {
            start = Some(press);
        }
        // A fast drag may press, move and release in one input batch. Use the
        // original press event and still apply its final position on release.
        if let (Some(start), Some(pos)) = (start, pos)
            && (down || released)
            && start.distance(pos) > 2.0
        {
            let coordinate = match self.axis {
                Axis::Horizontal => pos.x,
                Axis::Vertical => pos.y,
            };
            *value = (if self.reverse {
                self.origin - coordinate
            } else {
                coordinate - self.origin
            })
            .clamp(self.min, self.max);
            ui.ctx().request_repaint();
            ui.ctx().set_cursor_icon(cursor);
        }
        ui.ctx().data_mut(|data| {
            if down {
                if let Some(start) = start {
                    data.insert_temp(latch_id, start);
                }
            } else {
                data.remove::<egui::Pos2>(latch_id);
            }
        });
        if response.double_clicked() {
            *value = self.reset.clamp(self.min, self.max);
        }
        response.on_hover_text("拖动调整区域大小；双击恢复默认大小");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_drag_press_move_release_in_one_frame_is_not_lost() {
        let ctx = egui::Context::default();
        let mut value = 250.0;
        let splitter = Splitter {
            rect: Rect::from_min_size(egui::pos2(250.0, 0.0), egui::vec2(8.0, 700.0)),
            axis: Axis::Horizontal,
            origin: 0.0,
            reverse: false,
            min: 190.0,
            max: 380.0,
            reset: 250.0,
        };
        let start = egui::pos2(254.0, 440.0);
        let end = egui::pos2(220.0, 440.0);
        for events in [
            vec![],
            vec![],
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerMoved(end),
                egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| splitter.show(ui, "quick_drag", &mut value));
                },
            );
        }
        assert_eq!(value, 220.0);
    }
    #[test]
    fn actual_pointer_drags_resize_all_three_splitters() {
        for (axis, reverse, initial, start, end, origin, expected) in [
            (
                Axis::Horizontal,
                false,
                200.0,
                egui::pos2(204.0, 100.0),
                egui::pos2(260.0, 100.0),
                0.0,
                260.0,
            ),
            (
                Axis::Horizontal,
                true,
                400.0,
                egui::pos2(604.0, 100.0),
                egui::pos2(550.0, 100.0),
                1000.0,
                450.0,
            ),
            (
                Axis::Vertical,
                true,
                250.0,
                egui::pos2(400.0, 554.0),
                egui::pos2(400.0, 500.0),
                800.0,
                300.0,
            ),
        ] {
            let ctx = egui::Context::default();
            let mut value = initial;
            let rect = match axis {
                Axis::Horizontal => {
                    Rect::from_min_size(egui::pos2(start.x - 4.0, 0.0), egui::vec2(8.0, 800.0))
                }
                Axis::Vertical => {
                    Rect::from_min_size(egui::pos2(0.0, start.y - 4.0), egui::vec2(1000.0, 8.0))
                }
            };
            let splitter = Splitter {
                rect,
                axis,
                origin,
                reverse,
                min: 100.0,
                max: 700.0,
                reset: initial,
            };
            let frames = vec![
                vec![],
                vec![],
                vec![egui::Event::PointerMoved(start)],
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                vec![egui::Event::PointerMoved(end)],
                vec![egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ];
            for events in frames {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, 800.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .show(ctx, |ui| splitter.show(ui, "test_splitter", &mut value));
                    },
                );
            }
            assert_eq!(value, expected);
        }
    }
}

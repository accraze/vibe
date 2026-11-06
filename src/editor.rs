//! Pedal-style editor: wet knobs on top, looper knobs below, Bypass and Record footswitches.

use crate::{Shared, VibeParams};
use nih_plug::prelude::*;
use nih_plug_egui::egui::{
    self, emath::TSTransform, Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
};
use nih_plug_egui::{create_egui_editor, resizable_window::ResizableWindow};
use std::f32::consts::{PI, TAU};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub const WIDTH: u32 = 400;
pub const HEIGHT: u32 = 480;
const DESIGN: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(WIDTH as f32, HEIGHT as f32));

const KNOB_RADIUS: f32 = 26.0;
/// Knobs turn +-135 degrees from straight up
const SWEEP: f32 = 0.75 * PI;
const WHITE: Color32 = Color32::from_rgb(236, 234, 228);
const ENCLOSURE: Color32 = Color32::from_rgb(30, 44, 66);

pub fn create(params: Arc<VibeParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    create_egui_editor(
        params.editor_state.clone(),
        (),
        |_, _| {},
        move |ctx, setter, _| {
            // The LED follows the loop state, which changes without any param change
            ctx.request_repaint();
            ResizableWindow::new("resize")
                .min_size(DESIGN.size() / 2.0)
                .show(ctx, &params.editor_state, |ui| {
                    let window = ui.max_rect();
                    ui.painter()
                        .rect_filled(window, 0.0, Color32::from_gray(48));

                    // Draw at the design size on its own layer, then scale that layer (input
                    // included) to fit the window. The window corner stays in the margin so the
                    // resize handle underneath remains visible and grabbable.
                    let scale =
                        (window.width() / DESIGN.width()).min(window.height() / DESIGN.height());
                    let offset = window.center().to_vec2() - DESIGN.center().to_vec2() * scale;
                    let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("pedal"));
                    ctx.set_transform_layer(layer, TSTransform::new(offset, scale));
                    ui.scope_builder(UiBuilder::new().layer_id(layer).max_rect(DESIGN), |ui| {
                        ui.set_clip_rect(DESIGN);
                        pedal(ui, setter, &params, &shared);
                    });
                });
        },
    )
}

fn pedal(ui: &Ui, setter: &ParamSetter, params: &VibeParams, shared: &Shared) {
    let body = ui.max_rect().shrink(12.0);
    let p = ui.painter();
    p.rect_filled(body, 14.0, ENCLOSURE);
    p.rect_stroke(
        body,
        14.0,
        Stroke::new(2.0_f32, Color32::from_gray(90)),
        StrokeKind::Inside,
    );
    for corner in [
        body.left_top(),
        body.right_top(),
        body.left_bottom(),
        body.right_bottom(),
    ] {
        screw(p, corner + (body.center() - corner).normalized() * 22.0);
    }

    let col = |i: f32| body.left() + body.width() * i;
    for (i, param) in [&params.wet, &params.time, &params.clock]
        .into_iter()
        .enumerate()
    {
        knob(
            ui,
            setter,
            param,
            Pos2::new(col((i as f32 + 0.5) / 3.0), body.top() + 80.0),
        );
    }
    let row2 = body.top() + 190.0;
    knob(ui, setter, &params.loop_level, Pos2::new(col(0.25), row2));
    knob(ui, setter, &params.speed, Pos2::new(col(0.75), row2));
    clear_button(ui, shared, Pos2::new(col(0.5), row2));

    let logo = Pos2::new(body.center().x, body.top() + 275.0);
    // The default font has no bold weight, so fake one by overdrawing
    for offset in [
        Vec2::ZERO,
        Vec2::new(1.0, 0.0),
        Vec2::new(0.0, 1.0),
        Vec2::splat(1.0),
    ] {
        p.text(
            logo + offset,
            Align2::CENTER_CENTER,
            "VIBE",
            FontId::proportional(50.0),
            WHITE,
        );
    }
    p.text(
        logo + Vec2::new(0.0, 36.0),
        Align2::CENTER_CENTER,
        "A M B I E N T   L O O P E R",
        FontId::proportional(11.0),
        WHITE,
    );

    let switch_y = body.bottom() - 58.0;
    let led_y = switch_y - 50.0;
    let bypass_led = (!params.bypass.value()).then_some(Color32::from_rgb(255, 50, 35));
    // Red while recording, green while a loop plays, off when empty
    let record_led = if params.record.value() {
        Some(Color32::from_rgb(255, 50, 35))
    } else if shared.has_loop.load(Ordering::Relaxed) {
        Some(Color32::from_rgb(60, 230, 90))
    } else {
        None
    };
    for (x, param, lit) in [
        (col(0.25), &params.bypass, bypass_led),
        (col(0.75), &params.record, record_led),
    ] {
        led(p, Pos2::new(x, led_y), lit);
        p.text(
            Pos2::new(x, led_y - 12.0),
            Align2::CENTER_BOTTOM,
            param.name().to_uppercase(),
            FontId::proportional(10.0),
            Color32::from_gray(170),
        );
        footswitch(ui, setter, param, Pos2::new(x, switch_y));
    }
}

fn led(p: &Painter, pos: Pos2, lit: Option<Color32>) {
    match lit {
        None => {
            p.circle_filled(pos, 4.5, Color32::from_gray(20));
        }
        Some(c) => {
            p.circle_filled(pos, 10.0, c.gamma_multiply(0.16));
            p.circle_filled(pos, 4.5, c);
            p.circle_filled(
                pos + Vec2::new(-1.5, -1.5),
                1.5,
                c.lerp_to_gamma(Color32::WHITE, 0.7),
            );
        }
    }
}

/// Small momentary button that wipes the loop.
fn clear_button(ui: &Ui, shared: &Shared, center: Pos2) {
    let rect = Rect::from_center_size(center, Vec2::new(54.0, 22.0));
    let response = ui.interact(rect, ui.id().with("clear"), Sense::click());
    if response.clicked() {
        shared.clear.store(true, Ordering::Relaxed);
    }
    let p = ui.painter();
    let fill = if response.is_pointer_button_down_on() {
        Color32::from_gray(60)
    } else {
        Color32::from_gray(10)
    };
    p.rect_filled(rect, 6.0, fill);
    p.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0_f32, Color32::from_gray(90)),
        StrokeKind::Outside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "CLEAR",
        FontId::proportional(10.0),
        WHITE,
    );
}

/// Unit vector for a knob angle, measured clockwise from straight up.
fn dir(angle: f32) -> Vec2 {
    Vec2::new(angle.sin(), -angle.cos())
}

fn knob(ui: &Ui, setter: &ParamSetter, param: &FloatParam, center: Pos2) {
    let rect = Rect::from_center_size(center, Vec2::splat(2.0 * KNOB_RADIUS + 12.0));
    let response = ui.interact(rect, ui.id().with(param.name()), Sense::click_and_drag());

    // Accumulate drags in egui memory since the param value can lag behind the host round trip
    if response.drag_started() {
        setter.begin_set_parameter(param);
        ui.data_mut(|d| d.insert_temp(response.id, param.unmodulated_normalized_value()));
    }
    if response.dragged() {
        let speed = if ui.input(|i| i.modifiers.shift) {
            0.0005
        } else {
            0.005
        };
        let delta = response.drag_delta();
        let start = ui
            .data(|d| d.get_temp(response.id))
            .unwrap_or(param.unmodulated_normalized_value());
        let value = (start + (delta.x - delta.y) * speed).clamp(0.0, 1.0);
        ui.data_mut(|d| d.insert_temp(response.id, value));
        setter.set_parameter_normalized(param, value);
    }
    if response.drag_stopped() {
        setter.end_set_parameter(param);
    }
    if response.double_clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, param.default_plain_value());
        setter.end_set_parameter(param);
    }

    if response.hovered() {
        let (notches, shift) = ui.input(|i| {
            let notches: f32 = i
                .events
                .iter()
                .map(|e| match e {
                    // Smooth-scrolling touchpads report points; treat ~50 points as one notch
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta,
                        ..
                    } => delta.y / 50.0,
                    egui::Event::MouseWheel { delta, .. } => delta.y,
                    _ => 0.0,
                })
                .sum();
            (notches, i.modifiers.shift)
        });
        if notches != 0.0 {
            let step = if shift { 0.002 } else { 0.02 };
            let value = (param.unmodulated_normalized_value() + notches * step).clamp(0.0, 1.0);
            setter.begin_set_parameter(param);
            setter.set_parameter_normalized(param, value);
            setter.end_set_parameter(param);
        }
    }

    let value = param.unmodulated_normalized_value();
    let angle = -SWEEP + 2.0 * SWEEP * value;
    let p = ui.painter();

    for i in 0..=10 {
        let a = -SWEEP + 2.0 * SWEEP * i as f32 / 10.0;
        let r = if i % 5 == 0 { 2.0 } else { 1.2 };
        p.circle_filled(center + dir(a) * (KNOB_RADIUS + 9.0), r, WHITE);
    }

    // Knurled skirt that turns with the knob
    p.circle_filled(
        center + Vec2::new(0.0, 3.0),
        KNOB_RADIUS,
        Color32::from_black_alpha(160),
    );
    p.circle_filled(center, KNOB_RADIUS, Color32::from_gray(8));
    for i in 0..40 {
        let a = angle + i as f32 * TAU / 40.0;
        p.line_segment(
            [
                center + dir(a) * (KNOB_RADIUS - 6.0),
                center + dir(a) * (KNOB_RADIUS - 0.5),
            ],
            Stroke::new(1.5_f32, Color32::from_gray(42)),
        );
    }
    p.circle_filled(center, KNOB_RADIUS - 7.0, Color32::from_gray(26));
    p.circle_stroke(
        center,
        KNOB_RADIUS - 7.0,
        Stroke::new(1.0_f32, Color32::from_gray(58)),
    );
    p.line_segment(
        [
            center + dir(angle) * 7.0,
            center + dir(angle) * (KNOB_RADIUS - 9.0),
        ],
        Stroke::new(3.0_f32, WHITE),
    );

    p.text(
        center - Vec2::new(0.0, KNOB_RADIUS + 18.0),
        Align2::CENTER_BOTTOM,
        param.name().to_uppercase(),
        FontId::proportional(13.0),
        WHITE,
    );
    if response.hovered() || response.dragged() {
        p.text(
            center + Vec2::new(0.0, KNOB_RADIUS + 16.0),
            Align2::CENTER_TOP,
            param.normalized_value_to_string(value, true),
            FontId::proportional(12.0),
            Color32::from_gray(170),
        );
    }
}

fn screw(p: &Painter, center: Pos2) {
    p.circle_filled(center, 5.5, Color32::from_gray(90));
    p.circle_stroke(center, 5.5, Stroke::new(1.0_f32, Color32::from_gray(40)));
    p.line_segment(
        [center - dir(0.6) * 4.0, center + dir(0.6) * 4.0],
        Stroke::new(1.5_f32, Color32::from_gray(35)),
    );
}

fn footswitch(ui: &Ui, setter: &ParamSetter, param: &BoolParam, center: Pos2) {
    let rect = Rect::from_center_size(center, Vec2::splat(54.0));
    let response = ui.interact(rect, ui.id().with(param.name()), Sense::click());
    if response.clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, !param.value());
        setter.end_set_parameter(param);
    }

    // Hex nut, then the chrome plunger, which sinks a little while held
    let p = ui.painter();
    let nut: Vec<Pos2> = (0..6)
        .map(|i| center + dir(i as f32 * TAU / 6.0) * 27.0)
        .collect();
    p.add(egui::Shape::convex_polygon(
        nut,
        Color32::from_gray(110),
        Stroke::new(1.0_f32, Color32::from_gray(60)),
    ));
    p.circle_filled(center, 19.0, Color32::from_gray(150));
    p.circle_stroke(center, 19.0, Stroke::new(1.5_f32, Color32::from_gray(80)));
    let cap = if response.is_pointer_button_down_on() {
        11.5
    } else {
        13.0
    };
    p.circle_filled(center, cap, Color32::from_gray(195));
    p.circle_filled(center + Vec2::new(-4.0, -4.0), 4.0, Color32::from_gray(235));
}

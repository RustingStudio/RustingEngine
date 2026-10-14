//! Settings-page building blocks: cards with icon headers, two-column setting
//! rows, switches, segmented controls, stat tiles and navigation items.
//!
//! Colors and spacing come from `EditorTheme`; panels compose these instead of
//! stacking bare labels.

use super::EditorTheme;
use crate::editor::icons::paint_editor_icon;
use crate::editor::EditorIcon;

const CARD_RADIUS: u8 = 6;
const CARD_PAD: i8 = 12;

fn radius(value: u8) -> egui::CornerRadius {
    egui::CornerRadius::same(value)
}

/// Mixes `color` into `base` by `amount` (0 = base, 1 = color).
pub fn tint(
    base: egui::Color32,
    color: egui::Color32,
    amount: f32,
) -> egui::Color32 {
    base.lerp_to_gamma(color, amount)
}

/// Tracks whether a row is the first of its card, so rows get hairlines
/// between them and not above the first one.
pub struct CardRows {
    first: bool,
}

impl CardRows {
    /// One setting: title and hint on the left, `control` on the right.
    pub fn row(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        hint: &str,
        control: impl FnOnce(&mut egui::Ui),
    ) {
        if !self.first {
            let y = ui.cursor().min.y;
            ui.painter().hline(
                ui.max_rect().x_range(),
                y,
                egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT),
            );
        }
        self.first = false;
        setting_row(ui, title, hint, control);
    }
}

/// Rounded card with an icon tile, a title and an optional subtitle.
pub fn card(
    ui: &mut egui::Ui,
    icon: EditorIcon,
    title: &str,
    subtitle: &str,
    body: impl FnOnce(&mut egui::Ui, &mut CardRows),
) {
    egui::Frame::new()
        .fill(EditorTheme::PANEL_RAISED)
        .stroke(egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT))
        .corner_radius(radius(CARD_RADIUS))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: egui::Color32::from_black_alpha(60),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            // Header band.
            let header_height = if subtitle.is_empty() { 38.0 } else { 46.0 };
            let (band, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), header_height),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(
                band,
                egui::CornerRadius {
                    nw: CARD_RADIUS,
                    ne: CARD_RADIUS,
                    sw: 0,
                    se: 0,
                },
                tint(EditorTheme::PANEL_RAISED, egui::Color32::WHITE, 0.03),
            );
            ui.painter().hline(
                band.x_range(),
                band.bottom(),
                egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT),
            );
            let tile = egui::Rect::from_center_size(
                egui::pos2(band.left() + 24.0, band.center().y),
                egui::vec2(26.0, 26.0),
            );
            ui.painter().rect_filled(
                tile,
                radius(5),
                tint(EditorTheme::PANEL_RAISED, EditorTheme::ACCENT, 0.55),
            );
            paint_editor_icon(
                ui.painter(),
                icon,
                tile.shrink(3.0),
                EditorTheme::TEXT_STRONG,
            );
            let text_x = tile.right() + 10.0;
            if subtitle.is_empty() {
                ui.painter().text(
                    egui::pos2(text_x, band.center().y),
                    egui::Align2::LEFT_CENTER,
                    title,
                    egui::FontId::proportional(14.0),
                    EditorTheme::TEXT_STRONG,
                );
            } else {
                ui.painter().text(
                    egui::pos2(text_x, band.center().y - 7.0),
                    egui::Align2::LEFT_CENTER,
                    title,
                    egui::FontId::proportional(14.0),
                    EditorTheme::TEXT_STRONG,
                );
                ui.painter().text(
                    egui::pos2(text_x, band.center().y + 9.0),
                    egui::Align2::LEFT_CENTER,
                    subtitle,
                    egui::FontId::proportional(11.0),
                    EditorTheme::TEXT_MUTED,
                );
            }
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(CARD_PAD, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    body(ui, &mut CardRows { first: true });
                });
        });
}

/// Title and hint left, control right-aligned and vertically centered.
pub fn setting_row(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    const PAD: f32 = 9.0;
    let width = ui.available_width();
    let start = ui.cursor().min + egui::vec2(0.0, PAD);
    let left_width = (width * 0.48).max(110.0).min(width);
    let mut left = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                start,
                egui::vec2(left_width, 400.0),
            ))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    left.spacing_mut().item_spacing.y = 1.0;
    left.label(
        egui::RichText::new(title)
            .size(13.0)
            .color(EditorTheme::TEXT),
    );
    if !hint.is_empty() {
        left.add(
            egui::Label::new(
                egui::RichText::new(hint)
                    .size(11.0)
                    .color(EditorTheme::TEXT_MUTED),
            )
            .wrap(),
        );
    }
    let height = left.min_rect().height().max(EditorTheme::ROW_HEIGHT);
    let right_rect = egui::Rect::from_min_max(
        egui::pos2(start.x + left_width + 12.0, start.y),
        egui::pos2(start.x + width, start.y + height),
    );
    let mut right = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    control(&mut right);
    ui.allocate_rect(
        egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(width, height + PAD * 2.0),
        ),
        egui::Sense::hover(),
    );
}

/// Animated on/off switch.
pub fn toggle(ui: &mut egui::Ui, value: &mut bool) -> egui::Response {
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(36.0, 20.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(response.id, *value, 0.12);
    let track = if *value {
        EditorTheme::ACCENT
    } else {
        EditorTheme::INPUT
    };
    let track = if response.hovered() {
        tint(track, egui::Color32::WHITE, 0.08)
    } else {
        track
    };
    ui.painter().rect(
        rect,
        radius(10),
        track,
        egui::Stroke::new(
            1.0_f32,
            if *value {
                EditorTheme::ACCENT_HOVER
            } else {
                EditorTheme::BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    let knob_x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter().circle_filled(
        egui::pos2(knob_x, rect.center().y),
        7.0,
        if *value {
            EditorTheme::TEXT_STRONG
        } else {
            EditorTheme::TEXT_MUTED
        },
    );
    response
}

/// Segmented button group; falls back to a drop-down when the options do
/// not fit the space left in the row.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let base = egui::Id::new(&id);
    let font = egui::FontId::proportional(12.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap(
                    (*label).to_owned(),
                    font.clone(),
                    EditorTheme::TEXT,
                )
                .size()
                .x
                + 18.0
        })
        .collect();
    let total: f32 = widths.iter().sum();
    let mut changed = false;
    if total > ui.available_width() {
        let current = options
            .iter()
            .find(|(option, _)| option == value)
            .map_or("", |(_, label)| *label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (option, label) in options {
                    if ui.selectable_label(value == option, *label).clicked() {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        return changed;
    }
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(total + 4.0, 24.0),
        egui::Sense::hover(),
    );
    ui.painter().rect(
        rect,
        radius(5),
        EditorTheme::INPUT,
        egui::Stroke::new(1.0_f32, EditorTheme::BORDER),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 2.0;
    for ((option, label), width) in options.iter().zip(widths) {
        let segment = egui::Rect::from_min_size(
            egui::pos2(x, rect.top() + 2.0),
            egui::vec2(width, 20.0),
        );
        x += width;
        let response = ui
            .interact(segment, base.with(*label), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let selected = *value == *option;
        if selected {
            ui.painter()
                .rect_filled(segment, radius(4), EditorTheme::ACCENT);
        } else if response.hovered() {
            ui.painter().rect_filled(
                segment,
                radius(4),
                EditorTheme::BUTTON_HOVER,
            );
        }
        ui.painter().text(
            segment.center(),
            egui::Align2::CENTER_CENTER,
            label,
            font.clone(),
            if selected {
                EditorTheme::TEXT_STRONG
            } else {
                EditorTheme::TEXT_MUTED
            },
        );
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// Small rounded status label.
pub fn pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(11.0),
        color,
    );
    let (rect, _) = ui.allocate_exact_size(
        galley.size() + egui::vec2(14.0, 6.0),
        egui::Sense::hover(),
    );
    ui.painter().rect(
        rect,
        radius(9),
        tint(EditorTheme::PANEL_RAISED, color, 0.16),
        egui::Stroke::new(
            1.0_f32,
            tint(EditorTheme::PANEL_RAISED, color, 0.45),
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Filled call-to-action button with an icon.
pub fn action_button(
    ui: &mut egui::Ui,
    icon: EditorIcon,
    text: &str,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        font,
        EditorTheme::TEXT_STRONG,
    );
    let size = egui::vec2(galley.size().x + 46.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let (fill, stroke) = if !enabled {
        (EditorTheme::BUTTON, EditorTheme::BORDER_SOFT)
    } else if primary {
        let fill = if response.is_pointer_button_down_on() {
            EditorTheme::ACCENT_ACTIVE
        } else if response.hovered() {
            EditorTheme::ACCENT_HOVER
        } else {
            EditorTheme::ACCENT
        };
        (fill, EditorTheme::ACCENT_HOVER)
    } else {
        (
            if response.hovered() {
                EditorTheme::BUTTON_HOVER
            } else {
                EditorTheme::BUTTON
            },
            EditorTheme::BORDER,
        )
    };
    ui.painter().rect(
        rect,
        radius(5),
        fill,
        egui::Stroke::new(1.0_f32, stroke),
        egui::StrokeKind::Inside,
    );
    let color = if enabled {
        EditorTheme::TEXT_STRONG
    } else {
        EditorTheme::TEXT_MUTED
    };
    paint_editor_icon(
        ui.painter(),
        icon,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        ),
        color,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
}

/// Metric tile: small caption, large value, optional detail and meter.
pub fn stat_tile(
    ui: &mut egui::Ui,
    width: f32,
    caption: &str,
    value: &str,
    detail: &str,
    meter: Option<f32>,
    color: egui::Color32,
) {
    let height = 64.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().rect(
        rect,
        radius(5),
        EditorTheme::PANEL_RAISED,
        egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT),
        egui::StrokeKind::Inside,
    );
    // Accent edge.
    ui.painter().rect_filled(
        egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, 6.0),
            egui::vec2(3.0, height - 12.0),
        ),
        radius(2),
        color,
    );
    let left = rect.left() + 12.0;
    ui.painter().text(
        egui::pos2(left, rect.top() + 11.0),
        egui::Align2::LEFT_CENTER,
        caption.to_uppercase(),
        egui::FontId::proportional(10.0),
        EditorTheme::TEXT_MUTED,
    );
    ui.painter().text(
        egui::pos2(left, rect.top() + 31.0),
        egui::Align2::LEFT_CENTER,
        value,
        egui::FontId::proportional(20.0),
        EditorTheme::TEXT_STRONG,
    );
    if !detail.is_empty() {
        ui.painter().text(
            egui::pos2(left, rect.bottom() - 9.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::proportional(10.0),
            EditorTheme::TEXT_MUTED,
        );
    }
    if let Some(fraction) = meter {
        let track = egui::Rect::from_min_size(
            egui::pos2(left, rect.bottom() - 8.0),
            egui::vec2(rect.width() - 24.0, 3.0),
        );
        ui.painter()
            .rect_filled(track, radius(2), EditorTheme::INPUT);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                track.min,
                egui::vec2(track.width() * fraction.clamp(0.0, 1.0), 3.0),
            ),
            radius(2),
            color,
        );
    }
}

/// Lays `count` tiles of equal width in as many columns as fit, and calls
/// `tile(ui, index, width)` for each.
pub fn tile_grid(
    ui: &mut egui::Ui,
    count: usize,
    min_width: f32,
    mut tile: impl FnMut(&mut egui::Ui, usize, f32),
) {
    let gap = 8.0;
    let available = ui.available_width();
    let columns = (((available + gap) / (min_width + gap)) as usize)
        .clamp(1, count.max(1));
    let width = (available - gap * (columns - 1) as f32) / columns as f32;
    let mut index = 0;
    while index < count {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for _ in 0..columns {
                if index < count {
                    tile(ui, index, width);
                    index += 1;
                }
            }
        });
        ui.add_space(gap - 4.0);
    }
}

/// Navigation entry of a settings page: icon and label, accent bar when
/// selected. Pass `compact` to draw it as a horizontal chip.
pub fn nav_item(
    ui: &mut egui::Ui,
    icon: EditorIcon,
    label: &str,
    selected: bool,
    compact: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), font, EditorTheme::TEXT);
    let size = if compact {
        egui::vec2(galley.size().x + 40.0, 28.0)
    } else {
        egui::vec2(ui.available_width(), 32.0)
    };
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if selected {
        ui.painter().rect_filled(
            rect,
            radius(5),
            tint(EditorTheme::PANEL, EditorTheme::ACCENT, 0.32),
        );
        if !compact {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    rect.min + egui::vec2(0.0, 6.0),
                    egui::vec2(3.0, rect.height() - 12.0),
                ),
                radius(2),
                EditorTheme::ACCENT_HOVER,
            );
        }
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, radius(5), EditorTheme::PANEL_RAISED);
    }
    let color = if selected {
        EditorTheme::TEXT_STRONG
    } else {
        EditorTheme::TEXT_MUTED
    };
    paint_editor_icon(
        ui.painter(),
        icon,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::vec2(15.0, 15.0),
        ),
        color,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Full-width call-to-action with a centered icon and label.
pub fn wide_button(
    ui: &mut egui::Ui,
    icon: EditorIcon,
    text: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 30.0),
        egui::Sense::click(),
    );
    let fill = if response.is_pointer_button_down_on() {
        tint(EditorTheme::PANEL, EditorTheme::ACCENT, 0.5)
    } else if response.hovered() {
        tint(EditorTheme::PANEL, EditorTheme::ACCENT, 0.4)
    } else {
        tint(EditorTheme::PANEL, EditorTheme::ACCENT, 0.22)
    };
    ui.painter().rect(
        rect,
        radius(5),
        fill,
        egui::Stroke::new(1.0_f32, EditorTheme::ACCENT),
        egui::StrokeKind::Inside,
    );
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        EditorTheme::TEXT_STRONG,
    );
    let start = rect.center().x - (galley.size().x + 22.0) / 2.0;
    paint_editor_icon(
        ui.painter(),
        icon,
        egui::Rect::from_center_size(
            egui::pos2(start + 8.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        ),
        EditorTheme::TEXT_STRONG,
    );
    ui.painter().galley(
        egui::pos2(start + 22.0, rect.center().y - galley.size().y / 2.0),
        galley,
        EditorTheme::TEXT_STRONG,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A raised strip for a panel's tools: buttons and a search field in one row.
pub fn toolbar<R>(
    ui: &mut egui::Ui,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(EditorTheme::PANEL_RAISED)
        .stroke(egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT))
        .corner_radius(radius(EditorTheme::RADIUS))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| ui.horizontal(add).inner)
        .inner
}

/// A search box that fills the rest of the row and clears itself with a
/// small cross while it holds text.
pub fn search_field(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
) -> egui::Response {
    let filled = !text.is_empty();
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .margin(egui::Margin {
                left: 6,
                right: if filled { 22 } else { 6 },
                top: 3,
                bottom: 3,
            })
            .desired_width(f32::INFINITY),
    );
    if filled {
        let rect = egui::Rect::from_center_size(
            egui::pos2(response.rect.right() - 12.0, response.rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        let clear = ui
            .interact(rect, response.id.with("clear"), egui::Sense::click())
            .on_hover_text("Clear");
        paint_editor_icon(
            ui.painter(),
            EditorIcon::Close,
            rect,
            if clear.hovered() {
                EditorTheme::TEXT
            } else {
                EditorTheme::TEXT_MUTED
            },
        );
        if clear.clicked() {
            text.clear();
        }
    }
    response
}

/// A dark, bordered well for lists of rows. It takes the height that is
/// left, less `reserve` for a footer below it.
pub fn list_well<R>(
    ui: &mut egui::Ui,
    reserve: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let height = (ui.available_height() - reserve).max(40.0);
    egui::Frame::new()
        .fill(EditorTheme::BACKGROUND)
        .stroke(egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT))
        .corner_radius(radius(EditorTheme::RADIUS))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.set_height(height - 6.0);
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A small muted status line under a list.
pub fn footer(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(text)
            .small()
            .color(EditorTheme::TEXT_MUTED),
    );
}

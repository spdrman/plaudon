//! The waveform view.
//!
//! One `egui::Mesh` of one-point-wide coloured rectangles, folded from the
//! peak pyramid at whatever resolution the current zoom needs. The painting
//! technique is filmcraft's (see `NOTICE`); the pyramid query behind it is
//! not, and it is the reason this stays cheap: the number of buckets touched
//! is a function of the width of the view, never of the length of the
//! recording.
//!
//! Everything the widget decides is decided in `plaudon-audio-playback`:
//! which time a pixel is, which level answers a query, where the playhead
//! sits. This file allocates a rect, turns pointer and key events into
//! [`PlayerCommand`]s, and paints.

use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Mesh, Modifiers, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo,
};
use plaudon_audio_playback::peaks::{PeakBucket, PeakPyramid};
use plaudon_audio_playback::regions::SpeakerRegions;
use plaudon_audio_playback::ruler::format_clock;
use plaudon_audio_playback::sync::PlayerCommand;
use plaudon_audio_playback::time::{Micros, TimeSpan};
use plaudon_audio_playback::timeline::{Ratio, Timeline};

use super::style::PlayerStyle;

/// Arrow-key seek step, matching the React scrubber.
pub const ARROW_STEP: Micros = Micros::new(5_000_000);
/// Page-key seek step, matching the React scrubber.
pub const PAGE_STEP: Micros = Micros::new(30_000_000);
/// One wheel notch of horizontal pan, in points.
const WHEEL_PAN_POINTS: f32 = 48.0;

/// What one frame of the waveform produced.
#[derive(Debug)]
pub struct WaveformOutcome {
    /// The allocated response, for callers that want to add a context menu.
    pub response: Response,
    /// Transport commands the interaction asks for, in order.
    pub commands: Vec<PlayerCommand>,
    /// Time under the pointer, when it is over the waveform.
    pub hovered_time: Option<Micros>,
    /// Whether the user is dragging the playhead right now.
    pub scrubbing: bool,
}

/// A scrollable, zoomable waveform with a playhead.
#[derive(Debug)]
pub struct WaveformView<'a> {
    timeline: &'a mut Timeline,
    peaks: Option<&'a PeakPyramid>,
    regions: Option<&'a SpeakerRegions>,
    position: Micros,
    style: &'a PlayerStyle,
    height: Option<f32>,
    interactive: bool,
}

impl<'a> WaveformView<'a> {
    /// A view over `timeline`, with the playhead at `position`.
    #[must_use]
    pub fn new(timeline: &'a mut Timeline, position: Micros, style: &'a PlayerStyle) -> Self {
        Self {
            timeline,
            peaks: None,
            regions: None,
            position,
            style,
            height: None,
            interactive: true,
        }
    }

    /// Supply the peak pyramid. Without one the view paints an empty track,
    /// which is what it shows while the worker is still decoding.
    #[must_use]
    pub fn peaks(mut self, peaks: Option<&'a PeakPyramid>) -> Self {
        self.peaks = peaks;
        self
    }

    /// Supply speaker regions to draw under the waveform.
    #[must_use]
    pub fn regions(mut self, regions: Option<&'a SpeakerRegions>) -> Self {
        self.regions = regions;
        self
    }

    /// Override the height.
    #[must_use]
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Turn interaction off, for a read-only preview.
    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Lay out, handle input and paint.
    pub fn show(self, ui: &mut Ui) -> WaveformOutcome {
        let Self {
            timeline,
            peaks,
            regions,
            position,
            style,
            height,
            interactive,
        } = self;

        let band = if regions.is_some() {
            style.speaker_band_height
        } else {
            0.0
        };
        let height = height.unwrap_or(style.waveform_height) + band;
        let sense = if interactive {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
        timeline.set_viewport_points(rect.width());
        if interactive {
            // The arrows seek; without this egui treats them as focus
            // navigation and the second keystroke lands on another widget.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }

        let mut commands = Vec::new();
        let mut scrubbing = false;
        let hovered_time = response
            .hover_pos()
            .map(|pos| timeline.clamp_time(timeline.time_at_point(pos.x - rect.left())));

        if interactive {
            scrubbing = handle_pointer(timeline, &response, rect, &mut commands);
            handle_scroll_and_zoom(ui, timeline, &response, rect);
            handle_keys(ui, timeline, position, &response, &mut commands);
        }

        let waveform_rect = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - band));
        paint(
            ui,
            timeline,
            peaks,
            regions,
            position,
            style,
            rect,
            waveform_rect,
            &response,
            hovered_time,
        );

        let duration = timeline.duration();
        let fraction = if duration.get() > 0 {
            position.get() as f64 / duration.get() as f64
        } else {
            0.0
        };
        // The React scrubber was `role="slider"` with an `aria-valuetext` of
        // "position of duration"; AccessKit gets the same thing here.
        response.widget_info(|| {
            WidgetInfo::slider(
                interactive,
                fraction,
                format!("{} of {}", format_clock(position), format_clock(duration)),
            )
        });

        WaveformOutcome {
            response,
            commands,
            hovered_time,
            scrubbing,
        }
    }
}

/// Click and drag seek. Returns whether a drag is in progress.
fn handle_pointer(
    timeline: &Timeline,
    response: &Response,
    rect: Rect,
    commands: &mut Vec<PlayerCommand>,
) -> bool {
    let seek_to = |pos: egui::Pos2| {
        let time = timeline.time_at_point(pos.x - rect.left());
        PlayerCommand::Seek(timeline.clamp_time(time))
    };
    if response.clicked() {
        response.request_focus();
        if let Some(pos) = response.interact_pointer_pos() {
            commands.push(seek_to(pos));
        }
        return false;
    }
    if response.dragged() || response.drag_started() {
        response.request_focus();
        if let Some(pos) = response.interact_pointer_pos() {
            commands.push(seek_to(pos));
        }
        return true;
    }
    false
}

/// Wheel pans; wheel with the zoom modifier zooms about the pointer.
fn handle_scroll_and_zoom(ui: &Ui, timeline: &mut Timeline, response: &Response, rect: Rect) {
    if !response.hovered() {
        return;
    }
    let (scroll, zoom, pointer) = ui.input(|input| {
        (
            input.smooth_scroll_delta,
            input.zoom_delta(),
            input.pointer.hover_pos(),
        )
    });
    let anchor = pointer.map_or(rect.width() / 2.0, |pos| pos.x - rect.left());
    if (zoom - 1.0).abs() > 1e-4 {
        // A pinch or ctrl+wheel: zooming in means less time per point.
        timeline.zoom_about(Ratio::from_f32(1.0 / zoom), anchor);
        return;
    }
    if scroll.x.abs() > 0.0 {
        timeline.scroll_by_points(-scroll.x);
    } else if scroll.y.abs() > 0.0 {
        // A plain vertical wheel pans: there is nothing to scroll vertically
        // in a waveform, and panning is what the gesture means here.
        timeline.scroll_by_points(-scroll.y.signum() * WHEEL_PAN_POINTS);
    }
}

/// Keyboard, live only while the waveform has focus.
fn handle_keys(
    ui: &Ui,
    timeline: &mut Timeline,
    position: Micros,
    response: &Response,
    commands: &mut Vec<PlayerCommand>,
) {
    if !response.has_focus() {
        return;
    }
    let keys: Vec<(Key, Modifiers)> = ui.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
            .collect()
    });
    for (key, modifiers) in keys {
        match key {
            Key::ArrowLeft | Key::ArrowDown => commands.push(PlayerCommand::Skip(-ARROW_STEP)),
            Key::ArrowRight | Key::ArrowUp => commands.push(PlayerCommand::Skip(ARROW_STEP)),
            Key::PageUp => commands.push(PlayerCommand::Skip(-PAGE_STEP)),
            Key::PageDown => commands.push(PlayerCommand::Skip(PAGE_STEP)),
            Key::Home => commands.push(PlayerCommand::Seek(Micros::ZERO)),
            Key::End => commands.push(PlayerCommand::Seek(timeline.duration())),
            Key::Plus | Key::Equals => {
                timeline.zoom_about(Ratio::ZOOM_IN_STEP, playhead_anchor(timeline, position));
            }
            Key::Minus => {
                timeline.zoom_about(Ratio::ZOOM_OUT_STEP, playhead_anchor(timeline, position));
            }
            Key::Num0 if modifiers.is_none() => timeline.zoom_to_fit(),
            _ => {}
        }
    }
}

/// Zoom around the playhead when it is on screen, else the view centre.
fn playhead_anchor(timeline: &Timeline, position: Micros) -> f32 {
    let point = timeline.point_of_time(position);
    let width = timeline.viewport_points() as f32;
    if (0.0..=width).contains(&point) {
        point
    } else {
        width / 2.0
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one painting pass over everything the view shows; splitting it \
              would only move the argument list"
)]
fn paint(
    ui: &Ui,
    timeline: &Timeline,
    peaks: Option<&PeakPyramid>,
    regions: Option<&SpeakerRegions>,
    position: Micros,
    style: &PlayerStyle,
    rect: Rect,
    waveform_rect: Rect,
    response: &Response,
    hovered_time: Option<Micros>,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, style.background);

    let visible = timeline.visible();
    let playhead_x = rect.left() + timeline.point_of_time(position);

    if let Some(peaks) = peaks {
        painter.add(waveform_mesh(
            peaks,
            visible,
            waveform_rect,
            playhead_x,
            style,
        ));
    }

    painter.hline(
        rect.left()..=rect.right(),
        waveform_rect.center().y,
        Stroke::new(1.0, style.center_line),
    );

    if let Some(regions) = regions {
        paint_speaker_band(&painter, regions, timeline, rect, style);
    }

    if let Some(time) = hovered_time {
        let x = rect.left() + timeline.point_of_time(time);
        painter.vline(
            x,
            rect.top()..=rect.bottom(),
            Stroke::new(1.0, style.cursor),
        );
        painter.text(
            pos2(x + 4.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format_clock(time),
            FontId::monospace(10.0),
            style.cursor,
        );
    }

    if visible.contains(position) || position == visible.end() {
        painter.vline(
            playhead_x,
            rect.top()..=rect.bottom(),
            Stroke::new(2.0, style.playhead),
        );
    }

    if response.has_focus() {
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            ui.visuals().selection.stroke,
            StrokeKind::Inside,
        );
    }
}

/// One mesh for the whole waveform: a coloured rectangle per point column,
/// played and unplayed in different colours.
fn waveform_mesh(
    peaks: &PeakPyramid,
    visible: TimeSpan,
    rect: Rect,
    playhead_x: f32,
    style: &PlayerStyle,
) -> Mesh {
    let mut mesh = Mesh::default();
    let columns = rect.width().round().max(1.0) as u32;
    let buckets = peaks.columns(visible, columns);
    let half = rect.height() * 0.45;
    let middle = rect.center().y;
    for (index, bucket) in buckets.iter().enumerate() {
        let x = rect.left() + index as f32;
        let column = column_rect(*bucket, x, middle, half);
        let Some(column) = column else { continue };
        let color = if x < playhead_x {
            style.played
        } else {
            style.unplayed
        };
        mesh.add_colored_rect(column, color);
    }
    mesh
}

/// The rectangle for one column, or `None` when the column carries no
/// audible swing — the centre line already draws silence, and a quad per
/// silent column is pure cost.
fn column_rect(bucket: PeakBucket, x: f32, middle: f32, half: f32) -> Option<Rect> {
    let low = bucket.min_f32().clamp(-1.0, 1.0);
    let high = bucket.max_f32().clamp(-1.0, 1.0);
    if high - low <= 0.002 {
        return None;
    }
    let top = middle - high * half;
    let bottom = (middle - low * half + 1.0).max(top + 1.0);
    Some(Rect::from_min_max(pos2(x, top), pos2(x + 1.0, bottom)))
}

fn paint_speaker_band(
    painter: &egui::Painter,
    regions: &SpeakerRegions,
    timeline: &Timeline,
    rect: Rect,
    style: &PlayerStyle,
) {
    let band_top = rect.bottom() - style.speaker_band_height;
    for region in regions.overlapping(timeline.visible()) {
        let from = rect.left() + timeline.point_of_time(region.span.start());
        let to = rect.left() + timeline.point_of_time(region.span.end());
        let band = Rect::from_min_max(
            pos2(from.max(rect.left()), band_top),
            pos2(to.min(rect.right()), rect.bottom()),
        );
        if band.width() <= 0.0 {
            continue;
        }
        let color = style.speaker(regions.color_index(region.speaker_id));
        painter.rect_filled(band, CornerRadius::ZERO, color);
    }
    painter.hline(
        rect.left()..=rect.right(),
        band_top,
        Stroke::new(1.0, Color32::from_black_alpha(40)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_column_draws_nothing() {
        assert!(column_rect(PeakBucket::SILENT, 0.0, 50.0, 40.0).is_none());
    }

    #[test]
    fn a_loud_column_spans_the_track() {
        let bucket = PeakBucket {
            min: -32_767,
            max: 32_767,
        };
        let rect = column_rect(bucket, 10.0, 50.0, 40.0).expect("a rect");
        assert!((rect.left() - 10.0).abs() < 1e-6);
        assert!((rect.width() - 1.0).abs() < 1e-6);
        assert!(rect.height() >= 80.0);
    }

    #[test]
    fn the_mesh_has_one_quad_per_audible_column() {
        use plaudon_audio_playback::peaks::{PeakBuilder, PeakParams};

        let mut builder = PeakBuilder::new(PeakParams::default());
        let samples: Vec<f32> = (0..16_000)
            .map(|index| if index % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        builder.push_samples(&samples);
        let pyramid = builder.finish();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        let mesh = waveform_mesh(
            &pyramid,
            TimeSpan::new(Micros::ZERO, pyramid.duration()),
            rect,
            100.0,
            &PlayerStyle::default(),
        );
        // Four vertices and six indices per quad.
        assert_eq!(mesh.vertices.len(), 200 * 4);
        assert_eq!(mesh.indices.len(), 200 * 6);
    }

    #[test]
    fn silence_produces_an_empty_mesh() {
        use plaudon_audio_playback::peaks::{PeakBuilder, PeakParams};

        let mut builder = PeakBuilder::new(PeakParams::default());
        builder.push_samples(&vec![0.0f32; 16_000]);
        let pyramid = builder.finish();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        let mesh = waveform_mesh(
            &pyramid,
            TimeSpan::new(Micros::ZERO, pyramid.duration()),
            rect,
            0.0,
            &PlayerStyle::default(),
        );
        assert!(mesh.is_empty());
    }
}

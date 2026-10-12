//! The export menu (§4.6, J1.3): transcript in five formats and the original
//! audio.
//!
//! All five transcript formats are reachable — the React menu mounted only
//! three — and SRT and VTT are **disabled** when the revision's alignment
//! origin is `none`, with the reason stated, because captions need real
//! segment times and none are invented (§8.6A).
//!
//! The panel cannot open a file dialog: that is the host's. It emits
//! [`AiIntent::PickTranscriptExportPath`] or
//! [`AiIntent::PickMediaExportDirectory`], and the host turns the chosen path
//! into `export_recording` or `export_media`.

use egui::Ui;
use plaudon_app::dto::{ExportFormat, ExportMediaResponse};
use plaudon_app::tone::Tone;

use super::{
    fact, format_available, AiIntent, AiOutput, Badge, RecordingContext, NO_TIMESTAMPS_REASON,
};

/// The five transcript formats, with the label and the extension each uses.
pub const TRANSCRIPT_FORMATS: &[(ExportFormat, &str, &str)] = &[
    (ExportFormat::Txt, "Plain text", "txt"),
    (ExportFormat::Markdown, "Markdown", "md"),
    (ExportFormat::Json, "Structured JSON", "json"),
    (ExportFormat::Srt, "SubRip captions", "srt"),
    (ExportFormat::Vtt, "WebVTT captions", "vtt"),
];

/// Exporting a transcript or the original audio.
#[derive(Debug, Default)]
pub struct ExportMenu {
    include_sanitized_derivative: bool,
}

impl ExportMenu {
    /// Draw the menu.
    ///
    /// `media_result` is the last `export_media` response, when one has
    /// arrived: it carries the digests and the removed metadata, which the
    /// user is shown rather than told about.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        media_result: Option<&ExportMediaResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Export").heading());

        ui.label(egui::RichText::new("Transcript formats").strong());
        let Some(revision_id) = context.revision_id else {
            ui.label("This recording has no transcript revision to export yet.");
            return self.media(ui, context, media_result, output);
        };

        for (format, label, extension) in TRANSCRIPT_FORMATS {
            let available = format_available(*format, context.alignment_origin);
            let text = if available {
                (*label).to_owned()
            } else {
                format!("{label} (unavailable)")
            };
            let response = ui.add_enabled(available, egui::Button::new(text));
            if !available {
                ui.label(NO_TIMESTAMPS_REASON);
            }
            if response.clicked() {
                output.push(AiIntent::PickTranscriptExportPath {
                    recording_id: context.recording_id.to_owned(),
                    revision_id: revision_id.to_owned(),
                    format: *format,
                    suggested_file_name: format!("{}.{extension}", context.title),
                });
            }
        }

        self.media(ui, context, media_result, output)
    }

    fn media(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        media_result: Option<&ExportMediaResponse>,
        mut output: AiOutput,
    ) -> AiOutput {
        ui.separator();
        ui.label(egui::RichText::new("Original audio").strong());
        ui.checkbox(
            &mut self.include_sanitized_derivative,
            "Also produce a sanitized derivative (removes metadata that could identify the \
             source device or editing software)",
        );
        if ui.button("Export audio file").clicked() {
            output.push(AiIntent::PickMediaExportDirectory {
                recording_id: context.recording_id.to_owned(),
                include_sanitized_derivative: self.include_sanitized_derivative,
            });
        }

        if let Some(result) = media_result {
            exported_file(ui, "Original", &result.original);
            if let Some(sanitized) = result.sanitized.as_ref() {
                exported_file(ui, "Sanitized derivative", sanitized);
            }
            if let Some(refusal) = result.sanitized_refusal.as_deref() {
                Badge::new("!", "No sanitized derivative was produced", Tone::Warning)
                    .description(refusal)
                    .ui(ui);
            }
        }
        output
    }
}

fn exported_file(ui: &mut Ui, heading: &str, file: &plaudon_app::dto::ExportedMediaFile) {
    ui.label(egui::RichText::new(format!("{heading}: {}", file.label)).strong());
    fact(ui, "File", &file.file_name);
    fact(ui, "Bytes", &file.byte_length.to_string());
    fact(ui, "SHA-256", &file.sha256.clone());
    if file.stripped_metadata.is_empty() {
        fact(ui, "Removed metadata", "None");
    } else {
        fact(ui, "Removed metadata", &file.stripped_metadata.join(", "));
    }
}

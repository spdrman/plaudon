import { EmptyState } from "../../components/EmptyState";

/**
 * Templates is a required top-level destination (§5.1). Template authoring and
 * the default meeting templates are delivered by the summarization EPIC
 * (§9.3); this build states that plainly rather than showing an inert editor.
 */
export function TemplatesScreen() {
  return (
    <EmptyState
      title="Templates are not part of this build"
      body="Summary and extraction templates are configured once a text model is available. Transcription and the library work without them."
    />
  );
}

/**
 * Media access for playback.
 *
 * JavaScript never sees or handles a filesystem path (§15.1). The Rust side
 * registers a custom URI scheme that resolves a recording id to the immutable
 * archived blob and serves it read-only with HTTP range support; the asset
 * protocol stays disabled.
 */
export const MEDIA_SCHEME = "plaudon-media";

let overrideResolver: ((recordingId: string) => string) | null = null;

/** Test/e2e harness hook: serve fixtures without the Rust process. */
export function setMediaUrlResolver(resolver: ((recordingId: string) => string) | null): void {
  overrideResolver = resolver;
}

export function mediaUrl(recordingId: string): string {
  if (overrideResolver) return overrideResolver(recordingId);
  const id = encodeURIComponent(recordingId);
  // Windows webviews require the `http://<scheme>.localhost/` form.
  const isWindows =
    typeof navigator !== "undefined" && /win/i.test(navigator.userAgent ?? "");
  return isWindows
    ? `http://${MEDIA_SCHEME}.localhost/${id}`
    : `${MEDIA_SCHEME}://localhost/${id}`;
}

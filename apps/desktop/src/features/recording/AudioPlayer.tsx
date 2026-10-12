/**
 * Recording playback (§4.5, §5.2, §5.3).
 *
 * The media never travels through JavaScript as a path: the element is fed by
 * `mediaUrl()` (§15.1). Transport math lives in `playerState.ts`; this module
 * owns the DOM, the waveform decoding budget and the announcements.
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";
import { Button } from "../../components/Button";
import { SelectField } from "../../components/Field";
import { mediaUrl } from "../../ipc/media";
import {
  ARROW_SEEK_MS,
  PAGE_SEEK_MS,
  PEAK_CHUNK_BUCKETS,
  PLAYBACK_SPEEDS,
  SKIP_MS,
  clampPosition,
  computePeakRange,
  createPlayerState,
  formatClock,
  formatPositionLabel,
  peakBucketCount,
  playerReducer,
  type PlaybackSpeed,
  type PlayerState,
} from "./playerState";

export interface PlayerController {
  state: PlayerState;
  audioRef: RefObject<HTMLAudioElement | null>;
  /** Latest polite announcement; rendered by the player's live region. */
  announcement: string;
  setPlaying: (playing: boolean) => void;
  toggle: () => void;
  /** Absolute seek in milliseconds; clamped and pushed to the media element. */
  seekTo: (positionMs: number) => void;
  /** Relative seek, e.g. ±10 s. */
  skip: (deltaMs: number) => void;
  setSpeed: (speed: PlaybackSpeed) => void;
  stepSpeedBy: (direction: 1 | -1) => void;
  /** Playback tick from the media element; silent, never announced. */
  reportTime: (seconds: number) => void;
  /** Media metadata: it may only *downgrade* a length to unknown (§8.6A). */
  reportMediaDuration: (seconds: number) => void;
  reportEnded: () => void;
}

/**
 * Owns playback state for one recording. The detail screen holds the
 * controller so the transcript can seek without reaching into the player.
 */
export function usePlayerController(durationMs: number | null): PlayerController {
  const [state, dispatch] = useReducer(playerReducer, durationMs, createPlayerState);
  const [announcement, setAnnouncement] = useState("");
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    dispatch({ type: "set-duration", durationMs });
  }, [durationMs]);

  const setPlaying = useCallback((playing: boolean) => {
    const audio = audioRef.current;
    if (audio) {
      if (playing) {
        const started: unknown = audio.play();
        if (started instanceof Promise) started.catch(() => dispatch({ type: "pause" }));
      } else {
        audio.pause();
      }
    }
    dispatch({ type: playing ? "play" : "pause" });
    setAnnouncement(playing ? "Playing" : "Paused");
  }, []);

  const seekTo = useCallback((positionMs: number) => {
    const next = clampPosition(positionMs, stateRef.current.durationMs);
    const audio = audioRef.current;
    if (audio) audio.currentTime = next / 1000;
    dispatch({ type: "seek-to", positionMs: next });
    setAnnouncement(`Position ${formatClock(next)}`);
  }, []);

  const setSpeed = useCallback((speed: PlaybackSpeed) => {
    const audio = audioRef.current;
    if (audio) audio.playbackRate = speed;
    dispatch({ type: "set-speed", speed });
    setAnnouncement(`Speed ${speed}×`);
  }, []);

  return useMemo<PlayerController>(
    () => ({
      state,
      audioRef,
      announcement,
      setPlaying,
      toggle: () => setPlaying(!stateRef.current.playing),
      seekTo,
      skip: (deltaMs: number) => seekTo(stateRef.current.positionMs + deltaMs),
      setSpeed,
      stepSpeedBy: (direction: 1 | -1) => {
        const index = PLAYBACK_SPEEDS.indexOf(stateRef.current.speed);
        const bounded = Math.min(PLAYBACK_SPEEDS.length - 1, Math.max(0, index + direction));
        setSpeed(PLAYBACK_SPEEDS[bounded]!);
      },
      reportTime: (seconds: number) => {
        if (!Number.isFinite(seconds)) return;
        dispatch({ type: "time-update", positionMs: seconds * 1000 });
      },
      reportMediaDuration: (seconds: number) => {
        if (Number.isFinite(seconds) && seconds > 0) return;
        dispatch({ type: "set-duration", durationMs: null });
      },
      reportEnded: () => {
        dispatch({ type: "ended" });
        setAnnouncement("Ended");
      },
    }),
    [state, announcement, setPlaying, seekTo, setSpeed],
  );
}

const TEXT_ENTRY = "input, textarea, select, [contenteditable='true']";
const ACTIVATABLE = "button, [role='button'], a[href]";
const MODAL = "[role='dialog'], [role='alertdialog']";

/** The subset of a keyboard event the shortcuts need; React and DOM both fit. */
export interface ShortcutEvent {
  key: string;
  altKey: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
  target: EventTarget | null;
  preventDefault: () => void;
}

/**
 * §5.3 shortcuts, scoped to the detail view: Space play/pause, J/L ∓10 s,
 * K pause, `,`/`.` speed. Keys are left alone while a text field, a control or
 * a modal has focus so editing and dialogs keep working.
 */
export function usePlayerShortcuts(controller: PlayerController) {
  return useCallback(
    (event: ShortcutEvent) => {
      if (event.altKey || event.metaKey || event.ctrlKey) return;
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (target?.closest(TEXT_ENTRY) || target?.closest(MODAL)) return;
      switch (event.key) {
        case " ":
          if (target?.closest(ACTIVATABLE)) return;
          controller.toggle();
          break;
        case "j":
        case "J":
          controller.skip(-SKIP_MS);
          break;
        case "l":
        case "L":
          controller.skip(SKIP_MS);
          break;
        case "k":
        case "K":
          controller.setPlaying(false);
          break;
        case ",":
          controller.stepSpeedBy(-1);
          break;
        case ".":
          controller.stepSpeedBy(1);
          break;
        default:
          return;
      }
      event.preventDefault();
    },
    [controller],
  );
}

export interface AudioPlayerProps {
  recordingId: string;
  controller: PlayerController;
  /** Used for the transport group's accessible name. */
  title: string;
}

export function AudioPlayer({ recordingId, controller, title }: AudioPlayerProps) {
  const { state, audioRef } = controller;
  const timeline = state.durationMs !== null;
  const peaks = usePeaks(recordingId, timeline);
  const canvasRef = useWaveformCanvas(peaks, state);

  const onSliderKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const duration = state.durationMs;
    if (duration === null) return;
    switch (event.key) {
      case "ArrowLeft":
      case "ArrowDown":
        controller.skip(-ARROW_SEEK_MS);
        break;
      case "ArrowRight":
      case "ArrowUp":
        controller.skip(ARROW_SEEK_MS);
        break;
      case "PageDown":
        controller.skip(-PAGE_SEEK_MS);
        break;
      case "PageUp":
        controller.skip(PAGE_SEEK_MS);
        break;
      case "Home":
        controller.seekTo(0);
        break;
      case "End":
        controller.seekTo(duration);
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  const onSliderPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    const duration = state.durationMs;
    if (duration === null) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const ratio = (event.clientX - rect.left) / rect.width;
    controller.seekTo(duration * Math.min(1, Math.max(0, ratio)));
  };

  const progress = state.durationMs ? state.positionMs / state.durationMs : 0;

  return (
    <section className="player" aria-label={`Playback: ${title}`}>
      <audio
        ref={audioRef}
        className="player__media"
        src={mediaUrl(recordingId)}
        preload="metadata"
        data-testid="player-media"
        onTimeUpdate={(event) => controller.reportTime(event.currentTarget.currentTime)}
        onDurationChange={(event) => controller.reportMediaDuration(event.currentTarget.duration)}
        onEnded={() => controller.reportEnded()}
      />

      <div className="player__transport">
        <Button
          variant="primary"
          glyph={state.playing ? "❙❙" : "▶"}
          onClick={() => controller.toggle()}
        >
          {state.playing ? "Pause" : "Play"}
        </Button>
        <Button
          glyph="↩"
          disabled={!timeline}
          onClick={() => controller.skip(-SKIP_MS)}
          aria-describedby={timeline ? undefined : "player-skip-hint"}
        >
          Back 10 s
        </Button>
        <Button
          glyph="↪"
          disabled={!timeline}
          onClick={() => controller.skip(SKIP_MS)}
          aria-describedby={timeline ? undefined : "player-skip-hint"}
        >
          Forward 10 s
        </Button>
        <div className="player__speed">
          <SelectField
            label="Playback speed"
            value={String(state.speed)}
            onChange={(event) => controller.setSpeed(Number(event.target.value) as PlaybackSpeed)}
          >
            {PLAYBACK_SPEEDS.map((speed) => (
              <option key={speed} value={String(speed)}>
                {speed}×
              </option>
            ))}
          </SelectField>
        </div>
      </div>

      {timeline ? (
        <div
          className="player__scrubber"
          role="slider"
          tabIndex={0}
          aria-label="Seek"
          aria-valuemin={0}
          aria-valuemax={Math.round((state.durationMs ?? 0) / 1000)}
          aria-valuenow={Math.round(state.positionMs / 1000)}
          aria-valuetext={formatPositionLabel(state)}
          onKeyDown={onSliderKeyDown}
          onPointerDown={onSliderPointerDown}
        >
          {peaks ? (
            <canvas
              ref={canvasRef}
              className="player__waveform"
              aria-hidden="true"
              data-testid="player-waveform"
            />
          ) : (
            <div className="player__track" aria-hidden="true">
              <div className="player__track-fill" style={{ width: `${progress * 100}%` }} />
            </div>
          )}
        </div>
      ) : (
        <p className="player__unknown" id="player-skip-hint">
          Duration unknown — this file reports no measurable length, so there is no timeline to
          scrub and skipping by 10 seconds is unavailable.
        </p>
      )}

      <p className="player__time">
        {timeline
          ? `${formatClock(state.positionMs)} of ${formatClock(state.durationMs ?? 0)}`
          : `${formatClock(state.positionMs)} of an unknown duration`}
      </p>

      <p className="sr-only" role="status" aria-live="polite">
        {controller.announcement}
      </p>
    </section>
  );
}

/* --------------------------------------------------------------- waveform */

interface IdleHandle {
  cancel: () => void;
}

/** Idle slice scheduler; falls back to a macrotask where idle callbacks are absent. */
function scheduleIdle(run: () => void): IdleHandle {
  if (typeof requestIdleCallback === "function") {
    const handle = requestIdleCallback(() => run());
    return {
      cancel: () => {
        if (typeof cancelIdleCallback === "function") cancelIdleCallback(handle);
      },
    };
  }
  const timer = setTimeout(run, 0);
  return { cancel: () => clearTimeout(timer) };
}

/**
 * Decodes the media once and reduces it to at most `MAX_PEAK_BUCKETS` peaks,
 * one slice per idle callback so a multi-hour recording never blocks the
 * window. Returns null when the platform cannot decode audio — the caller then
 * renders a plain seek bar rather than a fabricated waveform.
 */
function usePeaks(recordingId: string, enabled: boolean): Float32Array | null {
  const [peaks, setPeaks] = useState<Float32Array | null>(null);

  useEffect(() => {
    setPeaks(null);
    if (!enabled) return;
    if (typeof AudioContext !== "function" || typeof fetch !== "function") return;

    let cancelled = false;
    let pending: IdleHandle | null = null;
    let context: AudioContext | null = null;

    const decode = async () => {
      const response = await fetch(mediaUrl(recordingId));
      const encoded = await response.arrayBuffer();
      if (cancelled) return;
      context = new AudioContext();
      const buffer = await context.decodeAudioData(encoded);
      if (cancelled) return;
      const channel = buffer.getChannelData(0);
      const buckets = peakBucketCount(channel.length);
      const out = new Float32Array(buckets);
      let next = 0;
      const step = () => {
        if (cancelled) return;
        const end = Math.min(buckets, next + PEAK_CHUNK_BUCKETS);
        computePeakRange(channel, out, next, end);
        next = end;
        // Progressive paint: the user sees the waveform fill in as we decode.
        setPeaks(Float32Array.from(out));
        if (next < buckets) pending = scheduleIdle(step);
      };
      pending = scheduleIdle(step);
    };

    void decode().catch(() => {
      if (!cancelled) setPeaks(null);
    });

    return () => {
      cancelled = true;
      pending?.cancel();
      void context?.close().catch(() => undefined);
    };
  }, [recordingId, enabled]);

  return peaks;
}

function useWaveformCanvas(peaks: Float32Array | null, state: PlayerState) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !peaks) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    const { width, height } = canvas.getBoundingClientRect();
    const pixelWidth = Math.max(1, Math.round(width));
    const pixelHeight = Math.max(1, Math.round(height));
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
    const styles = getComputedStyle(canvas);
    const playedColor = styles.getPropertyValue("--color-accent").trim();
    const remainingColor = styles.getPropertyValue("--color-border-strong").trim();
    context.clearRect(0, 0, pixelWidth, pixelHeight);
    const playedRatio = state.durationMs ? state.positionMs / state.durationMs : 0;
    const columns = Math.min(peaks.length, pixelWidth);
    const columnWidth = pixelWidth / Math.max(1, columns);
    for (let column = 0; column < columns; column += 1) {
      const peak = peaks[Math.floor((column / columns) * peaks.length)] ?? 0;
      const barHeight = Math.max(1, peak * pixelHeight);
      context.fillStyle = column / columns <= playedRatio ? playedColor : remainingColor;
      context.fillRect(
        column * columnWidth,
        (pixelHeight - barHeight) / 2,
        Math.max(1, columnWidth - 1),
        barHeight,
      );
    }
  }, [peaks, state.positionMs, state.durationMs]);

  return canvasRef;
}

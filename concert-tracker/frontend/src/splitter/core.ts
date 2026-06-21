// Pure, DOM-free editor logic for the track-split timeline (concert detail
// page). Adjacent tracks share a single split point by default (the boundary
// is "linked" — one handle is the end of track i and the start of track
// i+1). A boundary can be "detached" into two independent handles so the
// user can open a gap that belongs to no track (e.g. to cut out talking
// between songs).
//
// Ported 1:1 from the original static/splitter.js `_pure` namespace (see
// js-tests/splitter.test.ts, which now imports straight from this module
// instead of requiring the compiled bundle). No DOM access here — see
// ./index.ts for the interaction layer.
import type { SongTimestamp, SplitTimestampsResponse, TimestampPayload } from "../api/client";

// Mirror of MIN_SONG_DURATION_SECONDS in concert-tracker/src/split_timestamps.rs.
// The server rejects any track shorter than this, so the editor never lets a
// segment shrink below it.
export const MIN_SEG = 1.0;

// Two boundary times within this many seconds are treated as a single linked
// split point when loading stored timestamps.
const LINK_EPS = 0.05;

export interface EditorTrack {
  title: string;
  start: number;
  end: number;
}

export interface EditorState {
  duration: number;
  tracks: EditorTrack[];
  /** linked[i] describes the boundary between tracks[i] and tracks[i + 1]. */
  linked: boolean[];
}

export type HandleKind = "start" | "end";

/**
 * A draggable timeline handle. `boundary`/`linked` are only present for
 * interior handles (i.e. not the head-trim or tail-trim handle).
 */
export interface Handle {
  kind: HandleKind;
  track: number;
  boundary?: number;
  linked?: boolean;
}

export function clamp(v: number, lo: number, hi: number): number {
  if (hi < lo) return lo;
  return v < lo ? lo : v > hi ? hi : v;
}

export function round3(v: number): number {
  return Math.round(v * 1000) / 1000;
}

/**
 * Parse "m:ss(.s)", "h:mm:ss", or a bare seconds number into seconds.
 * Returns NaN for anything unparseable so callers can reject it.
 */
export function parseTimecode(str: unknown): number {
  if (typeof str !== "string") return NaN;
  const s = str.trim();
  if (s === "") return NaN;
  if (s.indexOf(":") === -1) {
    const n = Number(s);
    return Number.isFinite(n) ? n : NaN;
  }
  const parts = s.split(":");
  if (parts.length > 3) return NaN;
  let total = 0;
  for (const p of parts) {
    if (p.trim() === "") return NaN;
    const n = Number(p);
    if (!Number.isFinite(n) || n < 0) return NaN;
    total = total * 60 + n;
  }
  return total;
}

/** Format seconds as "m:ss.s" (one decimal) for the numeric inputs. */
export function formatTimecode(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) sec = 0;
  const m = Math.floor(sec / 60);
  const rest = sec - m * 60;
  const s = rest.toFixed(1).padStart(4, "0"); // "05.0"
  return m + ":" + s;
}

/**
 * Build editor state from the GET response. Prefers user timestamps over
 * auto. `duration` falls back to the last end time when media_duration is
 * null (e.g. ffprobe unavailable), so the timeline still has a right edge.
 * Returns null when there are no timestamps to edit yet.
 */
export function initState(resp: SplitTimestampsResponse): EditorState | null {
  const chosen: SongTimestamp[] | null | undefined = resp.user ?? resp.auto;
  if (!chosen || chosen.length === 0) return null;
  const tracks: EditorTrack[] = chosen.map((t) => ({
    title: t.title,
    start: t.start_time,
    end: t.end_time,
  }));
  const lastTrack = tracks[tracks.length - 1];
  if (!lastTrack) return null; // unreachable (chosen.length > 0 checked above)
  const lastEnd = lastTrack.end;
  let duration = resp.media_duration;
  if (duration == null || !Number.isFinite(duration) || duration < lastEnd) {
    duration = lastEnd;
  }
  const linked: boolean[] = [];
  for (let i = 0; i < tracks.length - 1; i++) {
    // Non-null: i and i+1 are both < tracks.length by the loop bound.
    linked.push(Math.abs(tracks[i]!.end - tracks[i + 1]!.start) <= LINK_EPS);
  }
  return { duration, tracks, linked };
}

/**
 * Move track i's start, honouring the linked boundary to its left. Mutates
 * and returns the passed-in state.
 */
export function setStart(state: EditorState, i: number, value: number): EditorState {
  const t = state.tracks;
  const lower = i === 0 ? 0 : state.linked[i - 1] ? t[i - 1]!.start + MIN_SEG : t[i - 1]!.end;
  const upper = t[i]!.end - MIN_SEG;
  const v = clamp(value, lower, upper);
  t[i]!.start = v;
  if (i > 0 && state.linked[i - 1]) t[i - 1]!.end = v;
  return state;
}

/** Move track i's end, honouring the linked boundary to its right. */
export function setEnd(state: EditorState, i: number, value: number): EditorState {
  const t = state.tracks;
  const last = t.length - 1;
  const upper =
    i === last ? state.duration : state.linked[i] ? t[i + 1]!.end - MIN_SEG : t[i + 1]!.start;
  const lower = t[i]!.start + MIN_SEG;
  const v = clamp(value, lower, upper);
  t[i]!.end = v;
  if (i < last && state.linked[i]) t[i + 1]!.start = v;
  return state;
}

/**
 * Split boundary i into two independent handles. Pure topology change — no
 * value is nudged, so the gap starts at zero until the user drags them apart.
 */
export function detach(state: EditorState, i: number): EditorState {
  state.linked[i] = false;
  return state;
}

/**
 * Re-couple boundary i, collapsing any gap by pulling track i+1's start back
 * to track i's end (which is always a valid start for i+1).
 */
export function link(state: EditorState, i: number): EditorState {
  state.linked[i] = true;
  state.tracks[i + 1]!.start = state.tracks[i]!.end;
  return state;
}

/**
 * Ordered list of draggable handles for the current topology. Each handle
 * reads and writes one boundary time via setStart/setEnd (which handle
 * linking).
 */
export function handlesFor(state: EditorState): Handle[] {
  const t = state.tracks;
  const last = t.length - 1;
  const out: Handle[] = [{ kind: "start", track: 0 }]; // head (trim intro)
  for (let i = 0; i < last; i++) {
    if (state.linked[i]) {
      out.push({ kind: "end", track: i, boundary: i, linked: true });
    } else {
      out.push({ kind: "end", track: i, boundary: i, linked: false });
      out.push({ kind: "start", track: i + 1, boundary: i, linked: false });
    }
  }
  out.push({ kind: "end", track: last }); // tail (trim outro)
  return out;
}

export function handleTime(state: EditorState, h: Handle): number {
  return h.kind === "start" ? state.tracks[h.track]!.start : state.tracks[h.track]!.end;
}

export function applyHandle(state: EditorState, h: Handle, value: number): EditorState {
  return h.kind === "start" ? setStart(state, h.track, value) : setEnd(state, h.track, value);
}

/** Validate against the same rules the server enforces. Returns error strings. */
export function validate(state: EditorState): string[] {
  const errors: string[] = [];
  const t = state.tracks;
  for (let i = 0; i < t.length; i++) {
    const track = t[i]!;
    if (track.start < -1e-6) errors.push(`${track.title}: starts before 0`);
    if (track.end - track.start < MIN_SEG - 1e-6) {
      errors.push(`${track.title}: shorter than ${MIN_SEG}s`);
    }
    if (track.end > state.duration + 1e-6) errors.push(`${track.title}: ends past media duration`);
    if (i < t.length - 1 && track.end > t[i + 1]!.start + 1e-6) {
      errors.push(`${track.title} overlaps ${t[i + 1]!.title}`);
    }
  }
  return errors;
}

export function buildPayload(state: EditorState): TimestampPayload {
  return {
    songs: state.tracks.map((t) => ({
      title: t.title,
      start_time: round3(t.start),
      end_time: round3(t.end),
    })),
  };
}

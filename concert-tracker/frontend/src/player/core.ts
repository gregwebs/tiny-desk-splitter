// Pure logic extracted from the pre-Foldkit ../player.ts: queue algebra, the
// contiguous-groupId queue-row grouping, next/prev enablement, concert
// reconstruction navigation math, and small predicates. No global `document`,
// `audio`, or `fetch` access here; predicates may take a DOM `Element` as an
// argument (e.g. clickShouldDismiss, the keyboard-target helpers below) and
// call closest/matches on it. See ../player/widget/ for the interaction
// layer (subscriptions, commands) that supplies those arguments.
// Unit-tested directly: see the *.unit.test.ts files alongside this one.
import type { PlaybackItemJson } from "../api/client";

// ── State shapes ─────────────────────────────────────────────────────────

export interface ConcertPlaybackState {
  id: number;
  items: PlaybackItemJson[];
  pos: number;
}

// The player's current-playback state. Named PlaybackState here (was
// PlayerState in the original module) to avoid colliding with the Foldkit
// widget's eventual MVU Model type.
export interface PlaybackState {
  concertId: number | null;
  trackIdx: number | null;
  isVideo: boolean;
  watchUrl: string | null;
  hasNext: boolean;
  hasPrev: boolean;
  liked: boolean;
  concert: ConcertPlaybackState | null;
}

export interface QueueEntry {
  concertId: number;
  trackIdx: number;
  title: string;
  liked: boolean;
  playlistName: string | null;
  groupId: number | null;
}

// ── Constants ────────────────────────────────────────────────────────────

export const SIDEBAR_WIDTH_KEY = "sidebarWidth";
export const SIDEBAR_MIN_WIDTH = 240;
export const SIDEBAR_MAX_WIDTH = 600;

// Downloads can take many minutes; give the whole prepare/poll chain a
// generous cap so an abandoned poll loop can't run forever.
export const PREPARE_POLL_MS = 2000;
export const PREPARE_TIMEOUT_MS = 30 * 60 * 1000;

// How long the video minimize button stays visible after the last mouse movement.
export const VIDEO_CONTROLS_IDLE_MS = 2500;

// Recognizes native controls and inline onclick handlers (the project's convention); a
// future control bound only via addEventListener would need adding here to be exempted.
export const INTERACTIVE_SELECTOR =
  'a, button, input, select, textarea, label, [role="button"], [onclick]';

// ── Sidebar width ────────────────────────────────────────────────────────

// The 240/600 clamp with no DOM access.
export function clampSidebarWidth(px: number): number {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, Math.round(px)));
}

// ── Queue algebra ────────────────────────────────────────────────────────

// Shared entry constructor so enqueue and playPlaylist both produce the same shape.
// playlistName is null for ad-hoc queued tracks and non-null when the entry came
// from a playlist (used by play() to show/clear the bar label). groupId is non-null
// only for playlist tracks; a contiguous run of entries sharing a groupId renders as
// one grouped block in the queue sidebar (see buildQueueRows).
export function makeQueueEntry(
  concertId: number,
  trackIdx: number,
  title: string,
  liked: boolean,
  playlistName: string | null = null,
  groupId: number | null = null,
): QueueEntry {
  return {
    concertId,
    trackIdx,
    title,
    liked: !!liked,
    playlistName: playlistName || null,
    groupId: groupId || null,
  };
}

// Append `entry` to `queue` unless a (concertId, trackIdx) duplicate is
// already queued. Returns a new array either way; `added` tells the caller
// whether anything changed (so it can skip a re-render/log).
export function enqueueDedupe(
  queue: readonly QueueEntry[],
  entry: QueueEntry,
): { queue: QueueEntry[]; added: boolean } {
  if (queue.some((q) => q.concertId === entry.concertId && q.trackIdx === entry.trackIdx)) {
    return { queue: [...queue], added: false };
  }
  return { queue: [...queue, entry], added: true };
}

// Build the queue entries for an entire playlist's available, resolved
// tracks, all sharing one groupId so they form a single removable block in
// the queue sidebar (see buildQueueRows / removeGroup).
export function playlistEntries(
  tracks: readonly { concert_id: number; track_index: number | null; title: string }[],
  playlistName: string,
  groupId: number,
): QueueEntry[] {
  const entries: QueueEntry[] = [];
  for (const t of tracks) {
    if (t.track_index == null) continue; // available implies a track index; guard for the type
    entries.push(makeQueueEntry(t.concert_id, t.track_index, t.title, false, playlistName, groupId));
  }
  return entries;
}

// Remove the entry at `pos`. Returns a new array.
export function dequeueAt(queue: readonly QueueEntry[], pos: number): QueueEntry[] {
  const next = [...queue];
  next.splice(pos, 1);
  return next;
}

// Remove every entry belonging to a playlist group in one action. Returns a new array.
export function removeGroup(queue: readonly QueueEntry[], groupId: number): QueueEntry[] {
  return queue.filter((q) => q.groupId !== groupId);
}

// Pop the head of the queue (FIFO), e.g. for auto-advance into the next
// queued track. Returns the popped entry (null if empty) and the remaining queue.
export function takeFromQueue(queue: readonly QueueEntry[]): {
  entry: QueueEntry | null;
  queue: QueueEntry[];
} {
  if (queue.length === 0) return { entry: null, queue: [] };
  const [entry, ...rest] = queue;
  return { entry: entry!, queue: rest };
}

// ── Queue sidebar rows ───────────────────────────────────────────────────

export type QueueRow =
  | { kind: "group-header"; groupId: number; name: string }
  | { kind: "song"; pos: number; entry: QueueEntry; nested: boolean };

export interface QueueRowsResult {
  rows: QueueRow[];
  // groupIds that reappeared non-contiguously (a future non-tail insert could
  // cause this; the caller should log it rather than silently splitting a
  // group across two headers, which is what this grouping logic does today).
  nonContiguousGroups: number[];
}

// Build the queue sidebar's row list from the queue: queue entries whose
// groupId is non-null and contiguous form a playlist group — one header row
// (playlist name) followed by indented song rows. Ad-hoc entries
// (groupId === null) render as ungrouped song rows. The list is reversed
// (highest index first), matching the sidebar's bottom-scrolled display order.
export function buildQueueRows(queue: readonly QueueEntry[]): QueueRowsResult {
  const rows: QueueRow[] = [];
  const nonContiguousGroups: number[] = [];

  // prevGroupId tracks the last seen groupId so we emit one header per
  // contiguous run. Seeded to undefined (not null) because null is the
  // meaningful "ad-hoc" value.
  let prevGroupId: number | null | undefined = undefined;
  const headeredGroups = new Set<number>();

  for (let i = queue.length - 1; i >= 0; i--) {
    const entry = queue[i]!;

    if (entry.groupId !== null && entry.groupId !== prevGroupId) {
      if (headeredGroups.has(entry.groupId)) {
        nonContiguousGroups.push(entry.groupId);
      }
      headeredGroups.add(entry.groupId);
      prevGroupId = entry.groupId;
      rows.push({ kind: "group-header", groupId: entry.groupId, name: entry.playlistName || "Playlist" });
    } else if (entry.groupId === null) {
      prevGroupId = null;
    }

    rows.push({ kind: "song", pos: i, entry, nested: entry.groupId !== null });
  }
  return { rows, nonContiguousGroups };
}

// ── Next/prev button enablement ─────────────────────────────────────────

// There is "something next" when in concert mode with a next item, the queue
// is non-empty, or the current track has a following track to auto-advance to.
export function nextEnabled(s: PlaybackState, queueLen: number): boolean {
  if (s.concert) {
    return s.concert.pos + 1 < s.concert.items.length;
  }
  return queueLen > 0 || s.hasNext;
}

// There is "something previous" when in concert mode with an earlier item, or
// the current track has a preceding track.
export function prevEnabled(s: PlaybackState): boolean {
  if (s.concert) {
    return s.concert.pos > 0;
  }
  return s.hasPrev;
}

// ── Concert reconstruction navigation ────────────────────────────────────

// Navigation facts for the concert item at `pos`. `item` is included for
// completeness/testability — today's only caller (player.ts's playConcertItem)
// already has the item from its own lookup and only reads hasPrev/hasNext.
export function concertItemNav(
  items: readonly PlaybackItemJson[],
  pos: number,
): { hasPrev: boolean; hasNext: boolean; item: PlaybackItemJson | null } {
  return {
    hasPrev: pos > 0,
    hasNext: pos + 1 < items.length,
    item: items[pos] ?? null,
  };
}

// The next position to advance to, or null at the end of the concert
// (caller should clear concert mode and hide the video panel).
export function concertAdvancePos(pos: number, len: number): number | null {
  const next = pos + 1;
  return next >= len ? null : next;
}

// Re-find a concert position by URL after the item list is refreshed
// (e.g. a track was deleted mid-concert), so navigation stays correct after
// items shift. Falls back to `fallbackPos` when there's no current URL or no match.
export function refindPosByUrl(
  items: readonly PlaybackItemJson[],
  currentUrl: string | null,
  fallbackPos: number,
): number {
  if (!currentUrl) return fallbackPos;
  const idx = items.findIndex((item) => item.url === currentUrl);
  return idx >= 0 ? idx : fallbackPos;
}

// ── Time / click / key predicates ───────────────────────────────────────

export function formatTime(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return m + ":" + (s < 10 ? "0" : "") + s;
}

// Does a click on `target` fall on dead space outside `container`, and so
// should dismiss the video? (false for clicks inside the container or on any
// interactive control, per INTERACTIVE_SELECTOR)
export function clickShouldDismiss(target: EventTarget | null, container: Element | null): boolean {
  if (!container || !target) return false;
  if (!(target instanceof Node)) return false;
  if (container.contains(target)) return false;
  if (target instanceof Element && target.closest && target.closest(INTERACTIVE_SELECTOR)) {
    return false;
  }
  return true;
}

export function isPlainSpaceKey(e: KeyboardEvent): boolean {
  return (
    (e.code === "Space" || e.key === " " || e.key === "Spacebar") &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    !e.shiftKey
  );
}

export function isPlainEscapeKey(e: KeyboardEvent): boolean {
  return (
    (e.code === "Escape" || e.key === "Escape" || e.key === "Esc") &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    !e.shiftKey
  );
}

// True when a click on `anchor` should fall through to the native href
// (new-tab, download, etc.) instead of being intercepted for an htmx partial
// swap — mirrors Foldkit's own link-router guard (browserListeners.ts):
// non-primary button, any modifier key, an already-handled event, a
// non-_self target, or a download link.
export function nativeClickShouldWin(e: MouseEvent, anchor: HTMLAnchorElement): boolean {
  if (e.button !== 0) return true;
  if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return true;
  if (e.defaultPrevented) return true;
  if (anchor.target !== "" && anchor.target !== "_self") return true;
  if (anchor.hasAttribute("download")) return true;
  return false;
}

// True for text-entry targets where native key behavior (typing a space,
// clearing/blurring on Escape) must win over the global player shortcuts.
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target.matches("input, textarea, select")) return true;
  const editable = target.closest<HTMLElement>("[contenteditable]");
  return !!(editable && editable.isContentEditable);
}

// True for targets where the global Space shortcut should still control
// playback: the player bar and the inline video panel (which contains
// #player-audio), excluding anything editable within them (e.g. #player-seek).
export function isPlayerPlaybackShortcutTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (isEditableTarget(target)) return false;
  return !!target.closest("#player-bar, #player-video-panel");
}

// True when the global Space shortcut should be suppressed in favor of
// native key behavior: editable targets, or interactive controls
// (INTERACTIVE_SELECTOR) outside the player bar / video panel.
export function isKeyboardShortcutIgnoredTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (isPlayerPlaybackShortcutTarget(target)) return false;
  if (isEditableTarget(target)) return true;
  return !!target.closest(INTERACTIVE_SELECTOR);
}

// Typed wrappers over fetch() for the JSON API, built on types generated from
// the backend's OpenAPI spec (see frontend/src/generated/openapi.d.ts and
// `just openapi-types`). Mirrors the request/response handling style already
// used by the hand-written JS (raw Response from POST/PATCH/DELETE so callers
// branch on status code; thrown error from GET helpers).
//
// Note on Option<T> fields: the backend serializes `Option<T>` as `T | null`
// (always present, never omitted — see e.g. MediaInfo.track_index). The
// generated types model these as `T | null | undefined` (slightly looser,
// since the JSON Schema "not required" also allows omission, which the server
// never actually does). Treat `null` and `undefined` as equivalent when
// reading these fields; don't rely on the field being absent.
import type { components } from "../generated/openapi";

type Schemas = components["schemas"];

// ── Re-exported wire types (used by the modules instead of redeclaring them) ──

export type MediaInfo = Schemas["MediaInfo"];
export type PrepareStatus = Schemas["PrepareStatus"];
export type PlaybackItemJson = Schemas["PlaybackItemJson"];
export type ConcertPlaybackResponse = Schemas["ConcertPlaybackResponse"];
export type SplitTimestampsResponse = Schemas["SplitTimestampsResponse"];
export type SplitStartStatus = Schemas["SplitStartStatus"];
export type SplitStartResponse = Schemas["SplitStartResponse"];
export type SongTimestamp = Schemas["SongTimestamp"];
export type TimestampPayload = Schemas["TimestampPayload"];
export type TimestampPayloadSong = Schemas["TimestampPayloadSong"];

export type PlaylistJson = Schemas["PlaylistJson"];
export type MembershipJson = Schemas["MembershipJson"];
export type ResolvedTrackJson = Schemas["ResolvedTrackJson"];
export type PlaylistSummaryJson = Schemas["PlaylistSummaryJson"];
export type PlaylistListEntry = Schemas["PlaylistListEntry"];
export type PlaylistItemJson = Schemas["PlaylistItemJson"];
export type PlaylistDetailJson = Schemas["PlaylistDetailJson"];
export type CreatePlaylistReq = Schemas["CreatePlaylistReq"];
export type UpdatePlaylistReq = Schemas["UpdatePlaylistReq"];
export type AddItemReq = Schemas["AddItemReq"];
export type ReorderReq = Schemas["ReorderReq"];
export type CreatedPlaylistJson = Schemas["CreatedPlaylistJson"];
export type CreatedItemJson = Schemas["CreatedItemJson"];

// Narrows ConcertPlaybackResponse to its "source" variant. The generated type
// is a true discriminated union on `mode` (verified against the OpenAPI
// `oneOf` + `#[serde(tag = "mode")]` on the Rust side), so `resp.mode ===
// "source"` already narrows `resp` for TypeScript — this helper just documents
// the call site.
export function isSourcePlayback(
  resp: ConcertPlaybackResponse,
): resp is Extract<ConcertPlaybackResponse, { mode: "source" }> {
  return resp.mode === "source";
}

// ── Generic fetch helpers ──────────────────────────────────────────────────

export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message?: string) {
    super(message ?? `HTTP ${status}`);
    this.name = "ApiError";
    this.status = status;
  }
}

/**
 * Decodes the JSON body of a Response already obtained by the caller (e.g.
 * after branching on status code). The single unchecked assertion `json()`
 * requires — the wire body is genuinely `unknown` until here — is centralized
 * in this one spot instead of repeated at every parse site.
 */
export async function readJson<T>(r: Response): Promise<T> {
  // oxlint-disable-next-line typescript/consistent-type-assertions -- json() returns Promise<unknown>; T is the caller-supplied wire type generated from the OpenAPI spec.
  return (await r.json()) as T;
}

/**
 * GET a JSON endpoint, throwing ApiError on a non-2xx response. `init` is
 * passed through to fetch() unchanged — mainly used to thread an
 * AbortSignal for cancellable requests (e.g. next/prev track prefetch).
 */
export async function getJson<T>(url: string, init?: RequestInit): Promise<T> {
  const r = await fetch(url, init);
  if (!r.ok) throw new ApiError(r.status);
  return readJson<T>(r);
}

/** GET a JSON endpoint, returning null instead of throwing on a non-2xx response. */
export async function getJsonOrNull<T>(url: string, init?: RequestInit): Promise<T | null> {
  const r = await fetch(url, init);
  if (!r.ok) return null;
  return readJson<T>(r);
}

/**
 * GET an endpoint and return the raw Response, for callers that need to
 * branch on status code themselves before consuming the body (e.g. an HTML
 * response on success vs. a plain-text error message).
 */
export async function fetchRaw(url: string, init?: RequestInit): Promise<Response> {
  return fetch(url, init);
}

/**
 * POST/PATCH/PUT/DELETE with an optional JSON body, returning the raw
 * Response so callers can branch on status code (e.g. 202 vs 409 vs 422) the
 * same way the original hand-written fetch() call sites did.
 */
export async function sendJson<TReq = unknown>(
  url: string,
  body?: TReq,
  method: "POST" | "PATCH" | "PUT" | "DELETE" = "POST",
): Promise<Response> {
  const init: RequestInit = { method };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  return fetch(url, init);
}

/** GET an endpoint that returns server-rendered HTML (not in the OpenAPI doc). */
export async function fetchText(url: string): Promise<string> {
  const r = await fetch(url);
  return r.text();
}

// ── Playlists JSON API (validated first — see docs/change/frontend-typescript.md) ──

export async function listPlaylists(): Promise<PlaylistListEntry[]> {
  return getJson<PlaylistListEntry[]>("/api/playlists");
}

export async function getPlaylist(id: number): Promise<PlaylistDetailJson> {
  return getJson<PlaylistDetailJson>(`/api/playlists/${id}`);
}

export async function createPlaylist(req: CreatePlaylistReq): Promise<Response> {
  return sendJson<CreatePlaylistReq>("/api/playlists", req, "POST");
}

export async function updatePlaylist(id: number, req: UpdatePlaylistReq): Promise<Response> {
  return sendJson<UpdatePlaylistReq>(`/api/playlists/${id}`, req, "PATCH");
}

export async function deletePlaylist(id: number): Promise<Response> {
  return sendJson(`/api/playlists/${id}`, undefined, "DELETE");
}

export async function addPlaylistItem(id: number, req: AddItemReq): Promise<Response> {
  return sendJson<AddItemReq>(`/api/playlists/${id}/items`, req, "POST");
}

export async function removePlaylistItem(id: number, itemId: number): Promise<Response> {
  return sendJson(`/api/playlists/${id}/items/${itemId}`, undefined, "DELETE");
}

export async function reorderPlaylistItems(id: number, itemIds: number[]): Promise<Response> {
  return sendJson<ReorderReq>(`/api/playlists/${id}/items/reorder`, { item_ids: itemIds }, "POST");
}

export async function trackMembership(concertId: number, trackIndex: number): Promise<MembershipJson[]> {
  return getJson<MembershipJson[]>(`/api/concerts/${concertId}/tracks/${trackIndex}/playlists`);
}

export async function concertMembership(concertId: number): Promise<MembershipJson[]> {
  return getJson<MembershipJson[]>(`/api/concerts/${concertId}/playlists`);
}

export async function playlistNestedIn(childPlaylistId: number): Promise<MembershipJson[]> {
  return getJson<MembershipJson[]>(`/api/playlists/${childPlaylistId}/nested-in`);
}

// ── Playback / media-info / prepare (player.ts) ─────────────────────────────

export async function getMediaInfo(concertId: number): Promise<MediaInfo> {
  return getJson<MediaInfo>(`/concerts/${concertId}/media-info`);
}

export async function getTrackMediaInfo(concertId: number, trackIdx: number): Promise<MediaInfo> {
  return getJson<MediaInfo>(`/concerts/${concertId}/tracks/${trackIdx}/media-info`);
}

export async function getTrackMediaInfoOrNull(
  concertId: number,
  trackIdx: number,
): Promise<MediaInfo | null> {
  return getJsonOrNull<MediaInfo>(`/concerts/${concertId}/tracks/${trackIdx}/media-info`);
}

export async function getNextTrackMediaInfo(
  concertId: number,
  trackIdx: number,
  signal?: AbortSignal,
): Promise<MediaInfo> {
  return getJson<MediaInfo>(`/concerts/${concertId}/tracks/${trackIdx}/next-media-info`, {
    signal: signal ?? null,
  });
}

export async function getPrevTrackMediaInfo(
  concertId: number,
  trackIdx: number,
  signal?: AbortSignal,
): Promise<MediaInfo> {
  return getJson<MediaInfo>(`/concerts/${concertId}/tracks/${trackIdx}/prev-media-info`, {
    signal: signal ?? null,
  });
}

export async function getConcertPlayback(concertId: number): Promise<ConcertPlaybackResponse> {
  return getJson<ConcertPlaybackResponse>(`/concerts/${concertId}/concert-playback`);
}

export async function postPrepare(concertId: number): Promise<Response> {
  return sendJson(`/concerts/${concertId}/prepare`, undefined, "POST");
}

export async function getPrepareStatus(concertId: number): Promise<PrepareStatus> {
  return getJson<PrepareStatus>(`/concerts/${concertId}/prepare-status`);
}

export async function postLikeTrack(concertId: number, trackIdx: number): Promise<Response> {
  return sendJson(`/concerts/${concertId}/tracks/${trackIdx}/like`, undefined, "POST");
}

export async function postDeleteTrack(concertId: number, trackIdx: number): Promise<Response> {
  return sendJson(`/concerts/${concertId}/tracks/${trackIdx}/delete`, undefined, "POST");
}

export async function postDeleteInterlude(
  concertId: number,
  interludeIdx: number,
): Promise<Response> {
  return sendJson(`/concerts/${concertId}/interludes/${interludeIdx}/delete`, undefined, "POST");
}

/** GET the sidebar's track-list HTML fragment (not in the OpenAPI doc — see fetchText). */
export async function fetchSidebarTracks(
  concertId: number,
  opts: { concertPlayback?: boolean } = {},
): Promise<Response> {
  const param = opts.concertPlayback ? "&playback=concert" : "";
  return fetchRaw(`/concerts/${concertId}/tracks?context=sidebar${param}`);
}

export type TrackDetailItem = Schemas["TrackDetailItem"];
export type TrackDetailsResponse = Schemas["TrackDetailsResponse"];

/** `GET /concerts/:id/track-details` — JSON track list for the player
 *  widget's sidebar concert section (whole-album / normal mode). */
export async function getTrackDetails(concertId: number): Promise<TrackDetailsResponse> {
  return getJson<TrackDetailsResponse>(`/concerts/${concertId}/track-details`);
}

/** Fire-and-forget POST to a server-provided listen/watch event URL. */
export async function postEvent(url: string): Promise<Response> {
  return sendJson(url, undefined, "POST");
}

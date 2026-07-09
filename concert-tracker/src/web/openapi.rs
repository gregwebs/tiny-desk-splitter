//! OpenAPI 3.1 document for the JSON API surface (the `/api/playlists*` family
//! plus the JSON helpers that sit alongside the htmx HTML pages: playback,
//! media-info, prepare-status, and split-timestamps). HTML/htmx routes are
//! intentionally not represented here — this documents the machine-readable
//! contract, not the whole app.
//!
//! Served at runtime via `/api-docs/openapi.json` and an interactive Swagger UI
//! at `/swagger-ui` (wired in [`super::router_with_opts`]).
//!
//! Paths are NOT listed here: each JSON handler's `#[utoipa::path]` attribute
//! is the single source of truth for both its axum route and its OpenAPI path,
//! registered via `.routes(routes!(handlers::foo))` in `router_with_opts`. This
//! struct only seeds the doc's info/components/tags; `OpenApiRouter` merges in
//! the actual paths at router-build time.

use utoipa::OpenApi;

use crate::split_timestamps::{
    SplitStartResponse, SplitStartStatus, SplitTimestampsResponse, TimestampPayload,
    TimestampPayloadSong,
};
use crate::web::handlers;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "concert-tracker JSON API",
        version = "0.1.0",
        description = "JSON endpoints for playlists, playback, and split-timestamp \
                        editing. The rest of the app is htmx-rendered HTML and is \
                        not represented here."
    ),
    components(schemas(
        handlers::PrepareStatus,
        handlers::MediaInfo,
        handlers::PlaybackItemJson,
        handlers::ConcertPlaybackResponse,
        handlers::TrackDetailsResponse,
        crate::model::TrackDetailItem,
        SplitTimestampsResponse,
        SplitStartStatus,
        SplitStartResponse,
        handlers::PlaylistJson,
        handlers::MembershipJson,
        handlers::ResolvedTrackJson,
        handlers::PlaylistSummaryJson,
        handlers::PlaylistListEntry,
        handlers::PlaylistItemJson,
        handlers::PlaylistDetailJson,
        handlers::CreatePlaylistReq,
        handlers::UpdatePlaylistReq,
        handlers::AddItemReq,
        handlers::ReorderReq,
        handlers::CreatedPlaylistJson,
        handlers::CreatedItemJson,
        TimestampPayload,
        TimestampPayloadSong,
        concert_types::SongTimestamp,
    )),
    tags(
        (name = "playlists", description = "Playlist CRUD and membership"),
        (name = "playback", description = "Concert/track media and playback info"),
        (name = "splitting", description = "Split timestamps and split-job status"),
    ),
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    /// The set of JSON paths we expect to be documented. Hardcoded rather than
    /// derived from the router so this test actually catches the "added a JSON
    /// handler, forgot to wire its `.routes(routes!(...))` into `api_router`"
    /// mistake — which would otherwise drop the route entirely, not just its
    /// docs. One-directional: adding a new JSON route also requires adding a
    /// line here.
    const EXPECTED_PATHS: &[&str] = &[
        "/concerts/{id}/prepare-status",
        "/concerts/{id}/prepare",
        "/concerts/{id}/concert-playback",
        "/concerts/{id}/track-details",
        "/concerts/{id}/media-info",
        "/concerts/{id}/tracks/{idx}/media-info",
        "/concerts/{id}/tracks/{idx}/next-media-info",
        "/concerts/{id}/tracks/{idx}/prev-media-info",
        "/concerts/{id}/split-timestamps",
        "/concerts/{id}/split-timestamps/reset",
        "/api/playlists",
        "/api/playlists/{id}",
        "/api/playlists/{id}/items",
        "/api/playlists/{id}/items/{item_id}",
        "/api/playlists/{id}/items/reorder",
        "/api/playlists/{id}/nested-in",
        "/api/concerts/{id}/playlists",
        "/api/concerts/{id}/tracks/{idx}/playlists",
    ];

    #[test]
    fn builds_and_serializes_without_panicking() {
        let doc = ApiDoc::openapi();
        let json = doc.to_json().expect("OpenAPI doc should serialize");
        assert!(json.contains("\"openapi\":\"3.1"));
    }

    #[test]
    fn documents_all_expected_json_paths() {
        // Paths aren't in `ApiDoc::openapi()` itself (see module doc comment) —
        // they're contributed by `routes!` when the router is actually built.
        let doc = crate::web::built_api_doc();
        let documented: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
        for expected in EXPECTED_PATHS {
            assert!(
                documented.contains(expected),
                "expected path {expected:?} missing from ApiDoc; documented: {documented:?}"
            );
        }
    }

    #[test]
    fn documents_representative_schemas() {
        let doc = ApiDoc::openapi();
        let components = doc.components.expect("components should be present");
        for name in ["PlaylistDetailJson", "MediaInfo"] {
            assert!(
                components.schemas.contains_key(name),
                "expected schema {name:?} missing from ApiDoc components"
            );
        }
    }
}

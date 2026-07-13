//! Test Control HTTP Adapter — translates concise `POST /test/...` Hurl
//! requests into in-process JSON-RPC calls against the Test Control API's
//! [`Methods`], and passes everything else through to jsonrpsee's own HTTP
//! service unchanged. See docs/adr/0004-test-control-http-adapter.md and
//! docs/change/2026-07-13-test-control-http-adapter-spec.md for the full
//! route/translation/error contract this implements.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http_body_util::{BodyExt, Limited};
use jsonrpsee::core::{BoxError, TEN_MB_SIZE_BYTES};
use jsonrpsee::server::{HttpBody, HttpRequest, HttpResponse, Methods};
use serde_json::{json, Value};
use tower::{Layer, Service};

/// Bound on adapter request bodies. `Methods::raw_json_request` (the
/// in-process dispatch used below) applies no request-size limit of its own —
/// unlike jsonrpsee's normal HTTP path, which enforces
/// `ServerBuilder::max_request_body_size` before a request ever reaches a
/// method. The adapter owns this bound itself so that protection isn't
/// silently lost. Matches jsonrpsee's own default limit.
const MAX_ADAPTER_BODY_BYTES: u32 = TEN_MB_SIZE_BYTES;

/// JSON-RPC id used for every adapter-generated call. Spec: "All
/// adapter-generated requests use JSON-RPC id `"default"`."
const ADAPTER_ID: &str = "default";

/// Capacity of the subscription channel `Methods::raw_json_request` always
/// allocates internally. Test Control has no subscription methods, so
/// nothing is ever sent on it — this is not a response-size or body-size
/// limit, just a channel buffer that must be nonzero.
const SUBSCRIPTION_BUF_SIZE: usize = 1;

/// A parsed adapter route: which JSON-RPC method a `POST /test/...` request
/// maps to. See the Route Contract table in
/// docs/change/2026-07-13-test-control-http-adapter-spec.md.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdapterRoute {
    Reset,
    Seed(String),
    Assert(String),
}

impl AdapterRoute {
    fn method_name(&self) -> String {
        match self {
            AdapterRoute::Reset => "test.reset".to_string(),
            AdapterRoute::Seed(name) => format!("test.seed_{name}"),
            AdapterRoute::Assert(name) => format!("test.assert_{name}"),
        }
    }
}

/// Matches `POST /test/reset`, `POST /test/seed/{name}`, and
/// `POST /test/assert/{name}` — exactly one path segment for `{name}`. Any
/// other method or path (including a non-`POST` method, an extra segment
/// like `/test/seed/foo/bar`, or an empty `{name}`) returns `None`, which the
/// caller turns into an ordinary HTTP 404. `{name}` itself is forwarded
/// verbatim with no further validation — spec: "No extra token validation is
/// required beyond using one path segment."
fn route_for(method: &http::Method, path: &str) -> Option<AdapterRoute> {
    if method != http::Method::POST {
        return None;
    }
    let mut segments = path.trim_start_matches('/').split('/');
    match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (Some("test"), Some("reset"), None, None) => Some(AdapterRoute::Reset),
        (Some("test"), Some("seed"), Some(name), None) if !name.is_empty() => {
            Some(AdapterRoute::Seed(name.to_string()))
        }
        (Some("test"), Some("assert"), Some(name), None) if !name.is_empty() => {
            Some(AdapterRoute::Assert(name.to_string()))
        }
        _ => None,
    }
}

/// Translates an adapter request body into a full JSON-RPC request string
/// ready for [`Methods::raw_json_request`]. Returns `Err(())` for invalid
/// JSON — the caller maps that to the adapter's HTTP 400 parse-error
/// response.
///
/// - An empty body becomes `params: {}`.
/// - Any other valid JSON value (including literal `null`) is preserved as
///   `params` verbatim — spec: "Literal JSON `null` is preserved as `null`;
///   only an actually empty body becomes `{}`."
/// - `/test/reset` is a no-argument JSON-RPC method; when its body is empty
///   or `{}`, `params` is omitted entirely rather than sent as `{}` — spec:
///   "translate empty body or `{}` to a no-argument-compatible JSON-RPC
///   request using whichever representation is least awkward for jsonrpsee."
fn translate(route: &AdapterRoute, body: &[u8]) -> Result<String, ()> {
    let params: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).map_err(|_| ())?
    };

    let omit_params = matches!(route, AdapterRoute::Reset) && params == json!({});

    let mut envelope = serde_json::Map::new();
    envelope.insert("jsonrpc".to_string(), json!("2.0"));
    envelope.insert("id".to_string(), json!(ADAPTER_ID));
    envelope.insert("method".to_string(), json!(route.method_name()));
    if !omit_params {
        envelope.insert("params".to_string(), params);
    }

    Ok(Value::Object(envelope).to_string())
}

/// Reads an HTTP body into memory, rejecting anything over
/// [`MAX_ADAPTER_BODY_BYTES`]. `Err` distinguishes an over-limit body (caller
/// returns HTTP 413) from any other body-stream failure (caller falls back
/// to the same HTTP 400 parse-error shape used for invalid JSON, since the
/// spec defines no separate status for an unreadable body).
async fn read_bounded_body(body: HttpBody) -> Result<Vec<u8>, BoxError> {
    let collected = Limited::new(body, MAX_ADAPTER_BODY_BYTES as usize)
        .collect()
        .await?;
    Ok(collected.to_bytes().to_vec())
}

/// Reads, translates, and dispatches one adapter request in-process through
/// `methods`, then wraps the result as an [`HttpResponse`]. Every failure
/// mode this function can detect (oversized body, invalid JSON, an internal
/// translation bug) becomes a specific HTTP response rather than an `Err`,
/// per the spec's Response And Error Contract.
async fn dispatch(methods: &Methods, route: &AdapterRoute, body: HttpBody) -> HttpResponse {
    let raw_body = match read_bounded_body(body).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                jsonrpsee::server::http::response::too_large(MAX_ADAPTER_BODY_BYTES)
            } else {
                jsonrpsee::server::http::response::malformed()
            };
        }
    };

    let envelope = match translate(route, &raw_body) {
        Ok(envelope) => envelope,
        Err(()) => return jsonrpsee::server::http::response::malformed(),
    };

    match methods
        .raw_json_request(&envelope, SUBSCRIPTION_BUF_SIZE)
        .await
    {
        Ok((response, _subscription_rx)) => {
            jsonrpsee::server::http::response::ok_response(response.get().to_string())
        }
        // `envelope` was built above from JSON we already validated, so this
        // only fires on an adapter bug, not on bad client input.
        Err(_) => jsonrpsee::server::http::response::internal_error(),
    }
}

fn not_found() -> HttpResponse {
    http::Response::builder()
        .status(http::StatusCode::NOT_FOUND)
        .body(HttpBody::empty())
        .expect("a fixed status with an empty body cannot fail to build")
}

/// Tower layer that mounts the Test Control HTTP Adapter in front of
/// jsonrpsee's own HTTP service. See the module docs.
#[derive(Clone)]
pub(super) struct TestControlAdapterLayer {
    methods: Methods,
}

impl TestControlAdapterLayer {
    pub(super) fn new(methods: Methods) -> Self {
        Self { methods }
    }
}

impl<S> Layer<S> for TestControlAdapterLayer {
    type Service = TestControlAdapterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TestControlAdapterService {
            inner,
            methods: self.methods.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct TestControlAdapterService<S> {
    inner: S,
    methods: Methods,
}

impl<S> Service<HttpRequest> for TestControlAdapterService<S>
where
    S: Service<HttpRequest, Response = HttpResponse> + Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        // Raw JSON-RPC stays available at the listener root, untouched, for
        // debugging adapter issues — spec: "Raw JSON-RPC remains available
        // at the root test-control endpoint."
        if req.uri().path() == "/" {
            let fut = self.inner.call(req);
            return Box::pin(async move { fut.await.map_err(Into::into) });
        }

        match route_for(req.method(), req.uri().path()) {
            Some(route) => {
                let methods = self.methods.clone();
                Box::pin(async move { Ok(dispatch(&methods, &route, req.into_body()).await) })
            }
            None => Box::pin(async { Ok(not_found()) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_control::{test_state, TestControlServer};
    use http::{Method, StatusCode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_methods() -> Methods {
        let conn = db_in_memory();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        TestControlServer::new(state).rpc_module().into()
    }

    fn db_in_memory() -> rusqlite::Connection {
        crate::db::connection::open_in_memory().unwrap()
    }

    async fn body_text(resp: HttpResponse) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // --- route_for: pure translation matrix (spec's required unit tests) ---

    #[test]
    fn reset_route_maps_to_test_reset() {
        assert_eq!(
            route_for(&Method::POST, "/test/reset"),
            Some(AdapterRoute::Reset)
        );
    }

    #[test]
    fn seed_route_maps_to_test_seed_name() {
        assert_eq!(
            route_for(&Method::POST, "/test/seed/listing"),
            Some(AdapterRoute::Seed("listing".to_string()))
        );
    }

    #[test]
    fn assert_route_maps_to_test_assert_name() {
        assert_eq!(
            route_for(&Method::POST, "/test/assert/concert_state"),
            Some(AdapterRoute::Assert("concert_state".to_string()))
        );
    }

    #[test]
    fn extra_path_segment_does_not_match() {
        assert_eq!(route_for(&Method::POST, "/test/seed/foo/bar"), None);
    }

    #[test]
    fn wrong_http_method_does_not_match() {
        assert_eq!(route_for(&Method::GET, "/test/reset"), None);
    }

    #[test]
    fn unmatched_prefix_does_not_match() {
        assert_eq!(route_for(&Method::POST, "/test/nope"), None);
    }

    #[test]
    fn missing_name_does_not_match() {
        assert_eq!(route_for(&Method::POST, "/test/seed/"), None);
    }

    #[test]
    fn root_path_does_not_match_adapter_routes() {
        assert_eq!(route_for(&Method::POST, "/"), None);
    }

    // --- translate: pure request-body translation (spec's required unit tests) ---

    #[test]
    fn empty_body_becomes_empty_object_params() {
        let envelope = translate(&AdapterRoute::Seed("listing".to_string()), b"").unwrap();
        let value: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(value["method"], "test.seed_listing");
        assert_eq!(value["id"], "default");
        assert_eq!(value["params"], json!({}));
    }

    #[test]
    fn literal_null_is_preserved() {
        let envelope = translate(&AdapterRoute::Seed("listing".to_string()), b"null").unwrap();
        let value: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(value["params"], Value::Null);
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert_eq!(
            translate(&AdapterRoute::Seed("listing".to_string()), b"{not json"),
            Err(())
        );
    }

    #[test]
    fn reset_with_empty_body_omits_params_entirely() {
        let envelope = translate(&AdapterRoute::Reset, b"").unwrap();
        let value: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(value["method"], "test.reset");
        assert!(
            value.get("params").is_none(),
            "reset must omit params, got {value}"
        );
    }

    #[test]
    fn reset_with_explicit_empty_object_body_omits_params_entirely() {
        let envelope = translate(&AdapterRoute::Reset, b"{}").unwrap();
        let value: Value = serde_json::from_str(&envelope).unwrap();
        assert!(value.get("params").is_none());
    }

    #[test]
    fn assert_route_preserves_params_flat() {
        let envelope = translate(
            &AdapterRoute::Assert("concert_state".to_string()),
            br#"{"id":1,"downloaded":true}"#,
        )
        .unwrap();
        let value: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(value["method"], "test.assert_concert_state");
        assert_eq!(value["params"], json!({"id": 1, "downloaded": true}));
    }

    // --- dispatch: reaches the real Test Control methods in-process ---

    #[tokio::test]
    async fn adapter_seed_call_reaches_the_real_seed_method() {
        let methods = test_methods();
        let response = dispatch(
            &methods,
            &AdapterRoute::Seed("listing".to_string()),
            HttpBody::from(r#"{"title":"Example"}"#),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["id"], "default");
        assert_eq!(body["result"]["title"], "Example");
    }

    #[tokio::test]
    async fn adapter_reset_call_reaches_the_real_reset_method() {
        let methods = test_methods();
        let response = dispatch(&methods, &AdapterRoute::Reset, HttpBody::empty()).await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["result"]["ok"], true);
    }

    #[tokio::test]
    async fn adapter_assert_call_reaches_the_real_assert_method() {
        let methods = test_methods();
        let seed = dispatch(
            &methods,
            &AdapterRoute::Seed("listing".to_string()),
            HttpBody::from("{}"),
        )
        .await;
        let seeded: Value = serde_json::from_str(&body_text(seed).await).unwrap();
        let id = seeded["result"]["id"].as_i64().unwrap();

        let response = dispatch(
            &methods,
            &AdapterRoute::Assert("concert_state".to_string()),
            HttpBody::from(format!(r#"{{"id":{id},"downloaded":false}}"#)),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["result"]["ok"], true);
    }

    #[tokio::test]
    async fn adapter_call_to_unregistered_method_name_gets_a_jsonrpc_method_error() {
        // Matches the adapter route pattern, but no `test.seed_not_registered`
        // method exists — spec: "Paths that match the adapter pattern but
        // name an unknown JSON-RPC method should be translated and let
        // JSON-RPC return its normal method error."
        let methods = test_methods();
        let response = dispatch(
            &methods,
            &AdapterRoute::Seed("not_registered".to_string()),
            HttpBody::empty(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn invalid_json_body_is_an_http_400_parse_error_with_null_id() {
        let methods = test_methods();
        let response = dispatch(
            &methods,
            &AdapterRoute::Seed("listing".to_string()),
            HttpBody::from("{not json"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["error"]["code"], -32700);
        assert_eq!(body["id"], Value::Null);
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_with_http_413() {
        let methods = test_methods();
        let oversized = vec![b' '; MAX_ADAPTER_BODY_BYTES as usize + 1];
        let response = dispatch(
            &methods,
            &AdapterRoute::Seed("listing".to_string()),
            HttpBody::from(oversized),
        )
        .await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn batch_shaped_body_is_an_ordinary_invalid_params_value_not_special_cased() {
        // The adapter has no batching support (spec: "does not support
        // JSON-RPC batching"). A JSON array body is just an ordinary params
        // value for whichever method it targets — here an invalid-params
        // JSON-RPC error, not an HTTP-level rejection.
        let methods = test_methods();
        let response = dispatch(
            &methods,
            &AdapterRoute::Seed("listing".to_string()),
            HttpBody::from("[1,2,3]"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert!(
            body.get("error").is_some(),
            "expected a JSON-RPC error, got {body}"
        );
    }

    // --- Service-level: root pass-through and 404s never touch the real dispatch ---

    #[derive(Clone)]
    struct RecordingInner {
        calls: Arc<AtomicUsize>,
    }

    impl Service<HttpRequest> for RecordingInner {
        type Response = HttpResponse;
        type Error = BoxError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: HttpRequest) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body(HttpBody::from("raw-jsonrpc-handled-it"))
                    .unwrap())
            })
        }
    }

    #[tokio::test]
    async fn root_path_passes_through_untouched_to_raw_jsonrpc() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = RecordingInner {
            calls: calls.clone(),
        };
        let mut service = TestControlAdapterLayer::new(test_methods()).layer(inner);

        let req = http::Request::builder()
            .method(Method::POST)
            .uri("/")
            .body(HttpBody::from("{}"))
            .unwrap();
        let response = service.call(req).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unmatched_path_is_404_without_reaching_raw_jsonrpc() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = RecordingInner {
            calls: calls.clone(),
        };
        let mut service = TestControlAdapterLayer::new(test_methods()).layer(inner);

        let req = http::Request::builder()
            .method(Method::POST)
            .uri("/test/nope")
            .body(HttpBody::empty())
            .unwrap();
        let response = service.call(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn matched_adapter_route_dispatches_without_reaching_raw_jsonrpc_service() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = RecordingInner {
            calls: calls.clone(),
        };
        let mut service = TestControlAdapterLayer::new(test_methods()).layer(inner);

        let req = http::Request::builder()
            .method(Method::POST)
            .uri("/test/reset")
            .body(HttpBody::empty())
            .unwrap();
        let response = service.call(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

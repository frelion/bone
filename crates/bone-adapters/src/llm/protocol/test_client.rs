//! A unit-test-only streaming transport for the protocol constructors.
//!
//! The integration contracts drive the streaming path through
//! `tests/support`, which a unit test cannot import. This client exists so a
//! protocol module can assert the request its public constructor dispatches —
//! method, URL, headers, and body — on BONE's only model-call path without a
//! network.

use std::{
    future::{self, Future},
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use rig_core::{
    http_client::{
        self, HeaderMap, HttpClientExt, LazyBody, Method, MultipartForm, Request, Response,
        StreamingResponse,
    },
    test_utils::MockStreamingClient,
    wasm_compat::WasmCompatSend,
};

/// The one dispatching request this client observed.
///
/// A protocol test asserts only the fields its own constructor is responsible
/// for, so the rest stay unread.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct CapturedRequest {
    pub(crate) method: Method,
    pub(crate) uri: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

/// A transport that records one request and replays scripted SSE bytes.
#[derive(Clone, Debug, Default)]
pub(crate) struct ScriptedStreamingClient {
    inner: MockStreamingClient,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl ScriptedStreamingClient {
    /// Serve `body` as the SSE response to every request.
    pub(crate) fn sse(body: &str) -> Self {
        Self {
            inner: MockStreamingClient {
                sse_bytes: body.as_bytes().to_vec().into(),
            },
            ..Self::default()
        }
    }

    /// The requests this transport received, in order.
    pub(crate) fn requests(&self) -> Vec<CapturedRequest> {
        match self.requests.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl HttpClientExt for ScriptedStreamingClient {
    fn send<T, U>(
        &self,
        _request: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        future::ready(Err(http_client::Error::InvalidStatusCode(
            http::StatusCode::NOT_IMPLEMENTED,
        )))
    }

    fn send_multipart<U>(
        &self,
        _request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        future::ready(Err(http_client::Error::InvalidStatusCode(
            http::StatusCode::NOT_IMPLEMENTED,
        )))
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let (parts, body) = request.into_parts();
        let body = body.into();
        let captured = CapturedRequest {
            method: parts.method.clone(),
            uri: parts.uri.to_string(),
            headers: parts.headers.clone(),
            body: body.clone(),
        };
        match self.requests.lock() {
            Ok(mut guard) => guard.push(captured),
            Err(poisoned) => poisoned.into_inner().push(captured),
        }
        self.inner.send_streaming(Request::from_parts(parts, body))
    }
}

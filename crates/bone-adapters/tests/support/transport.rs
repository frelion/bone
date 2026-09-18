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
    test_utils::{CapturedHttpRequest, MockStreamingClient},
    wasm_compat::WasmCompatSend,
};

/// Request metadata Rig's test doubles do not retain themselves.
///
/// Every integration test compiles this module separately, so a contract that
/// asserts only on the URI leaves the other fields unread in its own binary.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CapturedRequestMetadata {
    pub method: Method,
    pub uri: String,
    pub headers: HeaderMap,
}

/// The shared transport for BONE's provider contracts.
///
/// Streaming is the only model-call mode, so this double serves the streaming
/// path only: it records method metadata and request bodies while replaying one
/// scripted SSE fixture, which lets a protocol constructor exercise its generic
/// custom-transport path. A unary call is a test error, not an empty success.
#[derive(Clone, Debug, Default)]
pub struct ScriptedHttpClient {
    streaming: MockStreamingClient,
    metadata: Arc<Mutex<Vec<CapturedRequestMetadata>>>,
    streaming_requests: Arc<Mutex<Vec<CapturedHttpRequest>>>,
}

impl ScriptedHttpClient {
    /// Serve `body` as the SSE response to every streaming request.
    pub fn sse(body: impl AsRef<[u8]>) -> Self {
        Self {
            streaming: MockStreamingClient {
                sse_bytes: body.as_ref().to_vec().into(),
            },
            ..Self::default()
        }
    }

    pub fn requests(&self) -> Vec<CapturedRequestMetadata> {
        match self.metadata.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    // Each integration test compiles this shared support module separately;
    // only contracts that inspect streaming wire bodies call this accessor.
    #[allow(dead_code)]
    pub fn streaming_requests(&self) -> Vec<CapturedHttpRequest> {
        match self.streaming_requests.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn record<T>(&self, request: &Request<T>) {
        let captured = CapturedRequestMetadata {
            method: request.method().clone(),
            uri: request.uri().to_string(),
            headers: request.headers().clone(),
        };
        match self.metadata.lock() {
            Ok(mut guard) => guard.push(captured),
            Err(poisoned) => poisoned.into_inner().push(captured),
        }
    }
}

impl HttpClientExt for ScriptedHttpClient {
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
        self.record(&request);
        let (parts, body) = request.into_parts();
        let body = body.into();
        let captured = CapturedHttpRequest {
            uri: parts.uri.to_string(),
            headers: parts.headers.clone(),
            body: body.clone(),
        };
        match self.streaming_requests.lock() {
            Ok(mut guard) => guard.push(captured),
            Err(poisoned) => poisoned.into_inner().push(captured),
        }
        self.streaming
            .send_streaming(Request::from_parts(parts, body))
    }
}

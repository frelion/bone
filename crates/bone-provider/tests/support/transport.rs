use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use bone_provider::rig::{
    http_client::{
        self, HeaderMap, HttpClientExt, LazyBody, Method, MultipartForm, Request, Response,
        StreamingResponse,
    },
    test_utils::{CapturedHttpRequest, MockStreamingClient, RecordingHttpClient},
    wasm_compat::WasmCompatSend,
};
use bytes::Bytes;

/// Request metadata Rig's test doubles do not retain themselves.
#[derive(Clone, Debug)]
pub struct CapturedRequestMetadata {
    pub method: Method,
    pub uri: String,
    pub headers: HeaderMap,
}

/// A small integration-test transport composed from Rig's supported doubles.
///
/// Unary request bodies are captured by [`RecordingHttpClient`]. This wrapper
/// also records method metadata and streaming request bodies while letting the
/// same concrete type serve SSE fixtures, so protocol constructors exercise
/// their generic custom-transport path.
#[derive(Clone, Debug, Default)]
pub struct ScriptedHttpClient {
    unary: RecordingHttpClient,
    streaming: MockStreamingClient,
    metadata: Arc<Mutex<Vec<CapturedRequestMetadata>>>,
    streaming_requests: Arc<Mutex<Vec<CapturedHttpRequest>>>,
}

impl ScriptedHttpClient {
    pub fn unary_json(body: &'static str) -> Self {
        Self {
            unary: RecordingHttpClient::new(body),
            ..Self::default()
        }
    }

    pub fn unary_error(status: &'static str, body: &'static str) -> Self {
        Self {
            unary: RecordingHttpClient::with_error_response(
                status.parse().expect("test status must be valid"),
                body,
            ),
            ..Self::default()
        }
    }

    pub fn sse(body: &'static str) -> Self {
        Self {
            streaming: MockStreamingClient {
                sse_bytes: body.as_bytes().to_vec().into(),
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

    pub fn unary_requests(&self) -> Vec<CapturedHttpRequest> {
        self.unary.requests()
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
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.record(&request);
        self.unary.send(request)
    }

    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.record(&request);
        self.unary.send_multipart(request)
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

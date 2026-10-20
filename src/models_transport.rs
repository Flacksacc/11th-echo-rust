//! Rig HTTP backend with an enforced response-body limit.
use bytes::Bytes;
use rig_core::{
    http_client::{
        self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
    },
    wasm_compat::WasmCompatSend,
};

const LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct LimitedClient(pub reqwest_rig::Client);

impl Default for LimitedClient {
    fn default() -> Self {
        Self(reqwest_rig::Client::new())
    }
}

impl std::fmt::Debug for LimitedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LimitedClient")
    }
}

fn transport_error(error: impl std::error::Error + Send + Sync + 'static) -> http_client::Error {
    http_client::Error::Instance(Box::new(error))
}

async fn limited<U>(
    mut response: reqwest_rig::Response,
) -> http_client::Result<Response<LazyBody<U>>>
where
    U: From<Bytes> + WasmCompatSend + 'static,
{
    if !response.status().is_success() {
        return Err(http_client::Error::InvalidStatusCode(response.status()));
    }
    if response
        .content_length()
        .is_some_and(|len| len > LIMIT as u64)
    {
        return Err(transport_error(std::io::Error::other(
            "Model response exceeded the size limit.",
        )));
    }
    let mut output = Response::builder().status(response.status());
    if let Some(headers) = output.headers_mut() {
        *headers = response.headers().clone();
    }
    let body: LazyBody<U> = Box::pin(async move {
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if data.len().saturating_add(chunk.len()) > LIMIT {
                return Err(transport_error(std::io::Error::other(
                    "Model response exceeded the size limit.",
                )));
            }
            data.extend_from_slice(&chunk);
        }
        Ok(U::from(Bytes::from(data)))
    });
    output.body(body).map_err(http_client::Error::Protocol)
}

impl HttpClientExt for LimitedClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        let client = self.0.clone();
        async move {
            let response = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(transport_error)?;
            limited(response).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let (parts, body) = req.into_parts();
        let client = self.0.clone();
        async move {
            let form = reqwest_rig::multipart::Form::from(body);
            let response = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .multipart(form)
                .send()
                .await
                .map_err(transport_error)?;
            limited(response).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        self.0.send_streaming(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn rejects_large_response_before_decoding() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", LIMIT + 1).as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = LimitedClient(reqwest_rig::Client::new());
        let req = Request::get(format!("http://127.0.0.1:{port}/test"))
            .body(Bytes::new())
            .unwrap();
        let response = <LimitedClient as HttpClientExt>::send::<_, Vec<u8>>(&client, req).await;
        assert!(response.is_err());
        server.await.unwrap();
    }
}

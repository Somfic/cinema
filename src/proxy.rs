use axum::{
    body::Body,
    extract::{Request, State},
    http::header,
    response::{IntoResponse, Response},
};

const HOP_BY_HOP: &[header::HeaderName] =
    &[header::HOST, header::CONNECTION, header::TRANSFER_ENCODING];

#[derive(Clone)]
pub struct DevProxy {
    client: reqwest::Client,
    target: String,
}

impl DevProxy {
    pub fn new(port: u16) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            target: format!("http://localhost:{}", port),
        }
    }
}

pub async fn dev_proxy_handler(State(proxy): State<DevProxy>, req: Request) -> impl IntoResponse {
    let uri = req.uri().to_string();
    let url = format!("{}{}", proxy.target, uri);
    tracing::trace!("dev proxy: {uri} → {url}");

    let mut builder = proxy.client.request(req.method().clone(), &url);
    for (key, val) in req.headers() {
        if !HOP_BY_HOP.contains(key) {
            builder = builder.header(key, val);
        }
    }

    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp.bytes().await.unwrap_or_default();
            let mut response = Response::builder().status(status.as_u16());
            for (k, v) in &headers {
                if !HOP_BY_HOP.contains(k) {
                    response = response.header(k, v);
                }
            }
            response.body(Body::from(body)).unwrap_or_default()
        }
        Err(_) => Response::builder()
            .status(502)
            .body(Body::from("Dev server not running"))
            .unwrap_or_default(),
    }
}

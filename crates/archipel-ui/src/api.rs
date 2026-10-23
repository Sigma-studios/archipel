//! Calls to the archipel API. The session token lives in localStorage and goes in an
//! `Authorization` header; a 401 sends the user back to the login page.

use archipel_core::api::ErrorBody;
use futures_util::StreamExt;
use gloo_net::http::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;

const TOKEN_KEY: &str = "archipel.token";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn token() -> Option<String> {
    storage()?.get_item(TOKEN_KEY).ok()?
}

pub fn set_token(token: Option<&str>) {
    if let Some(storage) = storage() {
        let _ = match token {
            Some(t) => storage.set_item(TOKEN_KEY, t),
            None => storage.remove_item(TOKEN_KEY),
        };
    }
}

fn go_to_login() {
    set_token(None);
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href("/login");
    }
}

async fn call(
    method: Method,
    path: &str,
    json: Option<String>,
    abort: Option<&web_sys::AbortSignal>,
) -> Result<Response, String> {
    let mut builder = RequestBuilder::new(path).method(method);
    if let Some(token) = token() {
        builder = builder.header("Authorization", &format!("Bearer {token}"));
    }
    builder = builder.abort_signal(abort);
    let request = match json {
        None => builder.build(),
        Some(json) => builder
            .header("Content-Type", "application/json")
            .body(json),
    }
    .map_err(|e| e.to_string())?;
    let response = request
        .send()
        .await
        .map_err(|_| "Cannot reach archipel. Is it running?".to_string())?;
    if response.ok() {
        return Ok(response);
    }
    if response.status() == 401 && !path.ends_with("/login") {
        go_to_login();
        return Err("Your session has expired, please sign in again.".into());
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err(serde_json::from_str::<ErrorBody>(&text)
        .map(|e| e.error)
        .unwrap_or_else(|_| format!("Something went wrong (error {status}).")))
}

fn json_body<B: Serialize>(body: &B) -> Option<String> {
    Some(serde_json::to_string(body).expect("serializable"))
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, String> {
    response.json().await.map_err(|e| e.to_string())
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    decode(call(Method::GET, path, None, None).await?).await
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    decode(call(Method::POST, path, json_body(body), None).await?).await
}

/// POST, ignoring the response (204).
pub async fn post_empty<B: Serialize>(path: &str, body: &B) -> Result<(), String> {
    call(Method::POST, path, json_body(body), None)
        .await
        .map(|_| ())
}

/// POST without a body, ignoring the response.
pub async fn act(path: &str) -> Result<(), String> {
    call(Method::POST, path, None, None).await.map(|_| ())
}

/// POST without a body.
pub async fn act_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    decode(call(Method::POST, path, None, None).await?).await
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, String> {
    decode(call(Method::PUT, path, json_body(body), None).await?).await
}

pub async fn put_empty<B: Serialize>(path: &str, body: &B) -> Result<(), String> {
    call(Method::PUT, path, json_body(body), None)
        .await
        .map(|_| ())
}

pub async fn delete(path: &str) -> Result<(), String> {
    call(Method::DELETE, path, None, None).await.map(|_| ())
}

/// Streams a text response chunk by chunk until it ends or `abort` fires.
pub async fn stream_text(
    path: &str,
    abort: &web_sys::AbortSignal,
    mut on_chunk: impl FnMut(String),
) -> Result<(), String> {
    let response = call(Method::GET, path, None, Some(abort)).await?;
    let body = response.body().ok_or("empty response")?;
    let mut stream = wasm_streams::ReadableStream::from_raw(body.unchecked_into()).into_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "The console connection was interrupted.".to_string())?;
        let bytes = js_sys::Uint8Array::new(&chunk).to_vec();
        on_chunk(String::from_utf8_lossy(&bytes).into_owned());
    }
    Ok(())
}

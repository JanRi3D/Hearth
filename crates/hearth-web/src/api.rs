//! Talking to the Hearth this page was served from. The admin UI uses the same documented
//! `/api/v1` endpoints as any other client.

use std::cell::RefCell;

use gloo_net::http::{Request, RequestBuilder};
use hearth_api::{API_PREFIX, ErrorBody, ErrorCode, SETUP_CODE_HEADER, WEB_UI_HEADER};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast, closure::Closure};

thread_local! {
    /// Set by the setup wizard and sent with every request while it runs.
    static SETUP_CODE: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn set_setup_code(code: &str) {
    SETUP_CODE.with(|slot| *slot.borrow_mut() = code.trim().to_string());
}

#[derive(Debug, Clone)]
pub struct ApiErr {
    pub status: u16,
    pub code: ErrorCode,
    pub message: String,
}

fn unreachable<E>(_: E) -> ApiErr {
    ApiErr { status: 0, code: ErrorCode::Unknown, message: "The Hearth did not answer. Check that it is running, then try again.".into() }
}

/// Reads what the Hearth sent back: the answer, or its own words about what went wrong.
fn answer<T: DeserializeOwned>(status: u16, text: &str) -> Result<T, ApiErr> {
    if (200..300).contains(&status) {
        // An empty answer reads as JSON null, which is what `()` expects.
        return serde_json::from_str(if text.is_empty() { "null" } else { text }).map_err(|err| ApiErr {
            status,
            code: ErrorCode::Internal,
            message: format!("The Hearth sent an answer this page does not understand: {err}"),
        });
    }
    Err(match serde_json::from_str::<ErrorBody>(text) {
        Ok(body) => ApiErr { status, code: body.error.code, message: body.error.message },
        Err(_) => ApiErr { status, code: ErrorCode::Unknown, message: format!("The Hearth answered with status {status}.") },
    })
}

async fn send<T: DeserializeOwned>(builder: RequestBuilder, body: Option<String>) -> Result<T, ApiErr> {
    let mut builder = builder.header(WEB_UI_HEADER, "1");
    let code = SETUP_CODE.with(|slot| slot.borrow().clone());
    if !code.is_empty() {
        builder = builder.header(SETUP_CODE_HEADER, &code);
    }
    let request = match body {
        Some(json) => builder.header("content-type", "application/json").body(json),
        None => builder.build(),
    }
    .map_err(unreachable)?;
    let response = request.send().await.map_err(unreachable)?;
    let status = response.status();
    answer(status, &response.text().await.map_err(unreachable)?)
}

fn url(path: &str) -> String {
    format!("{API_PREFIX}{path}")
}

fn json(body: &impl Serialize) -> Option<String> {
    serde_json::to_string(body).ok().filter(|json| json != "null")
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, ApiErr> {
    send(Request::get(&url(path)), None).await
}

pub async fn post<T: DeserializeOwned>(path: &str, body: &impl Serialize) -> Result<T, ApiErr> {
    send(Request::post(&url(path)), json(body)).await
}

pub async fn patch<T: DeserializeOwned>(path: &str, body: &impl Serialize) -> Result<T, ApiErr> {
    send(Request::patch(&url(path)), json(body)).await
}

pub async fn delete<T: DeserializeOwned>(path: &str) -> Result<T, ApiErr> {
    send(Request::delete(&url(path)), None).await
}

/// Sends a file as the body of a PUT. `on_progress` hears how much of it has been sent, in
/// percent. `fetch` cannot tell, so this one request is an XMLHttpRequest.
pub async fn upload(path: &str, file: &web_sys::File, on_progress: impl Fn(u32) + 'static) -> Result<(), ApiErr> {
    let request = web_sys::XmlHttpRequest::new().map_err(unreachable)?;
    request.open("PUT", &url(path)).map_err(unreachable)?;
    request.set_request_header(WEB_UI_HEADER, "1").map_err(unreachable)?;
    // Lives until this function returns, which is after the request is over.
    let progress = Closure::<dyn Fn(web_sys::ProgressEvent)>::new(move |event: web_sys::ProgressEvent| {
        if event.total() > 0.0 {
            on_progress((event.loaded() / event.total() * 100.0) as u32);
        }
    });
    request.upload().map_err(unreachable)?.set_onprogress(Some(progress.as_ref().unchecked_ref()));
    // Settles when the request is over, however it ended.
    let over = js_sys::Promise::new(&mut |resolve, _| request.set_onloadend(Some(&resolve)));
    request.send_with_opt_blob(Some(file)).map_err(unreachable)?;
    let _ = wasm_bindgen_futures::JsFuture::from(over).await;
    match request.status().unwrap_or(0) {
        0 => Err(unreachable(())),
        status => answer(status, &request.response_text().ok().flatten().unwrap_or_default()),
    }
}

/// For a path that goes into a query string.
pub fn encode(text: &str) -> String {
    js_sys::encode_uri_component(text).into()
}

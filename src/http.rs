/*
 * Copyright 2024 Oxide Computer Company
 */

use std::{result::Result as SResult, sync::Arc, time::Duration};

use ::image::{imageops::FilterType, Rgb};
use anyhow::{anyhow, bail, Result};
use dropshot::{
    endpoint, HttpError, HttpResponseUpdatedNoContent, RequestContext,
    TypedBody, UntypedBody,
};
use hyper::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use slog::info;

use crate::App;

#[derive(Deserialize, JsonSchema)]
struct Message {
    rgb: [u8; 3],
    text: String,
    height: u32,
    flash: Option<u32>,
}

impl From<Message> for crate::Message {
    fn from(value: Message) -> Self {
        crate::Message {
            rgb: Rgb([value.rgb[0], value.rgb[1], value.rgb[2]]),
            text: value.text,
            height: value.height,
            flash: value
                .flash
                .map(|msec| Duration::from_millis(msec.try_into().unwrap())),
        }
    }
}

#[endpoint {
    method = POST,
    path = "/clear",
}]
async fn clear(
    rc: RequestContext<Arc<App>>,
) -> SResult<HttpResponseUpdatedNoContent, HttpError> {
    let app = rc.context();

    let mut i = app.inner.lock().unwrap();

    i.msg = None;
    i.image = None;

    Ok(HttpResponseUpdatedNoContent())
}

#[endpoint {
    method = POST,
    path = "/message",
}]
async fn message(
    rc: RequestContext<Arc<App>>,
    body: TypedBody<Message>,
) -> SResult<HttpResponseUpdatedNoContent, HttpError> {
    let app = rc.context();
    let b = body.into_inner();

    let mut i = app.inner.lock().unwrap();

    i.msg = Some(b.into());

    Ok(HttpResponseUpdatedNoContent())
}

#[endpoint {
    method = POST,
    path = "/image",
}]
async fn image(
    rc: RequestContext<Arc<App>>,
    body: UntypedBody,
) -> SResult<HttpResponseUpdatedNoContent, HttpError> {
    let app = rc.context();
    let log = &rc.log;

    match ::image::load_from_memory(body.as_bytes()) {
        Ok(img) => {
            let mut i = app.inner.lock().unwrap();

            info!(
                log,
                "original image size = {} x {}",
                img.width(),
                img.height()
            );

            let img =
                img.resize(i.width, i.height, FilterType::Gaussian).to_rgb8();

            info!(log, "resized image = {} x {}", img.width(), img.height());

            i.image = Some(img);

            Ok(HttpResponseUpdatedNoContent())
        }
        Err(e) => Err(HttpError::for_client_error(
            None,
            StatusCode::BAD_REQUEST,
            format!("image problem: {e}"),
        )),
    }
}

pub(crate) async fn server(
    app: Arc<App>,
    bind_address: std::net::SocketAddr,
) -> Result<()> {
    let cd = dropshot::ConfigDropshot {
        bind_address,
        request_body_max_bytes: 32 * 1024 * 1024,
        ..Default::default()
    };

    let mut api = dropshot::ApiDescription::new();
    api.register(message).unwrap();
    api.register(clear).unwrap();
    api.register(image).unwrap();

    let log = app.log.clone();
    let s = dropshot::HttpServerStarter::new(&cd, api, app, &log)
        .map_err(|e| anyhow!("server starter error: {:?}", e))?;

    s.start().await.map_err(|e| anyhow!("HTTP server failure: {}", e))?;
    bail!("HTTP server exited unexpectedly");
}

use std::{sync::LazyLock, time::Duration};

use axum::{body::Body, extract::{ws::{CloseFrame, Message, WebSocket}, Request}, response::{IntoResponse, Response}};
use futures_util::{SinkExt as _, StreamExt};
use http::HeaderValue;
use log::{error, info};
use reqwest::Client;
use tokio::{select, time::timeout};
use tokio_tungstenite::{connect_async, tungstenite::{protocol::CloseFrame as TCloseFrame, ClientRequestBuilder, Message as TMessage}};
use anyhow::Result;

use crate::url::Url;

fn change_msg_to_axum(msg: TMessage) -> Message {
    match msg {
        TMessage::Text(t) => Message::Text(t),
        TMessage::Binary(vec) => Message::Binary(vec),
        TMessage::Ping(vec) => Message::Ping(vec),
        TMessage::Pong(vec) => Message::Pong(vec),
        TMessage::Close(close_frame) => {
            match close_frame {
                Some(f) => Message::Close(Some(CloseFrame{code: f.code.into(), reason: f.reason})),
                None => Message::Close(None),
            }
        }
        TMessage::Frame(_) => {
            panic!("bad message");
        },
    }
}

fn change_msg_to_tungstenite(msg: Message) -> TMessage {
    match msg {
        Message::Text(t) => TMessage::Text(t),
        Message::Binary(vec) => TMessage::Binary(vec),
        Message::Ping(vec) => TMessage::Ping(vec),
        Message::Pong(vec) => TMessage::Pong(vec),
        Message::Close(close_frame) => {
            match close_frame {
                Some(f) => TMessage::Close(Some(TCloseFrame{code: f.code.into(), reason: f.reason})),
                None => TMessage::Close(None)
            }
        }
    }
}

pub async fn ws_proxy(mut socket: WebSocket, req: Request<Body>) {
    info!("ws_proxy -> {}", req.uri().to_string());
    let mut builder = ClientRequestBuilder::new(req.uri().clone());
    for (name, value) in req.headers() {
        if name == http::header::HOST {
            let mut host = match req.uri().host() {
                Some(host) => {
                    let mut result = String::with_capacity(50);
                    result.push_str(host);
                    result
                },
                None => {
                    log::error!("ws_proxy failed: host is none");
                    return;
                },
            };
            if let Some(port) = req.uri().port() {
                host.push(':');
                host.push_str(port.as_str());
            }
            builder = builder.with_header(name.as_str(), host);
        } else {
            match value.to_str() {
                Ok(value) => {
                    builder = builder.with_header(name.as_str(), value);
                },
                Err(err) => {
                    error!("skip req.header.value to_str failed: {err}, key={name} value={value:?}");
                },
            }
        }
    }

    let (mut up, _) = match connect_async(builder).await {
        Ok(v) => v,
        Err(err) => {
            error!("connect websocket failed, {err}");
            return;
        },
    };
    loop {
        select! {
            msg = up.next() => {
                match msg {
                    Some(msg) => {
                        match msg {
                            Ok(msg) => {
                                let r = socket.send(change_msg_to_axum(msg)).await;
                                match r {
                                    Ok(_) => (),
                                    Err(e) => {
                                        error!("websocket send error: {e}");
                                        break;
                                    },
                                }
                            },
                            Err(_) => {
                                break;
                            }
                        }
                    },
                    None => {
                        break;
                    },
                }
            },
            msg = socket.recv() => {
                match msg {
                    Some(msg) => {
                        match msg {
                            Ok(msg) => {
                                let r = up.send(change_msg_to_tungstenite(msg)).await;
                                match r {
                                    Ok(_) => (),
                                    Err(_) => {break;},
                                }
                            },
                            Err(_) => {
                                break;
                            }
                        }
                    },
                    None => {
                        break;
                    },
                }
            },
        }
    }
}

static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .zstd(true)
        .deflate(true)
        .brotli(true)
        .gzip(true)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .build().unwrap()
});

pub async fn http_proxy(req: Request<Body>) -> Result<Response<Body>> {
    let url = Url::parse(&req.uri().to_string())?;
    let mut headers = req.headers().clone();

    if headers.contains_key(http::header::HOST) {
        headers.remove(http::header::HOST);
        headers.append(http::header::HOST, HeaderValue::from_str(&url.host)?);
    }
    if let Some(referer) = headers.get_mut(http::header::REFERER) {
        let mut referer = Url::parse(referer.to_str()?)?;
        referer.host = url.host.clone();
        headers.remove(http::header::REFERER);
        headers.append(http::header::REFERER, HeaderValue::from_str(&referer.to_str())?);
    }
    if headers.contains_key(http::header::ORIGIN) {
        headers.remove(http::header::ORIGIN);
        headers.append(http::header::ORIGIN, HeaderValue::from_str(&url.to_str_without_path())?);
    }
    info!("http_proxy -> {} {headers:?}", req.uri().to_string());

    let method = req.method().clone();

    let resp = timeout(
        Duration::from_secs(30),
        HTTP_CLIENT.request(method.clone(), url.to_str())
            .headers(headers)
            .body(reqwest::Body::wrap_stream(req.into_body().into_data_stream()))
            .send()
    ).await??;

    let status = resp.status();
    let mut header = resp.headers().clone();
    header.remove(http::header::CONTENT_LENGTH);
    header.remove(http::header::CONNECTION);
    header.remove(http::header::TRANSFER_ENCODING);
    let stream = resp.bytes_stream();
    let body = Body::from_stream(stream);
    Ok((status, header, body).into_response())
}

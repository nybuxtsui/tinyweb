use std::{borrow::Cow, net::IpAddr, path::PathBuf, str::FromStr as _};

use axum::{body::Body, extract::{Request, WebSocketUpgrade}, response::{IntoResponse as _, Redirect, Response}};
use http::{uri::{Authority, Scheme}, HeaderMap, HeaderValue, StatusCode, Uri};
use log::{debug, error, info};
use tokio::io::AsyncReadExt as _;
use tower::ServiceExt as _;
use tower_http::services::ServeDir;
use anyhow::{anyhow, Result};
use wol::MacAddr;

use crate::{fetch_from_cache, proxy, url::Url};

pub struct RequestContext {
    pub ws: Option<WebSocketUpgrade>,
    pub req: Request<Body>,
    pub cache: u64,
}

impl RequestContext {
    fn update_req_scheme_host_and_port(&mut self, scheme: &str, host_and_port: &str) -> anyhow::Result<()> {
        let mut parts = self.req.uri().clone().into_parts();
        parts.scheme = Some(Scheme::from_str(scheme)?);
        match parts.authority {
            Some(authority) => {
                let str_auth = authority.as_str();
                let temp: String;
                parts.authority = Some(Authority::from_str(match str_auth.rfind('@') {
                    Some(pos) => {
                        let user_and_pass = &str_auth[0..pos];
                        temp = format!("{user_and_pass}@{host_and_port}");
                        temp.as_str()
                    }
                    None => host_and_port,
                })?);
            }
            None => {
                parts.authority = Some(Authority::from_str(host_and_port)?);
            }
        }
        *self.req.uri_mut() = http::Uri::from_parts(parts)?;
        Ok(())
    }

    pub async fn exec(&mut self, directive: (&str, &str)) -> Option<Response<Body>> {
        debug!("exec {directive:?}");
        match directive.0 {
            "redir" => self.redir(directive.1),
            "strip_prefix" => self.strip_prefix(directive.1),
            "rewrite" => self.rewrite(directive.1),
            "cache" => self.cache(directive.1),
            "reverse_proxy" => self.reverse_proxy(directive.1).await,
            "file_server" => self.file_server(directive.1).await,
            "wol" => self.wol(directive.1),
            _ => { Some((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    HeaderMap::new(),
                    format!("unknown directive: {}", directive.0),
                ).into_response()) },
        }
    }

    fn redir(&mut self, param: &str) -> Option<Response<Body>> {
        Some(Redirect::permanent(param).into_response())
    }


    fn modify_path<'a, F>(&mut self, param: &'a str, f: F) -> anyhow::Result<()>
        where F: for<'b> FnOnce(&'b str, &'b str) -> Option<Cow<'b, str>>, {
        let path_and_query = match self.req.uri().path_and_query() {
            None => return Ok(()),
            Some(path_and_query) => (path_and_query.path(), path_and_query.query()),
        };
        let path = match f(path_and_query.0, param) {
            None => return Ok(()),
            Some(s) => {
                if s.is_empty() {
                    Cow::Borrowed("/")
                } else {
                    s
                }
            },
        };

        match path_and_query.1 {
            None => {
                *self.req.uri_mut() = match Uri::from_str(path.as_ref()) {
                    Ok(uri) => uri,
                    Err(err) => {
                        log::error!("uri invalid: {}, uri={}", err, path);
                        anyhow::bail!(err);
                    }
                }
            }
            Some(query) => {
                let mut new_path = String::with_capacity(50);
                new_path.push_str(path.as_ref());
                new_path.push('?');
                new_path.push_str(query);
                *self.req.uri_mut() = match Uri::from_str(&new_path) {
                    Ok(uri) => uri,
                    Err(err) => {
                        log::error!("uri invalid: {}, uri={}", err, path);
                        anyhow::bail!(err);
                    }
                }
            }
        }
        Ok(())
    }


    fn strip_prefix(&mut self, param: &str) -> Option<Response<Body>> {
        match self.modify_path(param, |s, _| s.strip_prefix(param).map(Into::into)) {
            Ok(_) => (),
            Err(err) => error!("update path failed: {err}"),
        }
        None
    }

    fn rewrite(&mut self, param: &str) -> Option<Response<Body>> {
        match self.modify_path(param, |_, s| Some(Cow::Borrowed(s))) {
            Ok(_) => (),
            Err(err) => error!("update path failed: {err}"),
        }
        None
    }

    fn cache(&mut self, param: &str) -> Option<Response<Body>> {
        self.cache = match param.parse() {
            Ok(u) => u,
            Err(err) => {
                error!("parse cache failed: {err}");
                0u64
            }
        };
        None
    }

    fn wol(&mut self, param: &str) -> Option<Response<Body>> {
        let mut bind_id = None;
        let args: Vec<_> = param.split_ascii_whitespace().collect();
        let mac = match args.len() {
            0 => {
                error!("wol: need argument");
                return Some((StatusCode::OK, HeaderMap::new(), "ok").into_response());
            },
            1 => {
                args[0].to_string()
            },
            2 => {
                let ip = args[1].parse::<IpAddr>();
                match ip {
                    Ok(ip) => { bind_id = Some(ip); },
                    Err(err) => {
                        error!("wol: bind_ip error: {err}");
                        return Some((StatusCode::OK, HeaderMap::new(), "ok").into_response());
                    }
                }
                args[0].to_string()
            }
            _ => {
                error!("wol: argument error: {}", param);
                return Some((StatusCode::OK, HeaderMap::new(), "ok").into_response());
            }
        };
        let mac = match MacAddr::from_str(&mac) {
            Ok(mac) => mac,
            Err(err) => {
                error!("wol: mac error: {}", err);
                return Some((StatusCode::OK, HeaderMap::new(), "ok").into_response());
            },
        };

        tokio::task::spawn_blocking(move || {
            match || -> Result<()> {
                wol::send_wol(mac, None, bind_id)?;
                Ok(())
            }() {
                Ok(_) => info!("send wol ok"),
                Err(err) => error!{"send wol failed: {err}"},
            }
        });
        Some((StatusCode::OK, HeaderMap::new(), "ok").into_response())
    }

    async fn reverse_proxy(&mut self, param: &str) -> Option<Response<Body>> {
        let old_url = self.req.uri().to_string();
        let mut url = match Url::parse(param) {
            Ok(url) => url,
            Err(err) => {
                return Some(
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        HeaderMap::new(),
                        err.to_string(),
                    )
                        .into_response(),
                );
            }
        };
        url.path = self.req.uri().path().to_string();
        let scheme = match url.scheme {
            Some(scheme) => scheme,
            None => {
                if self.ws.is_some() {
                    "ws"
                } else {
                    "http"
                }
                .to_string()
            }
        };

        self.update_req_scheme_host_and_port(&scheme, &url.host).unwrap();
        debug!("reverse_proxy {param} {old_url} => {}", self.req.uri().to_string());

        let ws = std::mem::take(&mut self.ws);
        let req = std::mem::take(&mut self.req);
        match ws {
            Some(ws) => Some(
                ws.on_upgrade(move |socket| proxy::ws_proxy(socket, req))
                    .into_response(),
            ),
            None => {
                let key = format!("rproxy:{}", req.uri());
                debug!("cache key: {key}");
                Some(
                    if self.cache == 0 {
                        match proxy::http_proxy(req).await {
                            Ok(resp) => resp,
                            Err(e) => (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                HeaderMap::new(),
                                e.to_string(),
                            ).into_response(),
                        }
                    } else {
                        fetch_from_cache(key, self.cache, || proxy::http_proxy(req)).await
                    }
                )
            }
        }
    }

    async fn file_server(&mut self, param: &str) -> Option<Response<Body>> {
        let mut pathbuf = match PathBuf::from_str(param) {
            Ok(pathbuf) => pathbuf,
            Err(err) => {
                error!("path {} error: {err}", param);
                return Some(
                    (StatusCode::NOT_FOUND, HeaderMap::new(), "not found").into_response(),
                );
            }
        };
        pathbuf.push(
            self.req
                .uri()
                .path()
                .strip_prefix("/")
                .unwrap_or(self.req.uri().path()),
        );
        if pathbuf.is_dir() {
            pathbuf.push("index.html");
        }
        if !pathbuf.exists() {
            error!("path {} notfound", pathbuf.to_string_lossy());
            return Some(
                (StatusCode::NOT_FOUND, HeaderMap::new(), "not found").into_response(),
            );
        }
        let len = pathbuf.metadata().map(|m| m.len()).unwrap_or(0);
        if self.cache == 0 || len == 0 || len > crate::CACHE_LIMIT as u64 {
            debug!("ServeFile by tower: {}", pathbuf.to_string_lossy());
            let dir = ServeDir::new(param)
                .append_index_html_on_directories(true)
                .with_buf_chunk_size(64 * 1024);
            let req = std::mem::take(&mut self.req);
            return Some(dir.oneshot(req).await.into_response());
        }

        let key = format!("file:{}", pathbuf.to_string_lossy());
        Some(fetch_from_cache(key, self.cache, || async {
            debug!("ServeFile by local: {}", pathbuf.to_string_lossy());
            let mut file = match tokio::fs::File::open(&pathbuf).await {
                Ok(file) => file,
                Err(err) => {
                    error!("path {} open failed: {err}", pathbuf.to_string_lossy());
                    return Err(anyhow!(err));
                }
            };
            let mut buf = vec![0u8; 256 * 1024];
            let len = match file.read(&mut buf).await {
                Ok(len) => len,
                Err(err) => {
                    error!("read failed: {err}");
                    return Err(anyhow!(err));
                }
            };
            buf.truncate(len);
            let content_type = mime_guess::from_path(&pathbuf)
                .first_or_octet_stream()
                .to_string();
            debug!(
                "ServeFile by local: {}, read len={} type={}",
                pathbuf.to_string_lossy(),
                len,
                content_type
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type).unwrap(),
            );
            headers.insert(
                http::header::CONTENT_LENGTH,
                HeaderValue::from_str(&len.to_string()).unwrap(),
            );
            Ok((StatusCode::OK, headers, buf).into_response())
        }).await)
    }
}
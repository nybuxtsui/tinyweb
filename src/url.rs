use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use log::error;
use regex::Regex;

static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^((?P<scheme>[^:]+)://)?(?P<host>[^/]+)(?P<path>[^#]*)").unwrap()
});

#[derive(Clone)]
pub struct Url {
    pub(crate) scheme: Option<String>,
    pub(crate) authority: Option<String>,
    pub(crate) host: String,
    pub(crate) path: String,
}

impl Url {
    pub fn parse(url: &str) -> Result<Self> {
        match RE_URL.captures(url) {
            Some(m) => {
                let scheme = m.name("scheme").map(|x|x.as_str().to_owned());
                let mut host = match m.name("host") {
                    Some(host) => { host.as_str().to_owned()},
                    None => { return Err(anyhow!("url host empty"))},
                };
                let mut authority = None;
                if host.contains("@") {
                    let s:Vec<&str> = host.split("@").collect();
                    if s.len() != 2 {
                        return Err(anyhow!("url anthority error"));
                    }
                    authority = Some(s[0].to_owned());
                    host = s[1].to_owned();
                }
                let path = m.name("path").map(|x|x.as_str()).unwrap_or("").to_owned();
                if host.is_empty() {
                    return Err(anyhow!("url host empty"));
                }
                Ok(Self{scheme, authority, host, path})
            },
            None => {
                error!("url error: {url}");
                Err(anyhow!("url error"))
            },
        }
    }

    pub fn to_str_without_path(&self) -> String {
        let mut result = String::new();
        if let Some(scheme) = &self.scheme {
            result.push_str(scheme);
            result.push_str("://");
        }
        if let Some(authority) = &self.authority {
            result.push_str(authority);
            result.push('@');
        }
        result.push_str(&self.host);
        result
    }

    pub fn to_str(&self) -> String {
        let mut result = self.to_str_without_path();
        result.push_str(&self.path);
        result
    }

}
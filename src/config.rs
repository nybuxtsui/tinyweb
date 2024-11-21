use log::{debug, warn};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Config {
    pub(crate) routes: Vec<ConfigRoute>,
    pub(crate) log: String,
    pub(crate) bind: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ConfigRoute {
    pub(crate) matcher: String,
    pub(crate) directive: Vec<String>,
}

impl ConfigRoute {
    pub fn is_match(&self, s: &str) -> bool {
        debug!("matcher params {:?}", self.matcher.split(" "));
        for item in self.matcher.trim().split(" ") {
            debug!("matcher {item} {s}");
            if item.is_empty() {
                continue;
            }
            if item.ends_with("*") {
                if item.len() == 1 {
                    return true;
                }
                let remain = &item[0..item.len() - 1];
                if remain.contains("*") {
                    warn!("matcher format error: multi *, {item}");
                    // 格式错误
                    continue;
                }
                if s.starts_with(remain) {
                    return true;
                }
            } else if item.contains("*") {
                warn!("matcher format error: mid *, {item}");
                // 格式错误
                continue;
            } else if item == s {
                return true;
            }
        }
        false
    }

    pub fn parse_directive(directive: &str) -> (&str, &str) {
        let items: Vec<&str> = directive.trim().splitn(2, " ").collect();
        if items.len() == 1 {
            (items[0], "")
        } else {
            (items[0], items[1])
        }
    }
}

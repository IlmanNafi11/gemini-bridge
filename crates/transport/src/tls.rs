use http::{HeaderMap, HeaderName, HeaderValue};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsProfile {
    #[default]
    Chrome,
    Firefox,
    Safari,
}

impl FromStr for TlsProfile {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "firefox" => Self::Firefox,
            "safari" => Self::Safari,
            _ => Self::Chrome,
        })
    }
}

impl TlsProfile {
    /// Return the User-Agent string for this profile.
    pub fn user_agent(&self) -> &'static str {
        match self {
            Self::Chrome => {
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
            }
            Self::Firefox => {
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:135.0) Gecko/20100101 Firefox/135.0"
            }
            Self::Safari => {
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_4) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/18.3 Safari/605.1.15"
            }
        }
    }

    /// Default header set matching the browser profile.
    /// Injected into outgoing requests when the caller has not explicitly set them.
    pub fn default_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let mut list = Vec::new();

        // User-Agent is common to all profiles.
        if let Ok(val) = HeaderValue::from_str(self.user_agent()) {
            list.push((http::header::USER_AGENT, val));
        }

        match self {
            Self::Chrome => {
                list.push((
                    HeaderName::from_static("sec-ch-ua"),
                    HeaderValue::from_static(
                        "\"Chromium\";v=\"134\", \"Google Chrome\";v=\"134\", \"Not:A-Brand\";v=\"24\"",
                    ),
                ));
                list.push((
                    HeaderName::from_static("sec-ch-ua-mobile"),
                    HeaderValue::from_static("?0"),
                ));
                list.push((
                    HeaderName::from_static("sec-ch-ua-platform"),
                    HeaderValue::from_static("\"Windows\""),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-dest"),
                    HeaderValue::from_static("empty"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-mode"),
                    HeaderValue::from_static("cors"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-site"),
                    HeaderValue::from_static("same-origin"),
                ));
                list.push((
                    http::header::ACCEPT_LANGUAGE,
                    HeaderValue::from_static("en-US,en;q=0.9"),
                ));
            }
            Self::Firefox => {
                list.push((
                    http::header::ACCEPT_LANGUAGE,
                    HeaderValue::from_static("en-US,en;q=0.5"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-dest"),
                    HeaderValue::from_static("empty"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-mode"),
                    HeaderValue::from_static("cors"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-site"),
                    HeaderValue::from_static("same-origin"),
                ));
            }
            Self::Safari => {
                list.push((
                    http::header::ACCEPT_LANGUAGE,
                    HeaderValue::from_static("en-US,en;q=0.9"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-dest"),
                    HeaderValue::from_static("empty"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-mode"),
                    HeaderValue::from_static("cors"),
                ));
                list.push((
                    HeaderName::from_static("sec-fetch-site"),
                    HeaderValue::from_static("same-origin"),
                ));
            }
        }

        list
    }

    /// Apply profile headers to a `HeaderMap`, without overwriting existing caller headers.
    pub fn apply_headers(&self, headers: &mut HeaderMap) {
        for (k, v) in self.default_headers() {
            if !headers.contains_key(&k) {
                headers.insert(k, v);
            }
        }
    }
}

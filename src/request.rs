use std::io::Read;
use std::sync::Arc;
use std::{fmt, time};

use url::{form_urlencoded, ParseError, Url};

use crate::body::Payload;
use crate::header::{self, Header};
use crate::unit::{self, Unit};
use crate::Response;
use crate::{agent::Agent, error::Error};

#[cfg(feature = "json")]
use super::SerdeValue;

pub type Result<T> = std::result::Result<T, Error>;

/// Parsed result of a request url with handy inspection methods.
#[derive(Clone)]
pub struct ParsedUrl {
    url_state: UrlState,
}

/// Encapsulation of the various state a url can be in.
#[derive(Clone)]
enum UrlState {
    /// String has not yet been parsed to a Url yet.
    Unparsed(Arc<String>),
    /// A successfully parsed url.
    Parsed(Url),
    /// Special state when using `Request::parsed_url().
    /// Query pairs copied out from Cow<str> in url::Url.
    ParsedAndQueryPairs(ParsedAndQueryPairs),
    /// A url that failed to parse. The string is the original we tried to parse.
    ParseError(Arc<String>, ParseError),
}

#[derive(Clone)]
struct ParsedAndQueryPairs {
    url: Url,
    query_pairs: Vec<(String, String)>,
}

impl UrlState {
    fn ensure_correct(self) -> Self {
        match self {
            UrlState::Parsed(url) => {
                // No hostname is fine for urls in general, but not for website urls.
                if url.host_str().is_none() {
                    return UrlState::ParseError(
                        Arc::new(url.as_str().to_string()),
                        ParseError::EmptyHost,
                    );
                }

                // validated ok
                UrlState::Parsed(url)
            }
            x @ _ => x,
        }
    }

    fn parse(self) -> Self {
        match self {
            UrlState::Unparsed(s) => match Url::parse(&*s) {
                Ok(url) => UrlState::Parsed(url),
                Err(err) => UrlState::ParseError(s, err),
            },
            x @ _ => x,
        }
    }

    /// Prepares the ParsedUrl to return an owned copy from `Request::parsed_url()`.
    fn cloned_with_query_pairs(&self) -> Self {
        match self.clone().parse() {
            UrlState::Parsed(url) => {
                let mut query_pairs = vec![];

                for (k, v) in url.query_pairs() {
                    query_pairs.push((k.into(), v.into()));
                }

                UrlState::ParsedAndQueryPairs(ParsedAndQueryPairs { url, query_pairs })
            }
            x @ _ => x,
        }
    }

    /// Force unwrap as ParsedAndQueryPairs.
    fn unwrap_parsed_and_query_pairs(&self) -> &ParsedAndQueryPairs {
        match self {
            UrlState::ParsedAndQueryPairs(p) => p,
            _ => panic!("as_parsed_and_query_pairs in unexpected state"),
        }
    }

    /// Append a query parameter. Causes parse() if need be.
    fn query_append(self, param: &str, value: &str) -> Self {
        let mut parsed = self.parse();
        match &mut parsed {
            UrlState::Parsed(url) => {
                url.query_pairs_mut().append_pair(param, value);
            }
            UrlState::ParseError(_, _) => {
                // ignore
            }
            _ => unreachable!(),
        }
        parsed
    }
}

impl fmt::Display for UrlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            UrlState::Unparsed(url) => write!(f, "{}", url),
            UrlState::Parsed(url) => write!(f, "{}", url.as_str()),
            UrlState::ParsedAndQueryPairs(p) => write!(f, "{}", p.url.as_str()),
            UrlState::ParseError(_, error) => write!(f, "{:?}", error),
        }
    }
}

impl fmt::Display for ParsedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url_state)
    }
}

impl From<String> for ParsedUrl {
    fn from(s: String) -> Self {
        ParsedUrl::new(UrlState::Unparsed(Arc::new(s)))
    }
}

impl From<Url> for ParsedUrl {
    fn from(url: Url) -> Self {
        ParsedUrl::new(UrlState::Parsed(url))
    }
}

impl ParsedUrl {
    fn new(url_state: UrlState) -> Self {
        let url_state = url_state.ensure_correct();

        ParsedUrl { url_state }
    }

    /// Prepares the ParsedUrl to return an owned copy from `Request::parsed_url()`.
    fn cloned_with_query_pairs(&self) -> Self {
        let url_state = self.url_state.cloned_with_query_pairs();
        ParsedUrl { url_state }
    }

    fn into_result(self) -> std::result::Result<Url, ParseError> {
        match self.url_state.parse() {
            UrlState::Parsed(url) => Ok(url),
            UrlState::ParseError(_, err) => Err(err),
            _ => unreachable!(),
        }
    }

    fn surface_error(self) -> std::result::Result<Self, ParseError> {
        match self.url_state.parse() {
            UrlState::ParseError(_, err) => Err(err),
            url_state @ _ => Ok(ParsedUrl { url_state }),
        }
    }

    fn as_str(&self) -> &str {
        match &self.url_state {
            UrlState::Unparsed(s) => s.as_str(),
            UrlState::Parsed(u) => u.as_str(),
            UrlState::ParsedAndQueryPairs(p) => p.url.as_str(),
            UrlState::ParseError(s, _) => s.as_str(),
        }
    }

    // NOTICE:
    // The unwrap() in all folllowing functions is ok because
    // we can only get a `ParsedUrl` via `Request::parsed_url()`, in
    // which case a potential error state is surfaced. In other words,
    // if the user has an instance of ParsedUrl, they are guaranteed it
    // contains an Ok() value.

    /// Handle the parsed url as a standard [`url::Url`].
    pub fn as_url(&self) -> &Url {
        &self.url_state.unwrap_parsed_and_query_pairs().url
    }

    /// Get the scheme of the parsed url, i.e. "https" or "http".
    pub fn scheme(&self) -> &str {
        self.as_url().scheme()
    }

    /// Host of the parsed url.
    pub fn host(&self) -> &str {
        // this unwrap() is ok, because ParsedUrl is tested for empty host
        // urls in ParsedUrl::new(). At this point we are guaranteed there
        // is no empty host.
        self.as_url().host_str().unwrap()
    }

    /// Port of the parsed url, if available. Ports are only available if they
    /// are present in the original url. Specifically the scheme default ports,
    /// 443 for `https` and and 80 for `http` are `None` unless explicitly
    /// set in the url, i.e. `https://my-host.com:443/some/path`.
    pub fn port(&self) -> Option<u16> {
        self.as_url().port()
    }

    /// Path of the parsed url.
    pub fn path(&self) -> &str {
        self.as_url().path()
    }

    /// Returns all query parameters as a vector of key-value pairs.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let req = ureq::get("http://httpbin.org/get")
    ///     .query("foo", "42")
    ///     .query("foo", "43");
    ///
    /// assert_eq!(req.parsed_url().unwrap().query_pairs(), vec![
    ///     ("foo", "42"),
    ///     ("foo", "43")
    /// ]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn query_pairs(&self) -> Vec<(&str, &str)> {
        let mut ret = vec![];

        let v = &self.url_state.unwrap_parsed_and_query_pairs().query_pairs;
        for (k, v) in v {
            ret.push((k.as_str(), v.as_str()));
        }

        ret
    }
}

/// Request instances are builders that creates a request.
///
/// ```
/// # fn main() -> Result<(), ureq::Error> {
/// # ureq::is_test(true);
/// let response = ureq::get("http://example.com/form")
///     .query("foo", "bar baz")  // add ?foo=bar+baz
///     .call()?;                 // run the request
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Request {
    agent: Agent,
    method: String,
    parsed_url: ParsedUrl,
    error_on_non_2xx: bool,
    headers: Vec<Header>,
    timeout: Option<time::Duration>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Request({} {}, {:?})",
            self.method, self.parsed_url, self.headers
        )
    }
}

impl Request {
    pub(crate) fn new(agent: Agent, method: String, url: String) -> Request {
        Self::_new(agent, method, url.into())
    }

    pub(crate) fn with_url(agent: Agent, method: String, url: Url) -> Request {
        Self::_new(agent, method, url.into())
    }

    fn _new(agent: Agent, method: String, parsed_url: ParsedUrl) -> Request {
        Request {
            agent,
            method,
            parsed_url,
            headers: vec![],
            error_on_non_2xx: true,
            timeout: None,
        }
    }

    #[inline(always)]
    /// Sets overall timeout for the request, overriding agent's configuration if any.
    pub fn timeout(mut self, timeout: time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sends the request with no body and blocks the caller until done.
    ///
    /// Use this with GET, HEAD, OPTIONS or TRACE. It sends neither
    /// Content-Length nor Transfer-Encoding.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::get("http://example.com/")
    ///     .call()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn call(self) -> Result<Response> {
        self.do_call(Payload::Empty)
    }

    fn do_call(self, payload: Payload) -> Result<Response> {
        for h in &self.headers {
            h.validate()?;
        }
        let url = self.parsed_url.into_result()?;

        let deadline = match self.timeout.or(self.agent.config.timeout) {
            None => None,
            Some(timeout) => {
                let now = time::Instant::now();
                Some(now.checked_add(timeout).unwrap())
            }
        };

        let reader = payload.into_read();
        let unit = Unit::new(
            &self.agent,
            &self.method,
            &url,
            &self.headers,
            &reader,
            deadline,
        );
        let response = unit::connect(unit, true, reader).map_err(|e| e.url(url.clone()))?;

        if response.status() >= 400 {
            Err(Error::Status(response.status(), response))
        } else {
            Ok(response)
        }
    }

    /// Send data a json value.
    ///
    /// Requires feature `ureq = { version = "*", features = ["json"] }`
    ///
    /// The `Content-Length` header is implicitly set to the length of the serialized value.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::post("http://httpbin.org/post")
    ///     .send_json(ureq::json!({
    ///       "name": "martin",
    ///       "rust": true,
    ///     }))?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "json")]
    pub fn send_json(mut self, data: SerdeValue) -> Result<Response> {
        if self.header("Content-Type").is_none() {
            self = self.set("Content-Type", "application/json");
        }
        self.do_call(Payload::JSON(data))
    }

    /// Send data as bytes.
    ///
    /// The `Content-Length` header is implicitly set to the length of the serialized value.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::put("http://httpbin.org/put")
    ///     .send_bytes(&[0; 1000])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_bytes(self, data: &[u8]) -> Result<Response> {
        self.do_call(Payload::Bytes(data))
    }

    /// Send data as a string.
    ///
    /// The `Content-Length` header is implicitly set to the length of the serialized value.
    /// Defaults to `utf-8`
    ///
    /// ## Charset support
    ///
    /// Requires feature `ureq = { version = "*", features = ["charset"] }`
    ///
    /// If a `Content-Type` header is present and it contains a charset specification, we
    /// attempt to encode the string using that character set. If it fails, we fall back
    /// on utf-8.
    ///
    /// ```
    /// // this example requires features = ["charset"]
    ///
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::post("http://httpbin.org/post")
    ///     .set("Content-Type", "text/plain; charset=iso-8859-1")
    ///     .send_string("Hällo Wörld!")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_string(self, data: &str) -> Result<Response> {
        let charset =
            crate::response::charset_from_content_type(self.header("content-type")).to_string();
        self.do_call(Payload::Text(data, charset))
    }

    /// Send a sequence of (key, value) pairs as form-urlencoded data.
    ///
    /// The `Content-Type` header is implicitly set to application/x-www-form-urlencoded.
    /// The `Content-Length` header is implicitly set to the length of the serialized value.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::post("http://httpbin.org/post")
    ///     .send_form(&[
    ///       ("foo", "bar"),
    ///       ("foo2", "bar2"),
    ///     ])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_form(mut self, data: &[(&str, &str)]) -> Result<Response> {
        if self.header("Content-Type").is_none() {
            self = self.set("Content-Type", "application/x-www-form-urlencoded");
        }
        let encoded = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(data)
            .finish();
        self.do_call(Payload::Bytes(&encoded.into_bytes()))
    }

    /// Send data from a reader.
    ///
    /// If no Content-Length and Transfer-Encoding header has been set, it uses the [chunked transfer encoding](https://tools.ietf.org/html/rfc7230#section-4.1).
    ///
    /// The caller may set the Content-Length header to the expected byte size of the reader if is
    /// known.
    ///
    /// The input from the reader is buffered into chunks of size 16,384, the max size of a TLS fragment.
    ///
    /// ```
    /// use std::io::Cursor;
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let read = Cursor::new(vec![0x20; 100]);
    /// let resp = ureq::post("http://httpbin.org/post")
    ///     .send(read)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send(self, reader: impl Read) -> Result<Response> {
        self.do_call(Payload::Reader(Box::new(reader)))
    }

    /// Set a header field.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::get("http://httpbin.org/bytes/1000")
    ///     .set("Accept", "text/plain")
    ///     .set("Range", "bytes=500-999")
    ///     .call()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set(mut self, header: &str, value: &str) -> Self {
        header::add_header(&mut self.headers, Header::new(header, value));
        self
    }

    /// Returns the value for a set header.
    ///
    /// ```
    /// let req = ureq::get("/my_page")
    ///     .set("X-API-Key", "foobar");
    /// assert_eq!("foobar", req.header("x-api-Key").unwrap());
    /// ```
    pub fn header(&self, name: &str) -> Option<&str> {
        header::get_header(&self.headers, name)
    }

    /// A list of the set header names in this request. Lowercased to be uniform.
    ///
    /// ```
    /// let req = ureq::get("/my_page")
    ///     .set("X-API-Key", "foobar")
    ///     .set("Content-Type", "application/json");
    /// assert_eq!(req.header_names(), vec!["x-api-key", "content-type"]);
    /// ```
    pub fn header_names(&self) -> Vec<String> {
        self.headers
            .iter()
            .map(|h| h.name().to_ascii_lowercase())
            .collect()
    }

    /// Tells if the header has been set.
    ///
    /// ```
    /// let req = ureq::get("/my_page")
    ///     .set("X-API-Key", "foobar");
    /// assert_eq!(true, req.has("x-api-Key"));
    /// ```
    pub fn has(&self, name: &str) -> bool {
        header::has_header(&self.headers, name)
    }

    /// All headers corresponding values for the give name, or empty vector.
    ///
    /// ```
    /// let req = ureq::get("/my_page")
    ///     .set("X-Forwarded-For", "1.2.3.4")
    ///     .set("X-Forwarded-For", "2.3.4.5");
    ///
    /// assert_eq!(req.all("x-forwarded-for"), vec![
    ///     "1.2.3.4",
    ///     "2.3.4.5",
    /// ]);
    /// ```
    pub fn all(&self, name: &str) -> Vec<&str> {
        header::get_all_headers(&self.headers, name)
    }

    /// Set a query parameter.
    ///
    /// For example, to set `?format=json&dest=/login`
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let resp = ureq::get("http://httpbin.org/get")
    ///     .query("format", "json")
    ///     .query("dest", "/login")
    ///     .call()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn query(mut self, param: &str, value: &str) -> Self {
        self.parsed_url.url_state = self.parsed_url.url_state.query_append(param, value);
        self
    }

    /// Returns the value of the request method. Something like `GET`, `POST`, `PUT` etc.
    ///
    /// ```
    /// let req = ureq::put("http://httpbin.org/put");
    ///
    /// assert_eq!(req.method(), "PUT");
    /// ```
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Get the parsed url that will be used for this request.
    ///
    /// The url might differ from that originally provided when constructing the
    /// request if additional query parameters have been added using [`Request::query()`].
    ///
    /// The url is wrapped in a `Result` since a common use case is to construct
    /// the [`Request`] using a `&str` in which case the url is parsed.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let req = ureq::get("http://httpbin.org/get")
    ///     .query("foo", "bar");
    ///
    /// assert_eq!(req.parsed_url().unwrap().host(), "httpbin.org");
    /// # Ok(())
    /// # }
    /// ```
    pub fn parsed_url(&self) -> Result<ParsedUrl> {
        let p = self.parsed_url.cloned_with_query_pairs();

        // If there is a parse error, surface it now.
        let p = p.surface_error()?;

        return Ok(p);
    }

    /// Get the url str that will be used for this request.
    ///
    /// The url might differ from that originally provided when constructing the
    /// request if additional query parameters have been added using [`Request::query()`].
    ///
    /// In case the original url provided to build the request is not possible to
    /// parse to a Url, this function returns the original, and it will error once the
    /// Request object is used.
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let req = ureq::get("http://httpbin.org/get")
    ///     .query("foo", "bar");
    ///
    /// assert_eq!(req.url(), "http://httpbin.org/get?foo=bar");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ```
    /// # fn main() -> Result<(), ureq::Error> {
    /// # ureq::is_test(true);
    /// let req = ureq::get("SO WRONG")
    ///     .query("foo", "bar"); // does nothing
    ///
    /// assert_eq!(req.url(), "SO WRONG");
    /// # Ok(())
    /// # }
    /// ```
    pub fn url(&self) -> &str {
        self.parsed_url.as_str()
    }
}

#[test]
fn request_implements_send_and_sync() {
    let _request: Box<dyn Send> = Box::new(Request::new(
        Agent::new(),
        "GET".to_string(),
        "https://example.com/".to_string(),
    ));
    let _request: Box<dyn Sync> = Box::new(Request::new(
        Agent::new(),
        "GET".to_string(),
        "https://example.com/".to_string(),
    ));
}

#[test]
fn send_byte_slice() {
    let bytes = vec![1, 2, 3];
    crate::agent()
        .post("http://example.com")
        .send(&bytes[1..2])
        .ok();
}

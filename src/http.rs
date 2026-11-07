//! HTTP client shared by every source: a cross-process throttle per host,
//! retries on 429/5xx/transport errors, and redirects followed hop by hop so
//! each host's throttle applies. The throttle's lock files live in a per-user
//! cache folder, so every run by the user shares them whatever its `$TMPDIR`.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ureq::http::Uri;

/// arXiv API terms: at most one request every 3 seconds (also applied to arxiv.org).
const ARXIV_INTERVAL_MS: u128 = 3_000;
/// Every other host. Crossref's public pool allows 5 requests/s with
/// concurrency 1, OpenAlex 10/s without a key; one per second stays well inside both.
const DEFAULT_INTERVAL_MS: u128 = 1_000;
pub const ATTEMPTS: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on a server-requested `Retry-After` wait.
const MAX_RETRY_AFTER_S: u64 = 300;
const MAX_REDIRECTS: u32 = 10;
/// Cap on API response bodies.
pub const API_BODY_LIMIT: u64 = 10 << 20;

pub const USER_AGENT: &str = concat!("papers/", env!("CARGO_PKG_VERSION"));

/// How long a cached API response is reused.
const CACHE_MAX_AGE_S: u64 = 7 * 24 * 3600;

pub struct Http {
    agent: ureq::Agent,
    cache: Option<Cache>,
}

/// Cached API responses: one JSON file per URL under `<dir>/<host>/`, named by
/// the URL's SHA-256. Only the URL and response are stored; request headers
/// (such as an API key) never are.
pub struct Cache {
    pub dir: PathBuf,
    /// Ignore cached responses (and replace them).
    pub refresh: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    url: String,
    /// Unix seconds.
    fetched: u64,
    status: u16,
    content_type: String,
    final_url: String,
    body: String,
}

/// Per-request policy.
pub struct Get<'a> {
    pub url: &'a str,
    /// Statuses returned to the caller; any other non-retried status is an error.
    pub accept: &'a [u16],
    pub attempts: u32,
    /// Retry 429s (APIs). Downloads from publishers report them instead.
    pub retry_429: bool,
    pub body_limit: u64,
    /// Extra headers, sent only to requests whose host is `headers_host`.
    pub headers: &'a [(&'a str, String)],
    pub headers_host: &'a str,
    /// Use and fill the response cache, when the client has one.
    pub cache: bool,
}

impl<'a> Get<'a> {
    /// An API request: retries 429/5xx, accepts 200 and `accept`.
    pub fn api(url: &'a str, accept: &'a [u16]) -> Self {
        Get {
            url,
            accept,
            attempts: ATTEMPTS,
            retry_429: true,
            body_limit: API_BODY_LIMIT,
            headers: &[],
            headers_host: "",
            cache: true,
        }
    }
}

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: String,
    /// URL after redirects.
    pub url: String,
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

enum Attempt {
    Done(Reply),
    Redirect(String),
    Retry { reason: String, after: Option<u64> },
    Fail(String),
}

impl Http {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(USER_AGENT)
            .build();
        Http {
            agent: config.into(),
            cache: None,
        }
    }

    /// Caches API responses under `cache.dir`.
    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// GET with the per-host throttle, retries, and redirects; API responses
    /// come from and go to the cache when there is one.
    pub fn get(&self, req: &Get) -> Result<Reply, String> {
        let cache = self.cache.as_ref().filter(|_| req.cache);
        let path = match cache {
            Some(c) => Some(cache_path(&c.dir, req.url).map_err(|e| redact(&e, req.url))?),
            None => None,
        };
        if let (Some(c), Some(path)) = (cache, &path)
            && !c.refresh
            && let Some(reply) = read_cache(path, req.url, now_s())
        {
            return Ok(reply);
        }
        let reply = self.fetch(req)?;
        if let Some(path) = &path {
            write_cache(path, req.url, &reply);
        }
        Ok(reply)
    }

    fn fetch(&self, req: &Get) -> Result<Reply, String> {
        let mut url = req.url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            match self.get_once(&url, req).map_err(|e| redact(&e, &url))? {
                Ok(reply) => return Ok(reply),
                Err(next) => url = next,
            }
        }
        let message = format!("too many redirects from {} (last {url})", req.url);
        Err(redact(&redact(&message, &url), req.url))
    }

    /// One URL with retries: `Ok(Ok(reply))`, or `Ok(Err(location))` for a redirect.
    fn get_once(&self, url: &str, req: &Get) -> Result<Result<Reply, String>, String> {
        let host = host(url)?;
        for attempt in 1..=req.attempts {
            let (reason, after) = match throttled(&host, || self.attempt(url, &host, req))? {
                Attempt::Done(reply) => return Ok(Ok(reply)),
                Attempt::Redirect(location) => return Ok(Err(location)),
                Attempt::Fail(message) => return Err(message),
                Attempt::Retry { reason, after } => (reason, after),
            };
            if attempt == req.attempts {
                let tries = if req.attempts == 1 {
                    String::new()
                } else {
                    format!(" (gave up after {} attempts)", req.attempts)
                };
                return Err(format!("{reason} from {url}{tries}"));
            }
            let wait = after
                .map(|s| s.min(MAX_RETRY_AFTER_S))
                .unwrap_or_else(|| (5u64 << (attempt - 1)).min(60));
            let notice = format!(
                "papers: {reason} from {url}; retry {attempt}/{} in {wait}s",
                req.attempts - 1
            );
            eprintln!("{}", redact(&notice, url));
            sleep(Duration::from_secs(wait));
        }
        unreachable!("the last attempt returns")
    }

    fn attempt(&self, url: &str, host: &str, req: &Get) -> Attempt {
        let mut request = self.agent.get(url);
        if sends_headers(req, url, host) {
            for (name, value) in req.headers {
                request = request.header(*name, value);
            }
        }
        let mut response = match request.call() {
            Ok(response) => response,
            Err(e) => {
                return Attempt::Retry {
                    reason: format!("request failed ({e})"),
                    after: None,
                };
            }
        };
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        if (300..400).contains(&status)
            && let Some(location) = header("location")
        {
            return match resolve(url, &location) {
                Some(next) => Attempt::Redirect(next),
                None => Attempt::Fail(format!(
                    "HTTP {status} from {url} with unusable Location {location:?}"
                )),
            };
        }
        if status >= 500 || (status == 429 && req.retry_429) {
            return Attempt::Retry {
                reason: format!("HTTP {status}"),
                after: header("retry-after").and_then(|v| v.trim().parse().ok()),
            };
        }
        let content_type = header("content-type").unwrap_or_default();
        if !req.accept.contains(&status) {
            let mut message = format!("HTTP {status} from {url}");
            if status == 403 || status == 429 {
                message += " (blocked or rate-limited; not retried)";
            }
            // API errors explain themselves in a short JSON body.
            if content_type.contains("json")
                && let Ok(body) = response
                    .body_mut()
                    .with_config()
                    .limit(4096)
                    .read_to_string()
            {
                message += &format!(": {}", crate::text::squash(&body));
            }
            return Attempt::Fail(message);
        }
        match response
            .body_mut()
            .with_config()
            .limit(req.body_limit)
            .read_to_vec()
        {
            Ok(body) => Attempt::Done(Reply {
                status,
                body,
                content_type,
                url: url.to_owned(),
            }),
            Err(ureq::Error::BodyExceedsLimit(limit)) => {
                Attempt::Fail(format!("response from {url} exceeds {limit} bytes"))
            }
            Err(e) => Attempt::Retry {
                reason: format!("reading response failed ({e})"),
                after: None,
            },
        }
    }
}

/// Whether `req.headers` go to this hop: only to `req.headers_host`, and never
/// over plain http once the request started on https (a downgrading redirect
/// would otherwise expose an API key in clear text).
fn sends_headers(req: &Get, url: &str, host: &str) -> bool {
    let downgraded = req.url.starts_with("https:") && !url.starts_with("https:");
    host == req.headers_host && !downgraded
}

/// Query parameters that carry credentials (CiNii's `appid`, API keys).
const SECRET_PARAMS: &[&str] = &["appid", "api_key", "apikey", "key", "token", "access_token"];

/// `message` with the value of every credential query parameter of `url`
/// replaced, so retry notices and errors never print a key.
fn redact(message: &str, url: &str) -> String {
    let query = url.split_once('?').map_or("", |(_, q)| q);
    let query = query.split_once('#').map_or(query, |(q, _)| q);
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, value)| {
            !value.is_empty() && SECRET_PARAMS.iter().any(|s| name.eq_ignore_ascii_case(s))
        })
        .fold(message.to_owned(), |out, (_, value)| {
            out.replace(value, "<redacted>")
        })
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn cache_path(dir: &Path, url: &str) -> Result<PathBuf, String> {
    let host = host(url)?;
    Ok(dir
        .join(lock_file_name(&host).trim_end_matches(".lock"))
        .join(format!("{}.json", sha256_hex(url.as_bytes()))))
}

/// A cached reply for `url` younger than the maximum age.
fn read_cache(path: &Path, url: &str, now: u64) -> Option<Reply> {
    let text = std::fs::read_to_string(path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&text).ok()?;
    (entry.url == url && now.saturating_sub(entry.fetched) < CACHE_MAX_AGE_S).then(|| Reply {
        status: entry.status,
        body: entry.body.into_bytes(),
        content_type: entry.content_type,
        url: entry.final_url,
    })
}

/// Stores a text reply; a failure to write is reported, not fatal.
fn write_cache(path: &Path, url: &str, reply: &Reply) {
    let Ok(body) = std::str::from_utf8(&reply.body) else {
        return;
    };
    let entry = CacheEntry {
        url: url.to_owned(),
        fetched: now_s(),
        status: reply.status,
        content_type: reply.content_type.clone(),
        final_url: reply.url.clone(),
        body: body.to_owned(),
    };
    let json = serde_json::to_string(&entry).expect("cache entries serialize");
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&tmp, json))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        eprintln!(
            "papers: warning: cannot cache {url} at {}: {e}",
            path.display()
        );
    }
}

/// Lowercase host of an absolute http(s) URL.
pub fn host(url: &str) -> Result<String, String> {
    let uri: Uri = url
        .parse()
        .map_err(|e| format!("invalid URL {url:?}: {e}"))?;
    match (uri.scheme_str(), uri.host()) {
        (Some("http" | "https"), Some(host)) => Ok(host.to_ascii_lowercase()),
        _ => Err(format!("not an http(s) URL: {url:?}")),
    }
}

/// Minimum spacing between requests to `host`, from any process.
fn interval_ms(host: &str) -> u128 {
    // Test servers run on the loopback interface; there is no one to be polite to.
    if cfg!(test) && host == "127.0.0.1" {
        return 0;
    }
    match host {
        "export.arxiv.org" | "arxiv.org" | "www.arxiv.org" => ARXIV_INTERVAL_MS,
        _ => DEFAULT_INTERVAL_MS,
    }
}

/// Overrides the folder that holds the per-host throttle files.
pub const LOCK_DIR_VAR: &str = "PAPERS_LOCK_DIR";
/// Per-user cache folder name (distinct from the unrelated "Papers" app).
const CACHE_NAME: &str = "papers-research-tool";

/// The lock folder to use before checking it is writable.
#[derive(Debug, PartialEq, Eq)]
enum LockDir {
    /// Set by `PAPERS_LOCK_DIR`: it must work.
    Explicit(PathBuf),
    /// The per-user cache folder: falls back to the temp folder if unusable.
    Cache(PathBuf),
    /// No home folder to put a cache in.
    Unknown,
}

/// Picks the lock folder from `PAPERS_LOCK_DIR`, `HOME`, and `XDG_CACHE_HOME`.
/// It is per user and independent of `$TMPDIR`, which can differ between
/// sandboxed sessions and would split processes into groups that ignore each other.
fn choose_lock_dir(
    explicit: Option<OsString>,
    home: Option<OsString>,
    xdg_cache: Option<OsString>,
    macos: bool,
) -> LockDir {
    if let Some(dir) = explicit {
        return LockDir::Explicit(dir.into());
    }
    if !macos && let Some(xdg) = xdg_cache.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return LockDir::Cache(xdg.join(CACHE_NAME));
    }
    let Some(home) = home.map(PathBuf::from).filter(|p| p.is_absolute()) else {
        return LockDir::Unknown;
    };
    LockDir::Cache(if macos {
        home.join("Library/Caches").join(CACHE_NAME)
    } else {
        home.join(".cache").join(CACHE_NAME)
    })
}

/// Creates `dir` and confirms files can be created in it (a sandbox may allow
/// the folder but deny writes).
fn usable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)?;
    std::fs::remove_file(probe)
}

/// The folder holding the per-host throttle files, resolved once per process.
fn lock_dir() -> Result<&'static Path, String> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    DIR.get_or_init(|| {
        let fallback = |why: String| {
            let tmp = std::env::temp_dir();
            eprintln!(
                "papers: {why}; request spacing now only covers runs that share {} \
                 (set {LOCK_DIR_VAR} to a shared writable folder)",
                tmp.display()
            );
            Ok(tmp)
        };
        match choose_lock_dir(
            var(LOCK_DIR_VAR),
            var("HOME"),
            var("XDG_CACHE_HOME"),
            cfg!(target_os = "macos"),
        ) {
            LockDir::Explicit(dir) => usable(&dir)
                .map(|()| dir.clone())
                .map_err(|e| format!("{LOCK_DIR_VAR}={}: {e}", dir.display())),
            LockDir::Cache(dir) => match usable(&dir) {
                Ok(()) => Ok(dir),
                Err(e) => fallback(format!("cannot use {} ({e})", dir.display())),
            },
            LockDir::Unknown => fallback("no home folder for the lock files".to_owned()),
        }
    })
    .as_ref()
    .map(PathBuf::as_path)
    .map_err(Clone::clone)
}

fn lock_file_name(host: &str) -> String {
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.lock")
}

/// Runs `request` holding an exclusive lock on the host's lock file, at least
/// the host's interval after the previous request (from any process) completed.
pub fn throttled<T>(host: &str, request: impl FnOnce() -> T) -> Result<T, String> {
    throttle_in(lock_dir()?, host, interval_ms(host), request)
}

/// `throttled` with an explicit lock folder and interval.
fn throttle_in<T>(
    dir: &Path,
    host: &str,
    min: u128,
    request: impl FnOnce() -> T,
) -> Result<T, String> {
    let path = dir.join(lock_file_name(host));
    let context = |e: std::io::Error| format!("throttle file {}: {e}", path.display());
    let mut file: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(context)?;
    file.lock().map_err(context)?;
    let mut last = String::new();
    file.read_to_string(&mut last).map_err(context)?;
    if let Ok(last) = last.trim().parse::<u128>() {
        let elapsed = now_ms().saturating_sub(last);
        if elapsed < min {
            // Also bounds the wait if the clock moved backwards.
            sleep(Duration::from_millis((min - elapsed) as u64));
        }
    }
    let out = request();
    file.set_len(0).map_err(context)?;
    file.seek(SeekFrom::Start(0)).map_err(context)?;
    write!(file, "{}", now_ms()).map_err(context)?;
    file.unlock().map_err(context)?;
    Ok(out)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// Resolves a `Location` (or link) against `base`: absolute, scheme-relative,
/// root-relative, or path-relative. `None` for non-http(s) targets.
pub fn resolve(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    let (scheme, rest) = base.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let resolved = if location.starts_with("http://") || location.starts_with("https://") {
        location.to_owned()
    } else if let Some(r) = location.strip_prefix("//") {
        format!("{scheme}://{r}")
    } else if location.starts_with('/') {
        format!("{scheme}://{authority}{location}")
    } else if has_scheme(location) || location.starts_with('#') {
        return None;
    } else {
        let path = rest[authority.len()..].split(['?', '#']).next()?;
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        let dir = if dir.is_empty() { "/" } else { dir };
        format!("{scheme}://{authority}{dir}{location}")
    };
    host(&resolved).ok().map(|_| resolved)
}

/// True for `scheme:...` references (RFC 3986 scheme syntax).
fn has_scheme(s: &str) -> bool {
    s.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    })
}

/// Percent-encodes a URL component, keeping unreserved characters and `keep`.
pub fn encode(s: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || keep.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A local HTTP/1.1 server for tests: answers each connection, in order, with
/// the next canned response and returns the request heads it received.
#[cfg(test)]
pub(crate) fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut heads = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            while reader.read_line(&mut head).unwrap() > 2 && !head.ends_with("\r\n\r\n") {}
            heads.push(head);
            stream.write_all(response.as_bytes()).unwrap();
        }
        heads
    });
    (base, handle)
}

/// A complete response with `Connection: close`.
#[cfg(test)]
pub(crate) fn response(status: &str, headers: &[&str], body: &str) -> String {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for h in headers {
        out += h;
        out += "\r\n";
    }
    out + "\r\n" + body
}

/// Stores a JSON reply for `url` in the cache at `dir`, so a test can answer
/// an API on a fixed host without the network.
#[cfg(test)]
pub(crate) fn seed_cache(dir: &Path, url: &str, status: u16, body: &str) {
    let reply = Reply {
        status,
        body: body.as_bytes().to_vec(),
        content_type: "application/json".into(),
        url: url.into(),
    };
    write_cache(&cache_path(dir, url).unwrap(), url, &reply);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_and_retry_notices_hide_url_credentials() {
        let busy = || response("503 Service Unavailable", &["Retry-After: 0"], "");
        let (base, server) = serve(vec![busy(), busy()]);
        let url = format!("{base}/opensearch/articles?q=x&appid=s3cr3t-app-id&format=json");
        let err = Http::new()
            .get(&Get {
                attempts: 2,
                cache: false,
                ..Get::api(&url, &[200])
            })
            .err()
            .unwrap();
        server.join().unwrap();
        assert!(!err.contains("s3cr3t-app-id"), "{err}");
        assert!(err.contains("appid=<redacted>&format=json"), "{err}");
        assert_eq!(
            redact(
                "from https://h.example/a?API_KEY=k1k1&q=k&token=",
                "https://h.example/a?API_KEY=k1k1&q=k&token="
            ),
            "from https://h.example/a?API_KEY=<redacted>&q=k&token="
        );
    }

    #[test]
    fn headers_never_follow_a_downgrade() {
        let headers = [("x-api-key", "secret".to_owned())];
        let req = Get {
            headers: &headers,
            headers_host: "api.example.org",
            ..Get::api("https://api.example.org/works", &[200])
        };
        assert!(sends_headers(
            &req,
            "https://api.example.org/works?page=2",
            "api.example.org"
        ));
        assert!(!sends_headers(
            &req,
            "http://api.example.org/works",
            "api.example.org"
        ));
        assert!(!sends_headers(
            &req,
            "https://cdn.example.org/works",
            "cdn.example.org"
        ));
    }

    #[test]
    fn hosts_and_intervals() {
        assert_eq!(
            host("https://Export.arXiv.org/api/query?x").unwrap(),
            "export.arxiv.org"
        );
        assert!(host("ftp://x.org/a").is_err());
        assert!(host("not a url").is_err());
        assert_eq!(interval_ms("export.arxiv.org"), 3_000);
        assert_eq!(interval_ms("arxiv.org"), 3_000);
        assert_eq!(interval_ms("api.crossref.org"), 1_000);
        assert_eq!(lock_file_name("api.openalex.org"), "api.openalex.org.lock");
        assert_eq!(lock_file_name("a/../b"), "a_.._b.lock");
    }

    #[test]
    fn cache_round_trips_and_expires() {
        let dir = std::env::temp_dir().join(format!("papers-cache-test-{}", std::process::id()));
        let url = "https://api.example.org/works?q=a%20b";
        let path = cache_path(&dir, url).unwrap();
        assert!(path.starts_with(dir.join("api.example.org")));
        let reply = Reply {
            status: 404,
            body: b"{\"x\":1}".to_vec(),
            content_type: "application/json".into(),
            url: url.into(),
        };
        write_cache(&path, url, &reply);
        let now = now_s();
        let cached = read_cache(&path, url, now).unwrap();
        assert_eq!(
            (cached.status, cached.text()),
            (404, "{\"x\":1}".to_owned())
        );
        assert!(read_cache(&path, "https://api.example.org/other", now).is_none());
        assert!(read_cache(&path, url, now + CACHE_MAX_AGE_S).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn lock_dir_is_per_user_and_ignores_tmpdir() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            choose_lock_dir(os("/shared/locks"), os("/home/u"), None, true),
            LockDir::Explicit("/shared/locks".into())
        );
        assert_eq!(
            choose_lock_dir(None, os("/home/u"), os("/x/cache"), true),
            LockDir::Cache("/home/u/Library/Caches/papers-research-tool".into())
        );
        assert_eq!(
            choose_lock_dir(None, os("/home/u"), os("/x/cache"), false),
            LockDir::Cache("/x/cache/papers-research-tool".into())
        );
        // A relative XDG_CACHE_HOME is invalid per the spec and ignored.
        assert_eq!(
            choose_lock_dir(None, os("/home/u"), os("rel/cache"), false),
            LockDir::Cache("/home/u/.cache/papers-research-tool".into())
        );
        assert_eq!(choose_lock_dir(None, None, None, false), LockDir::Unknown);
        assert_eq!(
            choose_lock_dir(None, os("relative"), None, true),
            LockDir::Unknown
        );
    }

    #[test]
    fn separate_lock_holders_are_spaced_by_the_interval() {
        // Each thread opens the lock file itself, so they contend like separate processes.
        let dir = std::env::temp_dir().join(format!("papers-throttle-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        const MIN_MS: u128 = 300;
        let starts: Vec<u128> = std::thread::scope(|s| {
            let runs: Vec<_> = (0..2)
                .map(|_| s.spawn(|| throttle_in(&dir, "example.org", MIN_MS, now_ms).unwrap()))
                .collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        std::fs::remove_dir_all(&dir).unwrap();
        let gap = starts[0].abs_diff(starts[1]);
        assert!(gap >= MIN_MS, "requests only {gap} ms apart");
    }

    #[test]
    fn resolves_locations() {
        let base = "https://www.nber.org/papers/w99001?x=1";
        assert_eq!(
            resolve(base, "https://www.nber.org/papers/w99001.pdf").unwrap(),
            "https://www.nber.org/papers/w99001.pdf"
        );
        assert_eq!(
            resolve(base, "/system/files/w99001.pdf").unwrap(),
            "https://www.nber.org/system/files/w99001.pdf"
        );
        assert_eq!(
            resolve(base, "//cdn.example.org/a.pdf").unwrap(),
            "https://cdn.example.org/a.pdf"
        );
        assert_eq!(
            resolve(base, "w99001.pdf").unwrap(),
            "https://www.nber.org/papers/w99001.pdf"
        );
        assert_eq!(
            resolve("https://x.org", "a.pdf").unwrap(),
            "https://x.org/a.pdf"
        );
        assert!(resolve(base, "javascript:alert(1)").is_none());
        assert!(resolve(base, "mailto:a@b.c").is_none());
        assert!(resolve(base, "javascript://x").is_none());
        assert!(resolve(base, "file:///etc/passwd").is_none());
    }

    #[test]
    fn encodes_components() {
        assert_eq!(encode("a b/c:d", b"/:"), "a%20b/c:d");
        assert_eq!(encode("10.1/x(1)", b"/"), "10.1/x%281%29");
    }
}

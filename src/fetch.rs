use crate::domain::{Document, ReadFailure};
use anyhow::{Context, Result};
use axum::body::Bytes;
use reqwest::{Client, Response, StatusCode, Url, header};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
    time::{Instant, sleep_until},
};

const PAGE_LIMIT: usize = 8 * 1024 * 1024;
/// The sitemap protocol allows 50 MB per uncompressed file.
const UNPACKED_PAGE_LIMIT: usize = 64 * 1024 * 1024;
const FILE_LIMIT: u64 = 250 * 1024 * 1024;
/// The most bytes one ZIP may expand to, counted as they are decompressed.
const ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
/// The longest one probe sample may take, including its wait for the rate limit.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Session cookies kept per host, and the longest cookie name or value kept.
const MAX_COOKIES: usize = 20;
const COOKIE_LENGTH: usize = 4096;
/// The highest per-portal request rate `WATCHTOWER_DEV_RPS` may set.
pub const MAX_DEV_RPS: f64 = 5.0;
/// Marks the start of a development cache entry and its format version.
const CACHE_MAGIC: &[u8; 8] = b"WTCACHE1";
const CACHE_CHUNK: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Fetcher {
    client: Client,
    next_request: Arc<Mutex<HashMap<String, Instant>>>,
    files: PathBuf,
    /// The least time between two requests to one portal or host. One second unless
    /// `WATCHTOWER_DEV_RPS` raises the rate during development.
    spacing: Duration,
    /// Development-only response cache directory, from `WATCHTOWER_DEV_CACHE`.
    cache: Option<Arc<PathBuf>>,
}

pub struct PortalHttp {
    fetcher: Fetcher,
    portal_host: String,
    /// Session cookies the portal set during this run, by host. Some portals keep a search's
    /// filter in the session and page its results through it, so later pages need them.
    cookies: std::sync::Mutex<HashMap<String, BTreeMap<String, String>>>,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl Drop for PortalHttp {
    fn drop(&mut self) {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        if self.fetcher.cache.is_some() && hits + misses > 0 {
            tracing::info!(
                host = %self.portal_host,
                hits,
                misses,
                "Development cache summary"
            );
        }
    }
}

/// Reads a `WATCHTOWER_DEV_RPS` value: a positive request rate per portal, 1 when unset, and at
/// most `MAX_DEV_RPS`.
pub fn dev_rps(value: Option<&str>) -> Result<f64> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(1.0);
    };
    let rps: f64 = value
        .parse()
        .context("WATCHTOWER_DEV_RPS must be a positive number")?;
    anyhow::ensure!(
        rps.is_finite() && rps > 0.0,
        "WATCHTOWER_DEV_RPS must be a positive number"
    );
    Ok(rps.min(MAX_DEV_RPS))
}

/// A response whose status was accepted, read from the network or the development cache.
struct Fetched {
    url: Url,
    status: StatusCode,
    headers: header::HeaderMap,
    body: Body,
    /// The cache entry written while the body streams. It is kept only on `commit`.
    entry: Option<CacheEntry>,
}

enum Body {
    Network(Response),
    Cached {
        file: tokio::fs::File,
        remaining: u64,
    },
}

struct CacheEntry {
    temp: NamedTempFile,
    file: tokio::fs::File,
    path: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheMeta {
    url: String,
    /// Redirect targets followed before the final response, in order.
    hops: Vec<String>,
    status: u16,
    headers: Vec<(String, String)>,
}

/// The headers callers read from a response, and so the ones a cache entry keeps.
const CACHED_HEADERS: [header::HeaderName; 2] = [header::CONTENT_TYPE, header::CONTENT_DISPOSITION];

impl Fetched {
    fn network(response: Response) -> Self {
        Self {
            url: response.url().clone(),
            status: response.status(),
            headers: response.headers().clone(),
            body: Body::Network(response),
            entry: None,
        }
    }

    fn content_type(&self) -> Option<&str> {
        self.headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    }

    fn content_length(&self) -> Option<u64> {
        match &self.body {
            Body::Network(response) => response.content_length(),
            Body::Cached { remaining, .. } => Some(*remaining),
        }
    }

    async fn chunk(&mut self, phase: &'static str) -> Result<Option<Bytes>, ReadFailure> {
        match &mut self.body {
            Body::Network(response) => {
                let chunk = response
                    .chunk()
                    .await
                    .map_err(|err| transport_error(&self.url, phase, err))?;
                if let (Some(chunk), Some(entry)) = (&chunk, &mut self.entry)
                    && let Err(error) = entry.file.write_all(chunk).await
                {
                    tracing::debug!(%error, "Development cache entry abandoned");
                    self.entry = None;
                }
                Ok(chunk)
            }
            Body::Cached { file, remaining } => {
                if *remaining == 0 {
                    return Ok(None);
                }
                let mut buffer = vec![0; (*remaining).min(CACHE_CHUNK) as usize];
                file.read_exact(&mut buffer).await.map_err(|err| {
                    ReadFailure::Incomplete(format!("Could not read a cached response: {err}"))
                })?;
                *remaining -= buffer.len() as u64;
                Ok(Some(Bytes::from(buffer)))
            }
        }
    }

    /// Keeps the cache entry for this response once its whole body was read and accepted. The
    /// entry appears under its final name only by rename, so a crash leaves no partial entry.
    async fn commit(&mut self) {
        let Some(CacheEntry {
            temp,
            mut file,
            path,
        }) = self.entry.take()
        else {
            return;
        };
        let result = async {
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
            temp.persist(&path).map_err(|err| err.error)?;
            std::io::Result::Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::debug!(%error, "Development cache entry not written");
        }
    }
}

fn cache_path(directory: &Path, url: &Url) -> PathBuf {
    let key = Sha256::digest(format!("GET {url}"));
    directory.join(format!("{key:x}"))
}

/// Opens a cache entry and leaves the file positioned at the body. Unreadable entries count as
/// missing.
async fn open_cached(path: &Path) -> std::io::Result<Option<(CacheMeta, Body)>> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let total = file.metadata().await?.len();
    if total < 12 {
        return Ok(None);
    }
    let mut magic = [0; 8];
    file.read_exact(&mut magic).await?;
    if &magic != CACHE_MAGIC {
        return Ok(None);
    }
    let length = u64::from(file.read_u32_le().await?);
    if 12 + length > total {
        return Ok(None);
    }
    let mut meta = vec![0; length as usize];
    file.read_exact(&mut meta).await?;
    let Ok(meta) = serde_json::from_slice::<CacheMeta>(&meta) else {
        return Ok(None);
    };
    Ok(Some((
        meta,
        Body::Cached {
            file,
            remaining: total - 12 - length,
        },
    )))
}

/// Starts a cache entry: a temporary file holding the response header, which `commit` renames
/// into place once the body has been written after it.
async fn start_entry(
    directory: &Path,
    path: PathBuf,
    response: &Response,
    hops: &[Url],
) -> std::io::Result<CacheEntry> {
    let meta = CacheMeta {
        url: response.url().to_string(),
        hops: hops.iter().map(Url::to_string).collect(),
        status: response.status().as_u16(),
        headers: CACHED_HEADERS
            .iter()
            .filter_map(|name| {
                let value = response.headers().get(name)?.to_str().ok()?;
                Some((name.as_str().to_owned(), value.to_owned()))
            })
            .collect(),
    };
    let meta = serde_json::to_vec(&meta)?;
    let temp = NamedTempFile::new_in(directory)?;
    let mut file = tokio::fs::File::create(temp.path()).await?;
    file.write_all(CACHE_MAGIC).await?;
    file.write_u32_le(meta.len() as u32).await?;
    file.write_all(&meta).await?;
    Ok(CacheEntry { temp, file, path })
}

#[derive(Debug)]
pub struct Page {
    pub url: Url,
    pub body: String,
}

/// A page read while probing: the body plus the status and content type that describe it.
#[derive(Debug)]
pub(crate) struct Sample {
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

#[derive(Debug)]
pub(crate) enum DocumentResponse {
    /// Staged files, plus notes on files left out because they passed a size limit.
    Files {
        documents: Vec<Document>,
        skipped: Vec<String>,
    },
    Html(Page),
}

impl DocumentResponse {
    fn skipped(note: String) -> Self {
        tracing::warn!(note, "Document skipped");
        Self::Files {
            documents: Vec::new(),
            skipped: vec![note],
        }
    }
}

impl Fetcher {
    pub fn new(files: PathBuf) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .user_agent("Watchtower/0.1 (public procurement reader)")
            .build()?;
        Ok(Self {
            client,
            next_request: Default::default(),
            files,
            spacing: Duration::from_secs(1),
            cache: None,
        })
    }

    /// Development only: spaces requests to one portal for `rps` requests per second, at most
    /// `MAX_DEV_RPS`. A value that is not positive keeps one request per second.
    pub fn with_rate(mut self, rps: f64) -> Self {
        if rps.is_finite() && rps > 0.0 {
            self.spacing = Duration::from_secs_f64(1.0 / rps.min(MAX_DEV_RPS));
        }
        self
    }

    /// Development only: saves successful GET responses under `directory` and answers repeated
    /// requests from there, with no network call and no rate-limit wait.
    pub fn with_cache(mut self, directory: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&directory)
            .context("Could not create the development cache directory")?;
        self.cache = Some(Arc::new(directory));
        Ok(self)
    }

    /// The least time between two requests to one portal.
    pub fn spacing(&self) -> Duration {
        self.spacing
    }

    /// The development cache directory, when one is set.
    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache.as_deref().map(PathBuf::as_path)
    }

    pub fn portal(&self, url: &Url) -> PortalHttp {
        PortalHttp {
            fetcher: self.clone(),
            portal_host: url.host_str().unwrap_or_default().into(),
            cookies: Default::default(),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        }
    }
}

pub fn public_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

impl PortalHttp {
    async fn request(&self, url: Url, phase: &'static str) -> Result<Fetched, ReadFailure> {
        self.request_unless_seen(url, phase, None)
            .await?
            .ok_or_else(|| ReadFailure::Incomplete("Portal URL was already sampled".into()))
    }

    /// Answers a request from the development cache, if one is set and holds the URL.
    async fn cached(
        &self,
        url: &Url,
        seen: Option<&HashSet<Url>>,
    ) -> Option<Result<Option<Fetched>, ReadFailure>> {
        let directory = self.fetcher.cache.as_deref()?;
        let path = cache_path(directory, url);
        let (meta, body) = match open_cached(&path).await {
            Ok(Some(entry)) => entry,
            Ok(None) => return None,
            Err(error) => {
                tracing::debug!(%error, "Development cache entry unreadable");
                return None;
            }
        };
        let final_url = Url::parse(&meta.url).ok()?;
        let hops = meta
            .hops
            .iter()
            .map(|hop| Url::parse(hop).ok())
            .collect::<Option<Vec<_>>>()?;
        let mut headers = header::HeaderMap::new();
        for (name, value) in &meta.headers {
            if let (Ok(name), Ok(value)) = (
                header::HeaderName::from_bytes(name.as_bytes()),
                header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
        let path_label: String = url.path().chars().take(240).collect();
        tracing::debug!(
            host = url.host_str().unwrap_or_default(),
            path = path_label,
            status = meta.status,
            "Development cache hit"
        );
        if hops
            .iter()
            .any(|hop| seen.is_some_and(|urls| urls.contains(hop)))
        {
            return Some(Ok(None));
        }
        let status = StatusCode::from_u16(meta.status).ok()?;
        Some(Ok(Some(Fetched {
            url: final_url,
            status,
            headers,
            body,
            entry: None,
        })))
    }

    async fn request_unless_seen(
        &self,
        mut url: Url,
        phase: &'static str,
        seen: Option<&HashSet<Url>>,
    ) -> Result<Option<Fetched>, ReadFailure> {
        let mut key = None;
        let mut hops = Vec::new();
        for _ in 0..=5 {
            if !public_http(&url) {
                return Err(ReadFailure::Incomplete(
                    "Portal URL must be HTTP(S) without credentials".into(),
                ));
            }
            url.set_fragment(None);
            if seen.is_some_and(|urls| urls.contains(&url)) {
                return Ok(None);
            }
            match &key {
                Some(_) => hops.push(url.clone()),
                None => {
                    if let Some(answer) = self.cached(&url, seen).await {
                        return answer;
                    }
                    if self.fetcher.cache.is_some() {
                        self.cache_misses.fetch_add(1, Ordering::Relaxed);
                    }
                    key = Some(url.clone());
                }
            }
            let host = url.host_str().unwrap_or_default().to_owned();
            loop {
                let mut entries = self.fetcher.next_request.lock().await;
                let now = Instant::now();
                let wait_until = entries
                    .get(&self.portal_host)
                    .copied()
                    .unwrap_or(now)
                    .max(entries.get(&host).copied().unwrap_or(now));
                if wait_until <= now {
                    let next = now + self.fetcher.spacing;
                    entries.insert(self.portal_host.clone(), next);
                    entries.insert(host.clone(), next);
                    break;
                }
                drop(entries);
                sleep_until(wait_until).await;
            }
            let mut request = self.fetcher.client.get(url.clone());
            if let Some(cookie) = self.cookie_header(&host) {
                request = request.header(header::COOKIE, cookie);
            }
            let response = request
                .send()
                .await
                .map_err(|err| transport_error(&url, phase, err))?;
            self.keep_cookies(&host, &response);
            let status = response.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                url = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|location| location.to_str().ok())
                    .and_then(|location| url.join(location).ok())
                    .ok_or_else(|| {
                        ReadFailure::Incomplete("Portal sent a malformed redirect".into())
                    })?;
                continue;
            }
            if status == StatusCode::UNAUTHORIZED || access_path(response.url()) {
                return Err(ReadFailure::Blocked("Portal requires a login".into()));
            }
            if status == StatusCode::FORBIDDEN {
                let body = read_text(
                    &mut Fetched::network(response),
                    "access-denied response body",
                )
                .await?;
                if let Some(reason) = access_page(&body) {
                    return Err(ReadFailure::Blocked(reason.into()));
                }
                return Err(ReadFailure::Incomplete("Portal returned HTTP 403".into()));
            }
            if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                let reason = format!("Portal returned HTTP {}", status.as_u16());
                return Err(
                    match response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(retry_after)
                    {
                        Some(retry_after) => ReadFailure::Throttled {
                            reason: format!(
                                "{reason}; the portal asked to wait {}s",
                                retry_after.as_secs()
                            ),
                            retry_after,
                        },
                        None => ReadFailure::Temporary(reason),
                    },
                );
            }
            if !status.is_success() {
                return Err(ReadFailure::Incomplete(format!(
                    "Portal returned HTTP {}",
                    status.as_u16()
                )));
            }
            let mut fetched = Fetched::network(response);
            if let (Some(directory), Some(key)) = (self.fetcher.cache.as_deref(), &key) {
                let Body::Network(response) = &fetched.body else {
                    unreachable!("a network response has a network body");
                };
                match start_entry(directory, cache_path(directory, key), response, &hops).await {
                    Ok(entry) => fetched.entry = Some(entry),
                    Err(error) => tracing::debug!(%error, "Development cache entry not started"),
                }
            }
            return Ok(Some(fetched));
        }
        Err(ReadFailure::Incomplete(
            "Portal redirected too many times".into(),
        ))
    }

    /// The `Cookie` header for `host`: the cookies it set earlier in this run.
    fn cookie_header(&self, host: &str) -> Option<String> {
        let cookies = self.cookies.lock().expect("cookie lock");
        let jar = cookies.get(host).filter(|jar| !jar.is_empty())?;
        Some(
            jar.iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Keeps the cookies a response sets, for later requests to the same host in this run.
    fn keep_cookies(&self, host: &str, response: &Response) {
        let mut cookies = self.cookies.lock().expect("cookie lock");
        for header in response.headers().get_all(header::SET_COOKIE) {
            let Some((name, value, expired)) = header.to_str().ok().and_then(set_cookie) else {
                continue;
            };
            let jar = cookies.entry(host.to_owned()).or_default();
            if expired {
                jar.remove(&name);
            } else if jar.len() < MAX_COOKIES || jar.contains_key(&name) {
                jar.insert(name, value);
            }
        }
    }

    pub async fn page(&self, url: Url) -> Result<Page, ReadFailure> {
        Self::read_page(self.request(url, "page headers").await?, "page body").await
    }

    /// Reads a page like `page`, keeping the status and content type for probing. A probe is a
    /// cheap check, so each sample gets `PROBE_TIMEOUT` rather than the client's read timeout:
    /// an optional route that hangs, such as a slow sitemap, is skipped instead of stalling.
    pub(crate) async fn sample(&self, url: Url) -> Result<Sample, ReadFailure> {
        let host = url.host_str().unwrap_or("unknown host").to_owned();
        let read = async {
            let response = self.request(url, "probe headers").await?;
            let status = response.status.as_u16();
            let content_type = response.content_type().map(str::to_owned);
            let page = Self::read_page(response, "probe body").await?;
            Ok(Sample {
                url: page.url,
                status,
                content_type,
                body: page.body,
            })
        };
        tokio::time::timeout(PROBE_TIMEOUT, read)
            .await
            .unwrap_or_else(|_| {
                Err(ReadFailure::Temporary(format!(
                    "Portal fetch failed on {host} (probe): request timed out"
                )))
            })
    }

    pub(crate) async fn page_unless_seen(
        &self,
        url: Url,
        seen: &HashSet<Url>,
    ) -> Result<Option<Page>, ReadFailure> {
        match self
            .request_unless_seen(url, "page headers", Some(seen))
            .await?
        {
            Some(response) => Self::read_page(response, "page body").await.map(Some),
            None => Ok(None),
        }
    }

    async fn read_page(mut response: Fetched, phase: &'static str) -> Result<Page, ReadFailure> {
        let url = response.url.clone();
        let body = read_text(&mut response, phase).await?;
        if let Some(reason) = access_page(&body) {
            return Err(ReadFailure::Blocked(reason.into()));
        }
        response.commit().await;
        Ok(Page { url, body })
    }

    pub(crate) async fn documents(&self, url: Url) -> Result<DocumentResponse, ReadFailure> {
        let mut response = self.request(url, "document headers").await?;
        let content_type = response
            .content_type()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("text/html") {
            let destination = response.url.clone();
            let body = read_text(&mut response, "document HTML response body").await?;
            if let Some(reason) = access_page(&body) {
                return Err(ReadFailure::Blocked(reason.into()));
            }
            response.commit().await;
            let host = destination.host_str().unwrap_or_default();
            let path: String = destination.path().chars().take(240).collect();
            tracing::info!(host, path, "Inspecting HTML returned by document link");
            return Ok(DocumentResponse::Html(Page {
                url: destination,
                body,
            }));
        }
        let name = filename(&response);
        if response
            .content_length()
            .is_some_and(|len| len > FILE_LIMIT)
        {
            return Ok(DocumentResponse::skipped(over_file_limit(&name)));
        }
        let temp = NamedTempFile::new_in(&self.fetcher.files).map_err(file_error)?;
        let mut file = tokio::fs::File::create(temp.path())
            .await
            .map_err(file_error)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut magic = Vec::with_capacity(4);
        while let Some(chunk) = response.chunk("document body").await? {
            size += chunk.len() as u64;
            if size > FILE_LIMIT {
                return Ok(DocumentResponse::skipped(over_file_limit(&name)));
            }
            if magic.len() < 4 {
                magic.extend_from_slice(&chunk[..chunk.len().min(4 - magic.len())]);
            }
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(file_error)?;
        }
        file.flush().await.map_err(file_error)?;
        drop(file);
        response.commit().await;
        let directory = self.fetcher.files.clone();
        let archive = name.to_ascii_lowercase().ends_with(".zip")
            || content_type.contains("application/zip")
            || (!name.contains('.') && magic.starts_with(b"PK\x03\x04"));
        tokio::task::spawn_blocking(move || {
            if archive {
                unpack(temp.path(), &directory, &name, UnpackLimits::default())
            } else {
                let sha = format!("{:x}", hash.finalize());
                Ok(Unpacked {
                    documents: vec![persist(temp, &directory, name, size, sha)?],
                    skipped: Vec::new(),
                })
            }
        })
        .await
        .map_err(file_error)?
        .map(|unpacked| {
            for note in &unpacked.skipped {
                tracing::warn!(note, "Archive member skipped");
            }
            DocumentResponse::Files {
                documents: unpacked.documents,
                skipped: unpacked.skipped,
            }
        })
        .map_err(file_error)
    }
}

/// Name, value and whether it was deleted, from a `Set-Cookie` header such as
/// `JSESSIONID=abc; Path=/; HttpOnly`. Attributes other than an expiry are ignored: cookies are
/// only sent back to the host that set them, within one run.
fn set_cookie(header: &str) -> Option<(String, String, bool)> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let (name, value) = (name.trim(), value.trim().trim_matches('"'));
    let valid = |text: &str| {
        text.len() <= COOKIE_LENGTH
            && text
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, ';' | ',' | '"' | '\\'))
    };
    if name.is_empty() || !valid(name) || !valid(value) {
        return None;
    }
    let expired = value.is_empty()
        || parts.any(|attribute| {
            attribute.split_once('=').is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("max-age") && value.trim().starts_with(['0', '-'])
            })
        });
    Some((name.to_owned(), value.to_owned(), expired))
}

async fn read_text(response: &mut Fetched, phase: &'static str) -> Result<String, ReadFailure> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk(phase).await? {
        if data.len() + chunk.len() > PAGE_LIMIT {
            return Err(ReadFailure::Incomplete(
                "Listing or detail page exceeds the page size limit".into(),
            ));
        }
        data.extend_from_slice(&chunk);
    }
    if data.starts_with(&[0x1f, 0x8b]) {
        data = gunzip(&data)?;
    }
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Unpacks a gzip body such as a compressed sitemap. Unpacked pages may be larger than the
/// download limit, up to `UNPACKED_PAGE_LIMIT`.
fn gunzip(data: &[u8]) -> Result<Vec<u8>, ReadFailure> {
    let mut unpacked = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(UNPACKED_PAGE_LIMIT as u64 + 1)
        .read_to_end(&mut unpacked)
        .map_err(|_| ReadFailure::Incomplete("A compressed page could not be unpacked".into()))?;
    if unpacked.len() > UNPACKED_PAGE_LIMIT {
        return Err(ReadFailure::Incomplete(
            "A compressed page exceeds the unpacked size limit".into(),
        ));
    }
    Ok(unpacked)
}

fn filename(response: &Fetched) -> String {
    let header_name = response
        .headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| disposition_filename(value.as_bytes()));
    header_name
        .as_deref()
        .or_else(|| response.url.path_segments().and_then(Iterator::last))
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("document")
        .to_owned()
}

/// The file name in a `Content-Disposition` header. An RFC 8187 `filename*` wins over a plain
/// `filename`. Parameter names are case-insensitive, quoted values may contain `;`, and a raw
/// UTF-8 name, which some portals send, is read as UTF-8.
fn disposition_filename(header: &[u8]) -> Option<String> {
    let header = String::from_utf8_lossy(header);
    let mut plain = None;
    let mut extended = None;
    for parameter in disposition_parameters(&header).into_iter().skip(1) {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "filename*" if extended.is_none() => extended = extended_value(value),
            "filename" if plain.is_none() => plain = Some(unquote(value)),
            _ => {}
        }
    }
    extended.or(plain).filter(|name| !name.trim().is_empty())
}

/// Splits a header at semicolons outside quoted strings.
fn disposition_parameters(header: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (index, ch) in header.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&header[start..]);
    parts
}

fn unquote(value: &str) -> String {
    match value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        Some(inner) => {
            let mut text = String::with_capacity(inner.len());
            let mut chars = inner.chars();
            while let Some(ch) = chars.next() {
                text.push(if ch == '\\' {
                    chars.next().unwrap_or(ch)
                } else {
                    ch
                });
            }
            text
        }
        None => value.trim_matches('"').to_owned(),
    }
}

/// Decodes an RFC 8187 value such as `UTF-8''Zad%C3%A1vac%C3%AD.pdf`.
fn extended_value(value: &str) -> Option<String> {
    let value = value.trim_matches('"');
    let mut parts = value.splitn(3, '\'');
    let (charset, _language, encoded) = (parts.next()?, parts.next()?, parts.next()?);
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut input = encoded.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let digits = [input.next()?, input.next()?];
            let hex = std::str::from_utf8(&digits).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some(if charset.eq_ignore_ascii_case("iso-8859-1") {
        bytes.iter().map(|&byte| char::from(byte)).collect()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn access_path(url: &Url) -> bool {
    matches!(
        url.path()
            .to_ascii_lowercase()
            .trim_end_matches('/')
            .rsplit('/')
            .next(),
        Some("login" | "signin" | "sign-in" | "logon")
    )
}

fn access_page(body: &str) -> Option<&'static str> {
    let text = body.to_ascii_lowercase();
    if (text.contains("verify you are human") || text.contains("verify that you are human"))
        && (text.contains("captcha") || text.contains("challenge-form"))
    {
        Some("Portal blocks delivery with a CAPTCHA")
    } else if (text.contains("type=\"password\"") || text.contains("type='password'"))
        && (text.contains("<title>login")
            || text.contains("<title>sign in")
            || text.contains("login required"))
    {
        Some("Portal requires a login")
    } else {
        None
    }
}

/// Reads a `Retry-After` value: delay seconds or an HTTP date. A date in the past means no wait.
fn retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let seconds = (date.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    Some(Duration::from_secs(seconds.max(0) as u64))
}

fn transport_error(url: &Url, phase: &'static str, error: reqwest::Error) -> ReadFailure {
    let cause = transport_cause(&error);
    let host = url.host_str().unwrap_or("unknown host");
    let path: String = url.path().chars().take(240).collect();
    tracing::warn!(host, phase, cause, path, "Portal fetch failed");
    ReadFailure::Temporary(format!("Portal fetch failed on {host} ({phase}): {cause}"))
}

fn transport_cause(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        return "request timed out";
    }
    let mut network_cause = None;
    let mut source = error.source();
    while let Some(current) = source {
        let message = current.to_string().to_ascii_lowercase();
        if message.contains("dns error") || message.contains("failed to lookup address") {
            return "DNS lookup failed";
        }
        if message.contains("certificate") || message.contains("tls alert") {
            return "TLS handshake failed";
        }
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            network_cause = match io_error.kind() {
                ErrorKind::ConnectionRefused => Some("connection refused"),
                ErrorKind::ConnectionReset => Some("connection reset"),
                ErrorKind::ConnectionAborted => Some("connection aborted"),
                ErrorKind::UnexpectedEof => Some("connection ended early"),
                ErrorKind::TimedOut => Some("request timed out"),
                _ => network_cause,
            };
        }
        source = current.source();
    }
    network_cause.unwrap_or(if error.is_connect() {
        "connection failed"
    } else if error.is_body() || error.is_decode() {
        "response ended early"
    } else {
        "HTTP transport failed"
    })
}

fn file_error(err: impl std::fmt::Display) -> ReadFailure {
    ReadFailure::Incomplete(format!("Could not stage document: {err}"))
}

fn persist(
    temp: NamedTempFile,
    directory: &Path,
    name: String,
    size: u64,
    sha: String,
) -> Result<Document> {
    match temp.persist_noclobber(directory.join(&sha)) {
        Ok(_) => {}
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    Ok(Document {
        file_name: name,
        size_bytes: size,
        path: format!("/files/{sha}"),
        sha256: sha,
    })
}

fn over_file_limit(name: &str) -> String {
    format!(
        "{name}: skipped, larger than the {} file limit",
        size_label(FILE_LIMIT)
    )
}

fn size_label(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= MB && bytes.is_multiple_of(MB) {
        format!("{} MB", bytes / MB)
    } else {
        format!("{bytes}-byte")
    }
}

/// Size limits for unpacking one ZIP. Tests use small limits.
#[derive(Clone, Copy)]
struct UnpackLimits {
    /// The most bytes one member may expand to.
    file: u64,
    /// The most bytes the whole archive may expand to, including members that were discarded.
    total: u64,
}

impl Default for UnpackLimits {
    fn default() -> Self {
        Self {
            file: FILE_LIMIT,
            total: ARCHIVE_LIMIT,
        }
    }
}

#[derive(Debug)]
struct Unpacked {
    documents: Vec<Document>,
    /// One note per member left out over the file limit, and one for the members left out
    /// once the archive reached its expanded-size limit.
    skipped: Vec<String>,
}

/// Unpacks the files in a ZIP. Directory structure is dropped and nested archives stay files.
///
/// Sizes are measured from the bytes actually decompressed, never from the headers. A member
/// that expands past `limits.file` is discarded. Once the archive would expand past
/// `limits.total`, the member in progress is discarded and no further members are read. Every
/// decompressed byte counts toward the total, including bytes of discarded members, so the
/// work and disk use stay bounded whatever the headers claim. A member is staged and listed
/// only after it was read completely.
fn unpack(
    path: &Path,
    directory: &Path,
    archive_name: &str,
    limits: UnpackLimits,
) -> Result<Unpacked> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    anyhow::ensure!(
        archive.len() <= MAX_ARCHIVE_ENTRIES,
        "ZIP contains too many entries"
    );
    let mut expanded = 0_u64;
    let mut documents = Vec::new();
    let mut skipped = Vec::new();
    let mut buffer = [0; 64 * 1024];
    let entries = archive.len();
    for index in 0..entries {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .enclosed_name()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .context("ZIP contains an invalid entry path")?;
        if entry.size() > limits.file {
            skipped.push(format!(
                "{archive_name}/{name}: skipped, declared larger than the {} file limit",
                size_label(limits.file)
            ));
            continue;
        }
        let mut temp = NamedTempFile::new_in(directory)?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut over_file = false;
        let mut over_total = false;
        loop {
            let length = entry.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            if expanded + length as u64 > limits.total {
                over_total = true;
                break;
            }
            expanded += length as u64;
            size += length as u64;
            if size > limits.file {
                over_file = true;
                break;
            }
            hash.update(&buffer[..length]);
            temp.write_all(&buffer[..length])?;
        }
        if over_total {
            drop(temp);
            let remaining = entries - index;
            skipped.push(format!(
                "{archive_name}: stopped at the {} expanded-size limit; skipped {name} and \
                 {} later entr{}",
                size_label(limits.total),
                remaining - 1,
                if remaining == 2 { "y" } else { "ies" }
            ));
            break;
        }
        if over_file {
            drop(temp);
            skipped.push(format!(
                "{archive_name}/{name}: skipped, expands past the {} file limit",
                size_label(limits.file)
            ));
            continue;
        }
        temp.flush()?;
        documents.push(persist(
            temp,
            directory,
            name,
            size,
            format!("{:x}", hash.finalize()),
        )?);
    }
    Ok(Unpacked { documents, skipped })
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[tokio::test]
    async fn refused_download_reports_host_phase_and_cause_without_query() {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/document.pdf?token=test-secret",
            socket.local_addr().unwrap()
        ))
        .unwrap();
        drop(socket);

        let files = tempfile::tempdir().unwrap();
        let portal = Fetcher::new(files.path().into()).unwrap().portal(&url);
        let failure = portal.documents(url).await.unwrap_err();
        assert!(matches!(failure, ReadFailure::Temporary(_)));
        let reason = failure.to_string();
        assert!(
            reason.contains("127.0.0.1 (document headers): connection refused"),
            "{reason}"
        );
        assert!(!reason.contains("test-secret"));
    }

    #[test]
    fn content_disposition_names_prefer_the_encoded_form() {
        for (header, expected) in [
            (
                &b"attachment; filename=\"Zad?vac?.pdf\"; filename*=UTF-8''Zad%C3%A1vac%C3%AD%20dokumentace.pdf"[..],
                Some("Zadávací dokumentace.pdf"),
            ),
            (b"attachment;filename=\"priloha_1.docx\"", Some("priloha_1.docx")),
            (b"inline; FILENAME=plan.pdf", Some("plan.pdf")),
            (b"attachment; filename=\"a;b \\\"c\\\".pdf\"", Some("a;b \"c\".pdf")),
            (b"attachment; filename*=iso-8859-1'cs'Smlouva%E1.pdf", Some("Smlouvaá.pdf")),
            ("attachment; filename=\"Výkaz výměr.xlsx\"".as_bytes(), Some("Výkaz výměr.xlsx")),
            (b"attachment", None),
            (b"attachment; filename=\"\"", None),
        ] {
            assert_eq!(disposition_filename(header).as_deref(), expected, "{header:?}");
        }
    }

    #[test]
    fn retry_after_reads_seconds_or_an_http_date() {
        assert_eq!(retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
        let later = (chrono::Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
        let wait = retry_after(&later).unwrap().as_secs();
        assert!((85..=90).contains(&wait), "{wait}");
        assert_eq!(retry_after("soon"), None);
    }
}

#[cfg(test)]
mod unpack_tests {
    use super::*;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in members {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// Rewrites the uncompressed size of `member` in its local and central headers.
    fn declare_size(archive: &mut [u8], member: &str, size: u32) {
        let mut patched = 0;
        for offset in 0..archive.len().saturating_sub(46) {
            let (size_at, length_at, name_at) = match &archive[offset..offset + 4] {
                b"PK\x03\x04" => (offset + 22, offset + 26, offset + 30),
                b"PK\x01\x02" => (offset + 24, offset + 28, offset + 46),
                _ => continue,
            };
            let length = u16::from_le_bytes([archive[length_at], archive[length_at + 1]]) as usize;
            if archive.get(name_at..name_at + length) == Some(member.as_bytes()) {
                archive[size_at..size_at + 4].copy_from_slice(&size.to_le_bytes());
                patched += 1;
            }
        }
        assert_eq!(patched, 2, "expected a local and a central header");
    }

    struct Staged {
        result: Result<Unpacked>,
        /// Names of the files left in the staging directory.
        files: Vec<String>,
        _directory: tempfile::TempDir,
    }

    fn unpack_bytes(archive: &[u8], file: u64, total: u64) -> Staged {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("bundle.zip");
        std::fs::write(&path, archive).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let result = unpack(
            &path,
            directory.path(),
            "bundle.zip",
            UnpackLimits { file, total },
        );
        let mut files: Vec<String> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        if let Ok(unpacked) = &result {
            for document in &unpacked.documents {
                let bytes = std::fs::read(directory.path().join(&document.sha256)).unwrap();
                assert_eq!(bytes.len() as u64, document.size_bytes);
                assert_eq!(format!("{:x}", Sha256::digest(&bytes)), document.sha256);
                assert_eq!(document.path, format!("/files/{}", document.sha256));
            }
        }
        Staged {
            result,
            files,
            _directory: directory,
        }
    }

    fn names(unpacked: &Unpacked) -> Vec<&str> {
        unpacked
            .documents
            .iter()
            .map(|document| document.file_name.as_str())
            .collect()
    }

    /// The staging directory holds exactly the listed documents: nothing half-written.
    fn assert_only_documents(staged: &Staged) {
        let unpacked = staged.result.as_ref().unwrap();
        let mut expected: Vec<String> = unpacked
            .documents
            .iter()
            .map(|document| document.sha256.clone())
            .collect();
        expected.sort();
        assert_eq!(staged.files, expected);
    }

    #[test]
    fn total_over_the_limit_keeps_earlier_members_and_skips_the_rest() {
        let archive = zip(&[
            ("a.txt", &[b'a'; 100]),
            ("docs/b.txt", &[b'b'; 100]),
            ("c.txt", &[b'c'; 100]),
            ("d.txt", &[b'd'; 100]),
        ]);
        let staged = unpack_bytes(&archive, 1_000, 250);
        let unpacked = staged.result.as_ref().unwrap();
        assert_eq!(names(unpacked), ["a.txt", "b.txt"]);
        assert_eq!(
            unpacked.skipped,
            [
                "bundle.zip: stopped at the 250-byte expanded-size limit; skipped c.txt and 1 later entry"
            ]
        );
        assert_only_documents(&staged);
    }

    #[test]
    fn member_over_the_file_limit_is_skipped_and_later_members_kept() {
        let archive = zip(&[
            ("first.txt", &[b'1'; 50]),
            ("big.bin", &[b'x'; 200]),
            ("last.txt", &[b'2'; 50]),
        ]);
        let staged = unpack_bytes(&archive, 150, 10_000);
        let unpacked = staged.result.as_ref().unwrap();
        assert_eq!(names(unpacked), ["first.txt", "last.txt"]);
        assert_eq!(
            unpacked.skipped,
            ["bundle.zip/big.bin: skipped, declared larger than the 150-byte file limit"]
        );
        assert_only_documents(&staged);
    }

    #[test]
    fn understated_member_size_is_measured_while_streaming() {
        let mut archive = zip(&[("bomb.bin", &[0; 10_000]), ("ok.txt", b"fine")]);
        declare_size(&mut archive, "bomb.bin", 10);
        let staged = unpack_bytes(&archive, 150, 1_000_000);
        let unpacked = staged.result.as_ref().unwrap();
        assert_eq!(names(unpacked), ["ok.txt"]);
        assert_eq!(
            unpacked.skipped,
            ["bundle.zip/bomb.bin: skipped, expands past the 150-byte file limit"]
        );
        assert_only_documents(&staged);
    }

    #[test]
    fn understated_member_bytes_count_toward_the_archive_limit() {
        let mut archive = zip(&[
            ("small.txt", &[b's'; 100]),
            ("bomb.bin", &[0; 10_000]),
            ("ok.txt", b"fine"),
        ]);
        declare_size(&mut archive, "bomb.bin", 10);
        let staged = unpack_bytes(&archive, 1_000_000, 500);
        let unpacked = staged.result.as_ref().unwrap();
        assert_eq!(names(unpacked), ["small.txt"]);
        assert_eq!(
            unpacked.skipped,
            [
                "bundle.zip: stopped at the 500-byte expanded-size limit; skipped bomb.bin and 1 later entry"
            ]
        );
        assert_only_documents(&staged);
    }

    #[test]
    fn nested_archives_stay_files_and_traversal_paths_are_refused() {
        let inner = zip(&[("inner.txt", b"inner")]);
        let staged = unpack_bytes(&zip(&[("nested.zip", &inner)]), 1_000, 10_000);
        assert_eq!(names(staged.result.as_ref().unwrap()), ["nested.zip"]);
        assert_only_documents(&staged);

        let staged = unpack_bytes(&zip(&[("../evil.txt", b"evil")]), 1_000, 10_000);
        let error = staged.result.unwrap_err().to_string();
        assert!(error.contains("invalid entry path"), "{error}");
        assert!(staged.files.is_empty());
    }

    #[test]
    fn size_labels_name_megabytes_or_bytes() {
        assert_eq!(size_label(FILE_LIMIT), "250 MB");
        assert_eq!(size_label(ARCHIVE_LIMIT), "1024 MB");
        assert_eq!(size_label(150), "150-byte");
    }
}

#[cfg(test)]
mod cookie_tests {
    use super::set_cookie;

    #[test]
    fn set_cookie_headers_keep_name_and_value_and_notice_deletion() {
        assert_eq!(
            set_cookie("JSESSIONID=s_Kdv.psat02; Path=/Satellite; Secure; HttpOnly"),
            Some(("JSESSIONID".into(), "s_Kdv.psat02".into(), false))
        );
        assert_eq!(
            set_cookie(r#"lang="de"; Max-Age=3600"#),
            Some(("lang".into(), "de".into(), false))
        );
        assert_eq!(
            set_cookie("lang=de; Max-Age=0"),
            Some(("lang".into(), "de".into(), true))
        );
        assert_eq!(
            set_cookie("lang=; Path=/"),
            Some(("lang".into(), String::new(), true))
        );
        for invalid in ["no-equals-sign", "=value", "a b=c", "name=va\"lue x"] {
            assert_eq!(set_cookie(invalid), None, "{invalid}");
        }
    }
}

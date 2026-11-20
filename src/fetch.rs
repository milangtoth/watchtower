use crate::domain::{Document, ReadFailure};
use anyhow::{Context, Result};
use reqwest::{Client, Response, StatusCode, Url, header};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::{
    io::AsyncWriteExt,
    sync::Mutex,
    time::{Instant, sleep_until},
};

const PAGE_LIMIT: usize = 8 * 1024 * 1024;
/// The sitemap protocol allows 50 MB per uncompressed file.
const UNPACKED_PAGE_LIMIT: usize = 64 * 1024 * 1024;
const FILE_LIMIT: u64 = 250 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;

#[derive(Clone)]
pub struct Fetcher {
    client: Client,
    next_request: Arc<Mutex<HashMap<String, Instant>>>,
    files: PathBuf,
}

pub struct PortalHttp {
    fetcher: Fetcher,
    portal_host: String,
}

#[derive(Debug)]
pub struct Page {
    pub url: Url,
    pub body: String,
}

#[derive(Debug)]
pub(crate) enum DocumentResponse {
    Files(Vec<Document>),
    Html(Page),
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
        })
    }

    pub fn portal(&self, url: &Url) -> PortalHttp {
        PortalHttp {
            fetcher: self.clone(),
            portal_host: url.host_str().unwrap_or_default().into(),
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
    async fn request(&self, url: Url, phase: &'static str) -> Result<Response, ReadFailure> {
        self.request_unless_seen(url, phase, None)
            .await?
            .ok_or_else(|| ReadFailure::Incomplete("Portal URL was already sampled".into()))
    }

    async fn request_unless_seen(
        &self,
        mut url: Url,
        phase: &'static str,
        seen: Option<&HashSet<Url>>,
    ) -> Result<Option<Response>, ReadFailure> {
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
                    let next = now + Duration::from_secs(1);
                    entries.insert(self.portal_host.clone(), next);
                    entries.insert(host.clone(), next);
                    break;
                }
                drop(entries);
                sleep_until(wait_until).await;
            }
            let response = self
                .fetcher
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|err| transport_error(&url, phase, err))?;
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
                let body = read_text(response, "access-denied response body").await?;
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
            return Ok(Some(response));
        }
        Err(ReadFailure::Incomplete(
            "Portal redirected too many times".into(),
        ))
    }

    pub async fn page(&self, url: Url) -> Result<Page, ReadFailure> {
        Self::read_page(self.request(url, "page headers").await?, "page body").await
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

    async fn read_page(response: Response, phase: &'static str) -> Result<Page, ReadFailure> {
        let url = response.url().clone();
        let body = read_text(response, phase).await?;
        if let Some(reason) = access_page(&body) {
            return Err(ReadFailure::Blocked(reason.into()));
        }
        Ok(Page { url, body })
    }

    pub(crate) async fn documents(&self, url: Url) -> Result<DocumentResponse, ReadFailure> {
        let mut response = self.request(url, "document headers").await?;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("text/html") {
            let destination = response.url().clone();
            let body = read_text(response, "document HTML response body").await?;
            if let Some(reason) = access_page(&body) {
                return Err(ReadFailure::Blocked(reason.into()));
            }
            let host = destination.host_str().unwrap_or_default();
            let path: String = destination.path().chars().take(240).collect();
            tracing::info!(host, path, "Inspecting HTML returned by document link");
            return Ok(DocumentResponse::Html(Page {
                url: destination,
                body,
            }));
        }
        if response
            .content_length()
            .is_some_and(|len| len > FILE_LIMIT)
        {
            return Ok(DocumentResponse::Files(Vec::new()));
        }
        let name = filename(&response);
        let temp = NamedTempFile::new_in(&self.fetcher.files).map_err(file_error)?;
        let mut file = tokio::fs::File::create(temp.path())
            .await
            .map_err(file_error)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut magic = Vec::with_capacity(4);
        let url = response.url().clone();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| transport_error(&url, "document body", err))?
        {
            size += chunk.len() as u64;
            if size > FILE_LIMIT {
                return Ok(DocumentResponse::Files(Vec::new()));
            }
            if magic.len() < 4 {
                magic.extend_from_slice(&chunk[..chunk.len().min(4 - magic.len())]);
            }
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(file_error)?;
        }
        file.flush().await.map_err(file_error)?;
        drop(file);
        let directory = self.fetcher.files.clone();
        let archive = name.to_ascii_lowercase().ends_with(".zip")
            || content_type.contains("application/zip")
            || (!name.contains('.') && magic.starts_with(b"PK\x03\x04"));
        tokio::task::spawn_blocking(move || {
            if archive {
                unpack(temp.path(), &directory)
            } else {
                let sha = format!("{:x}", hash.finalize());
                Ok(vec![persist(temp, &directory, name, size, sha)?])
            }
        })
        .await
        .map_err(file_error)?
        .map(DocumentResponse::Files)
        .map_err(file_error)
    }
}

async fn read_text(mut response: Response, phase: &'static str) -> Result<String, ReadFailure> {
    let mut data = Vec::new();
    let url = response.url().clone();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| transport_error(&url, phase, err))?
    {
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

fn filename(response: &Response) -> String {
    let header_name = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').skip(1).map(str::trim).find_map(|part| {
                part.strip_prefix("filename=")
                    .map(|value| value.trim_matches('"'))
            })
        });
    header_name
        .or_else(|| response.url().path_segments().and_then(Iterator::last))
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .filter(|name| !name.is_empty())
        .unwrap_or("document")
        .to_owned()
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

fn unpack(path: &Path, directory: &Path) -> Result<Vec<Document>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    anyhow::ensure!(
        archive.len() <= MAX_ARCHIVE_ENTRIES,
        "ZIP contains too many entries"
    );
    let mut expanded = 0_u64;
    let mut docs = Vec::new();
    let mut buffer = [0; 64 * 1024];
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() || entry.size() > FILE_LIMIT {
            continue;
        }
        let name = entry
            .enclosed_name()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .context("ZIP contains an invalid entry path")?;
        let mut temp = NamedTempFile::new_in(directory)?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        loop {
            let length = entry.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            size += length as u64;
            expanded += length as u64;
            anyhow::ensure!(
                expanded <= 1024 * 1024 * 1024,
                "ZIP exceeds the total decompression limit"
            );
            if size > FILE_LIMIT {
                break;
            }
            hash.update(&buffer[..length]);
            temp.write_all(&buffer[..length])?;
        }
        if size <= FILE_LIMIT {
            temp.flush()?;
            docs.push(persist(
                temp,
                directory,
                name,
                size,
                format!("{:x}", hash.finalize()),
            )?);
        }
    }
    Ok(docs)
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

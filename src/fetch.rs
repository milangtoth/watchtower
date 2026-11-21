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
/// The most bytes one ZIP may expand to, counted as they are decompressed.
const ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;
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

    /// Reads a page like `page`, keeping the status and content type for probing.
    pub(crate) async fn sample(&self, url: Url) -> Result<Sample, ReadFailure> {
        let response = self.request(url, "probe headers").await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let page = Self::read_page(response, "probe body").await?;
        Ok(Sample {
            url: page.url,
            status,
            content_type,
            body: page.body,
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
        let url = response.url().clone();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| transport_error(&url, "document body", err))?
        {
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

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::fs::{self, FileTimes};
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const DNS_ROOT: &str = "next-client-root.733702.xyz";
const FALLBACK_ROOT_HOST: &str = "81.70.189.57";
const SEER2_PATH: &str = "/seer2";
const SEER2_MEE_URL: &str = "http://seer2.61.com";
const FLASH_POLICY_PATH: &str = "/crossdomain.xml";
const FLASH_POLICY_DATA: &str = "<?xml version=\"1.0\"?><!DOCTYPE cross-domain-policy SYSTEM \"http://www.macromedia.com/xml/dtds/cross-domain-policy.dtd\"><cross-domain-policy><allow-access-from domain=\"*\" /></cross-domain-policy>";
const MAGIC_PATH: &str = "/seer2-next-client-hello";
const FAVICON_PATH: &str = "/favicon.ico";
const BLOOM_PATH: &str = "/config/bloom-path.data";
const LOCAL_ENTRY_PATH: &str = "/seer2/play-local.html";
const CLIENT_SWF_PATH: &str = "/seer2/Client.swf";
const LOAD_FAILED_PREFIX: &str =
    "\u{6e38}\u{620f}\u{52a0}\u{8f7d}\u{5931}\u{8d25}\u{ff0c}\u{8bf7}\u{68c0}\u{67e5}\u{7f51}\u{7edc}\u{540e}\u{91cd}\u{8bd5}\u{3002}";

pub type LoadFailureNotifier = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone)]
struct Bloom {
    func_num: usize,
    bits: Vec<bool>,
}

struct ServerState {
    root_url: String,
    bloom: Bloom,
    cache_dir: PathBuf,
    file_locks: Mutex<HashSet<String>>,
    metrics: Arc<CacheMetrics>,
    load_failure_notifier: Option<LoadFailureNotifier>,
    load_failure_reported: Mutex<bool>,
}

pub struct HttpServer {
    address: SocketAddr,
    _state: Arc<ServerState>,
    metrics: Arc<CacheMetrics>,
}

#[derive(Default)]
pub struct CacheMetrics {
    hit: AtomicU64,
    expired: AtomicU64,
    fetch: AtomicU64,
    cached: AtomicU64,
    checked: AtomicU64,
}

#[derive(Clone, Copy)]
enum CacheMetric {
    Hit,
    Expired,
    Fetch,
    Cached,
    Checked,
}

struct Request {
    path: String,
    query: Option<String>,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct RemoteResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl RemoteResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

impl HttpServer {
    pub fn start(
        app_data_dir: PathBuf,
        load_failure_notifier: Option<LoadFailureNotifier>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let state = Arc::new(create_state(app_data_dir, load_failure_notifier)?);
        let metrics = Arc::clone(&state.metrics);
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let server_state = Arc::clone(&state);

        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let state = Arc::clone(&server_state);
                        thread::spawn(move || handle_connection(state, stream));
                    }
                    Err(err) => log::warn!("Seer2 local server accept failed: {}", err),
                }
            }
        });

        log::info!("Seer2 local server listening on http://{}", address);
        Ok(Self {
            address,
            _state: state,
            metrics,
        })
    }

    pub fn movie_url(&self) -> String {
        format!("http://{}{}", self.address, CLIENT_SWF_PATH)
    }

    pub fn metrics(&self) -> Arc<CacheMetrics> {
        Arc::clone(&self.metrics)
    }
}

impl CacheMetrics {
    fn increment(&self, metric: CacheMetric) {
        match metric {
            CacheMetric::Hit => &self.hit,
            CacheMetric::Expired => &self.expired,
            CacheMetric::Fetch => &self.fetch,
            CacheMetric::Cached => &self.cached,
            CacheMetric::Checked => &self.checked,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot_text(&self) -> String {
        format!(
            "hit:{}\nexpired:{}\nfetch:{}\ncached:{}\nchecked:{}",
            self.hit.load(Ordering::Relaxed),
            self.expired.load(Ordering::Relaxed),
            self.fetch.load(Ordering::Relaxed),
            self.cached.load(Ordering::Relaxed),
            self.checked.load(Ordering::Relaxed),
        )
    }
}

fn create_state(
    app_data_dir: PathBuf,
    load_failure_notifier: Option<LoadFailureNotifier>,
) -> Result<ServerState, Box<dyn std::error::Error + Send + Sync>> {
    let root_url = resolve_root_url()?;
    let bloom_text = load_bloom_text(&root_url)?;
    let bloom =
        Bloom::parse(&bloom_text).map_err(|err| format!("version file parse failed: {err}"))?;

    let version_path = format!("/version/seer2-next-client/v{}", env!("CARGO_PKG_VERSION"));
    if !bloom.contains(&version_path) {
        return Err("current client version has been disabled".into());
    }

    let cache_dir = app_data_dir.join("gamecache");
    fs::create_dir_all(&cache_dir)
        .map_err(|err| format!("cache dir init failed: {}: {err}", cache_dir.display()))?;
    log::info!("Seer2 cache directory: {}", cache_dir.display());

    Ok(ServerState {
        root_url,
        bloom,
        cache_dir,
        file_locks: Mutex::new(HashSet::new()),
        metrics: Arc::new(CacheMetrics::default()),
        load_failure_notifier,
        load_failure_reported: Mutex::new(false),
    })
}

fn load_bloom_text(root_url: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{root_url}{BLOOM_PATH}");
    let mut last_error = None;

    for attempt in 1..=3 {
        match http_get(&url).and_then(|response| {
            if (200..300).contains(&response.status) {
                Ok(response.body)
            } else {
                Err(format!("HTTP {}: {}", response.status, url).into())
            }
        }) {
            Ok(bytes) => return Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(err) => {
                log::warn!(
                    "Seer2 version file load failed attempt {}: {}",
                    attempt,
                    err
                );
                last_error = Some(err.to_string());
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    Err(format!(
        "version file load failed: {}",
        last_error.unwrap_or_else(|| "unknown error".into())
    )
    .into())
}

impl Bloom {
    fn parse(data: &str) -> Result<Self, String> {
        let split: Vec<&str> = data.lines().collect();
        let func_num = split
            .get(1)
            .ok_or("missing bloom function count")?
            .trim()
            .parse::<usize>()
            .map_err(|err| err.to_string())?;
        let bytes = STANDARD
            .decode(split.get(2).ok_or("missing bloom bitset")?.trim())
            .map_err(|err| err.to_string())?;
        let mut bits = Vec::with_capacity(bytes.len() * 8);
        for byte in bytes {
            for index in 0..8 {
                bits.push(((byte >> index) & 1) == 1);
            }
        }
        if bits.is_empty() {
            return Err("empty bloom bitset".into());
        }
        Ok(Self { func_num, bits })
    }

    fn contains(&self, data: &str) -> bool {
        let hash = md5_hex(data.as_bytes());
        let hash1 = parse_hash_part(&hash[0..8]) ^ parse_hash_part(&hash[8..16]);
        let hash2 = parse_hash_part(&hash[16..24]) ^ parse_hash_part(&hash[24..32]);
        let mut combined_hash = hash1 as u64;

        for _ in 0..self.func_num {
            combined_hash &= 0xffff_ffff;
            if !self.bits[(combined_hash % self.bits.len() as u64) as usize] {
                return false;
            }
            combined_hash = combined_hash.wrapping_add(hash2 as u64);
        }
        true
    }
}

fn handle_connection(state: Arc<ServerState>, mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let response = match read_http_request(&mut stream) {
        Ok(Some((method, target))) => {
            let is_head = method.eq_ignore_ascii_case("HEAD");
            match handle_target(Arc::clone(&state), &target) {
                Ok(response) => to_http_response(response, is_head),
                Err(err) => to_http_response(
                    response(
                        500,
                        "text/plain; charset=utf-8",
                        format!("server error: {err}").into_bytes(),
                    ),
                    is_head,
                ),
            }
        }
        Ok(None) => to_http_response(
            response(400, "text/plain; charset=utf-8", b"bad request".to_vec()),
            false,
        ),
        Err(err) => to_http_response(
            response(
                500,
                "text/plain; charset=utf-8",
                format!("request read error: {err}").into_bytes(),
            ),
            false,
        ),
    };

    let _ = stream.write_all(&response);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn read_http_request(
    stream: &mut TcpStream,
) -> Result<Option<(String, String)>, Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 1024];

    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let len = stream.read(&mut chunk)?;
        if len == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..len]);
        if buffer.len() > 64 * 1024 {
            return Ok(None);
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let first_line = text.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() {
        return Ok(None);
    }

    Ok(Some((method.to_string(), target.to_string())))
}

fn handle_target(
    state: Arc<ServerState>,
    target: &str,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    if let Ok(url) = Url::parse(target) {
        if let Some(host) = url.host_str() {
            if !is_local_virtual_host(host) {
                return fetch_absolute_proxy(url);
            }
        }

        return handle_request(
            state,
            Request {
                path: normalized_path(url.path()),
                query: url.query().map(ToOwned::to_owned),
            },
        );
    }

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    handle_request(
        state,
        Request {
            path: normalized_path(path),
            query: (!query.is_empty()).then(|| query.to_string()),
        },
    )
}

fn normalized_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn fetch_absolute_proxy(url: Url) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    if url.scheme() != "http" {
        return Ok(response(
            403,
            "text/plain; charset=utf-8",
            b"unsupported proxy scheme".to_vec(),
        ));
    }

    let remote = http_get(url.as_str())?;
    let content_type = remote
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .to_string();
    Ok(response(remote.status, &content_type, remote.body))
}

fn http_get(url: &str) -> Result<RemoteResponse, Box<dyn std::error::Error + Send + Sync>> {
    http_get_with_headers(url, &[])
}

fn http_get_with_headers(
    url: &str,
    extra_headers: &[(&str, String)],
) -> Result<RemoteResponse, Box<dyn std::error::Error + Send + Sync>> {
    let mut current = Url::parse(url)?;

    for _ in 0..5 {
        let response = http_get_once(&current, extra_headers)?;
        let redirect = if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            response.header("location").map(ToOwned::to_owned)
        } else {
            None
        };

        if let Some(location) = redirect {
            current = current.join(&location)?;
            continue;
        }

        return Ok(response);
    }

    Err(format!("too many HTTP redirects: {url}").into())
}

fn http_get_once(
    url: &Url,
    extra_headers: &[(&str, String)],
) -> Result<RemoteResponse, Box<dyn std::error::Error + Send + Sync>> {
    if url.scheme() != "http" {
        return Err(format!("unsupported HTTP scheme: {}", url.scheme()).into());
    }

    let host = url.host_str().ok_or("missing HTTP host")?;
    let port = url.port_or_known_default().unwrap_or(80);
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or("HTTP host did not resolve")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    let target = match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    };
    let mut host_header = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if let Some(port) = url.port() {
        host_header.push(':');
        host_header.push_str(&port.to_string());
    }

    let mut request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: ruffle-android-seer2/{}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    for (name, value) in extra_headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    parse_http_response(bytes)
}

fn parse_http_response(
    bytes: Vec<u8>,
) -> Result<RemoteResponse, Box<dyn std::error::Error + Send + Sync>> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let header_text = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().ok_or("missing HTTP status line")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status")?
        .parse::<u16>()?;

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let mut body = bytes[header_end + 4..].to_vec();
    if headers
        .get("transfer-encoding")
        .map(|value| {
            value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false)
    {
        body = decode_chunked_body(&body)?;
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        body.truncate(length);
    }

    Ok(RemoteResponse {
        status,
        headers,
        body,
    })
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut output = Vec::new();
    let mut cursor = 0;

    loop {
        let line_end = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|position| cursor + position)
            .ok_or("malformed chunked response")?;
        let size_line = std::str::from_utf8(&body[cursor..line_end])?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)?;
        cursor = line_end + 2;

        if size == 0 {
            return Ok(output);
        }
        if cursor + size > body.len() {
            return Err("truncated chunked response".into());
        }

        output.extend_from_slice(&body[cursor..cursor + size]);
        cursor += size;

        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err("malformed chunked response terminator".into());
        }
        cursor += 2;
    }
}

fn is_local_virtual_host(host: &str) -> bool {
    matches!(
        host,
        "127.0.0.1"
            | "localhost"
            | "client.localhost"
            | "ruffle.localhost"
            | "seer2.localhost"
            | "local.client"
    )
}

fn handle_request(
    state: Arc<ServerState>,
    request: Request,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let url_path = request.path.clone();
    if url_path == MAGIC_PATH {
        return Ok(response(
            200,
            "application/json; charset=utf-8",
            format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION")).into_bytes(),
        ));
    }
    if url_path == FAVICON_PATH {
        return Ok(response(204, "image/x-icon", Vec::new()));
    }
    if url_path == FLASH_POLICY_PATH {
        return Ok(response(
            200,
            "application/xml; charset=utf-8",
            FLASH_POLICY_DATA.as_bytes().to_vec(),
        ));
    }
    if url_path.ends_with('/') || url_path.ends_with('\\') || !url_path.starts_with(SEER2_PATH) {
        return Ok(response(
            403,
            "text/plain; charset=utf-8",
            b"not a valid path".to_vec(),
        ));
    }
    if url_path == LOCAL_ENTRY_PATH && !query_has_key(request.query.as_deref(), "version") {
        return Ok(redirect(entry_url(&local_origin())));
    }

    let bloom_path = &url_path[SEER2_PATH.len()..];
    if bloom_path == BLOOM_PATH {
        return Ok(response(403, "text/plain; charset=utf-8", Vec::new()));
    }

    let path_hit_bloom = state.bloom.contains(bloom_path);
    let cache_path = state.cache_dir.join(format!(
        "{}_{}",
        md5_hex(&url_path.as_bytes()[1..]),
        url_path.len()
    ));

    if !is_file_locked(&state, bloom_path) {
        if let Ok(stats) = fs::metadata(&cache_path) {
            if stats.is_file() {
                let modified = stats.modified().unwrap_or(UNIX_EPOCH);
                if path_hit_bloom {
                    let bloom_version_path = format!("{bloom_path}?v={}", mtime_ms(modified));
                    if state.bloom.contains(&bloom_version_path) {
                        if let Ok(body) = read_cache(&cache_path) {
                            state.metrics.increment(CacheMetric::Hit);
                            return Ok(cache_response(&url_path, body, modified));
                        }
                    } else {
                        log::info!("Seer2 cache expired: {}", url_path);
                        state.metrics.increment(CacheMetric::Expired);
                    }
                } else if let Ok(body) = read_cache(&cache_path) {
                    state.metrics.increment(CacheMetric::Hit);
                    spawn_async_cache_check(
                        Arc::clone(&state),
                        bloom_path.to_string(),
                        cache_path.clone(),
                        modified,
                    );
                    return Ok(cache_response(&url_path, body, modified));
                }
            }
        }
    }

    fetch_and_cache(
        state,
        &url_path,
        bloom_path,
        request.query.as_deref(),
        path_hit_bloom,
        cache_path,
    )
}

fn fetch_and_cache(
    state: Arc<ServerState>,
    url_path: &str,
    bloom_path: &str,
    query: Option<&str>,
    path_hit_bloom: bool,
    cache_path: PathBuf,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let base = if path_hit_bloom {
        state.root_url.as_str()
    } else {
        SEER2_MEE_URL
    };
    let file_url = match query {
        Some(query) => format!("{base}{bloom_path}?{query}"),
        None => format!("{base}{bloom_path}"),
    };
    log::info!("Seer2 fetch: {}", file_url);
    state.metrics.increment(CacheMetric::Fetch);

    let remote = match http_get(&file_url) {
        Ok(remote) => remote,
        Err(err) => {
            if is_critical_game_path(url_path) {
                notify_game_load_failure(&state, format!("{LOAD_FAILED_PREFIX}\n{err}"));
            }
            return Err(err);
        }
    };
    let status = remote.status;
    if status >= 400 && is_critical_game_path(url_path) {
        notify_game_load_failure(
            &state,
            format!("{LOAD_FAILED_PREFIX}\nHTTP {status}: {url_path}"),
        );
    }
    let modified = remote.header("last-modified").and_then(parse_http_date);
    let body = remote.body;

    if status == 200 {
        let state_for_write = Arc::clone(&state);
        let bloom_path = bloom_path.to_string();
        let body_for_write = body.clone();
        thread::spawn(move || {
            if let Err(err) = write_cache(
                &state_for_write,
                &bloom_path,
                &cache_path,
                &body_for_write,
                modified,
            ) {
                log::warn!("Seer2 cache write error: {}", err);
            }
        });
    }

    let mut res = response(status, content_type(url_path), body);
    res.headers.push(("x-hit".into(), "fetch".into()));
    Ok(res)
}

fn spawn_async_cache_check(
    state: Arc<ServerState>,
    bloom_path: String,
    cache_path: PathBuf,
    modified: SystemTime,
) {
    thread::spawn(move || {
        if let Err(err) = async_check_cache(state, &bloom_path, &cache_path, modified) {
            log::warn!("Seer2 async cache check error: {}", err);
        }
    });
}

fn async_check_cache(
    state: Arc<ServerState>,
    bloom_path: &str,
    cache_path: &Path,
    modified: SystemTime,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    state.metrics.increment(CacheMetric::Checked);
    let file_url = format!("{SEER2_MEE_URL}{bloom_path}");
    let remote = http_get_with_headers(
        &file_url,
        &[("If-Modified-Since", format_http_date(modified))],
    )?;
    if remote.status == 304 {
        log::info!("Seer2 cache unchanged: {}", bloom_path);
        return Ok(());
    }
    if remote.status != 200 {
        log::info!(
            "Seer2 cache check returned HTTP {}: {}",
            remote.status,
            bloom_path
        );
        return Ok(());
    }

    let remote_modified = remote.header("last-modified").and_then(parse_http_date);
    if remote_modified
        .map(|time| mtime_ms(time) == mtime_ms(modified))
        .unwrap_or(false)
    {
        log::info!("Seer2 cache mtime unchanged: {}", bloom_path);
        return Ok(());
    }

    log::info!("Seer2 cache changed: {}", bloom_path);
    write_cache(
        &state,
        bloom_path,
        cache_path,
        &remote.body,
        remote_modified,
    )
}

fn notify_game_load_failure(state: &ServerState, message: String) {
    let Some(notifier) = &state.load_failure_notifier else {
        return;
    };
    let should_notify = state
        .load_failure_reported
        .lock()
        .map(|mut reported| {
            if *reported {
                false
            } else {
                *reported = true;
                true
            }
        })
        .unwrap_or(false);
    if should_notify {
        notifier(message);
    }
}

fn is_critical_game_path(url_path: &str) -> bool {
    url_path == LOCAL_ENTRY_PATH || url_path.eq_ignore_ascii_case(CLIENT_SWF_PATH)
}

fn write_cache(
    state: &ServerState,
    url_path: &str,
    file_path: &Path,
    body: &[u8],
    modified: Option<SystemTime>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let lock_key = url_path.to_string();
    {
        let mut locks = state.file_locks.lock().map_err(|_| "file lock poisoned")?;
        if !locks.insert(lock_key.clone()) {
            return Ok(());
        }
    }

    let result = (|| {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(file_path, body)?;
        if let Some(modified) = modified {
            fs::File::options()
                .write(true)
                .open(file_path)?
                .set_times(FileTimes::new().set_modified(modified))?;
        }
        Ok(())
    })();

    if let Ok(mut locks) = state.file_locks.lock() {
        locks.remove(&lock_key);
    }
    if result.is_ok() {
        state.metrics.increment(CacheMetric::Cached);
    }
    result
}

fn read_cache(file_path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(fs::read(file_path)?)
}

fn cache_response(url_path: &str, body: Vec<u8>, modified: SystemTime) -> Response {
    let mut res = response(200, content_type(url_path), body);
    res.headers.push(("x-hit".into(), "file".into()));
    res.headers
        .push(("x-cache-mtime-ms".into(), mtime_ms(modified).to_string()));
    res
}

fn response(status: u16, content_type: &str, body: Vec<u8>) -> Response {
    Response {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body,
    }
}

fn redirect(location: String) -> Response {
    Response {
        status: 302,
        headers: vec![("location".into(), location)],
        body: Vec::new(),
    }
}

fn to_http_response(response: Response, is_head: bool) -> Vec<u8> {
    let body_len = response.body.len();
    let status_text = status_text(response.status);
    let mut output = format!(
        "HTTP/1.1 {} {}\r\naccess-control-allow-origin: *\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status, status_text, body_len
    )
    .into_bytes();

    for (name, value) in response.headers {
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(b": ");
        output.extend_from_slice(value.as_bytes());
        output.extend_from_slice(b"\r\n");
    }

    output.extend_from_slice(b"\r\n");
    if !is_head {
        output.extend_from_slice(&response.body);
    }
    output
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn entry_url(origin: &str) -> String {
    format!(
        "{origin}{LOCAL_ENTRY_PATH}?version={}&platform={}&arch={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn local_origin() -> String {
    "http://127.0.0.1".to_string()
}

fn query_has_key(query: Option<&str>, key: &str) -> bool {
    query
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('=').map(|(name, _)| name).or(Some(pair)))
        .any(|name| name == key)
}

fn is_file_locked(state: &ServerState, bloom_path: &str) -> bool {
    state
        .file_locks
        .lock()
        .map(|locks| locks.contains(bloom_path))
        .unwrap_or(false)
}

fn resolve_root_url() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let address = match (DNS_ROOT, 80).to_socket_addrs() {
        Ok(mut addrs) => addrs.next().map(|addr| addr.ip()),
        Err(err) => {
            log::warn!(
                "Seer2 DNS lookup failed: {}: {}; using fallback {}",
                DNS_ROOT,
                err,
                FALLBACK_ROOT_HOST
            );
            None
        }
    };
    let host = match address {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => format!("[{ip}]"),
        None => FALLBACK_ROOT_HOST.to_string(),
    };
    log::info!("Seer2 root resolved: {} {}", DNS_ROOT, host);
    Ok(format!("http://{host}{SEER2_PATH}"))
}

fn parse_hash_part(part: &str) -> u32 {
    u32::from_str_radix(part, 16).unwrap_or(0)
}

fn md5_hex(data: &[u8]) -> String {
    md5_digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn md5_digest(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut message = data.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0 = 0x67452301_u32;
    let mut b0 = 0xefcdab89_u32;
    let mut c0 = 0x98badcfe_u32;
    let mut d0 = 0x10325476_u32;

    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (i, word) in words.iter_mut().enumerate() {
            let offset = i * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | (!d)), (7 * i) % 16)
            };

            let next = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(words[g])
                    .rotate_left(S[i]),
            );
            a = next;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0_u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());
    digest
}

fn mtime_ms(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn format_http_date(time: SystemTime) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    let weekday = (days + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[weekday],
        day,
        MONTHS[(month - 1) as usize],
        year,
        hour,
        minute,
        second
    )
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let (_, rest) = value.trim().split_once(',')?;
    let mut parts = rest.split_whitespace();
    let day = parts.next()?.parse::<u32>().ok()?;
    let month = parse_http_month(parts.next()?)?;
    let year = parts.next()?.parse::<i32>().ok()?;
    let time = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<u32>().ok()?;
    let minute = time_parts.next()?.parse::<u32>().ok()?;
    let second = time_parts.next()?.parse::<u32>().ok()?;
    if time_parts.next().is_some()
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let timestamp = unix_timestamp(year, month, day, hour, minute, second)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(timestamp as u64))
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    (year as i32, month as u32, day as u32)
}

fn parse_http_month(value: &str) -> Option<u32> {
    match value {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn unix_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    let seconds = i64::from(days) * 86_400
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second);
    (seconds >= 0).then_some(seconds)
}

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "html" | "htm" | "shtml" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "swf" => "application/x-shockwave-flash",
        "wasm" => "application/wasm",
        "map" => "application/json; charset=utf-8",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{md5_hex, parse_http_date};

    #[test]
    fn md5_matches_known_value() {
        assert_eq!(md5_hex(b"/Client.swf"), "977aa655fe7d23685d06345dcf2fe114");
    }

    #[test]
    fn parses_last_modified_http_date() {
        let modified = parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(super::mtime_ms(modified), 1_445_412_480_000);
    }

    #[test]
    fn formats_last_modified_http_date() {
        let modified = UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        assert_eq!(
            super::format_http_date(modified),
            "Wed, 21 Oct 2015 07:28:00 GMT"
        );
    }
}

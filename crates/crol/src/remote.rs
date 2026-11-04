//! Schemas from URLs and crates (`SPEC.md` 4.1): fetched over HTTPS only,
//! cached, and used from the cache when offline.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use semver::{Version, VersionReq};

use crate::error::Error;
use crate::mode::Mode;
use crate::module_path::current_if_empty;

/// How long a request may take.
#[cfg(any(feature = "remote-schemas", feature = "remote-schemas-async"))]
const TIMEOUT: Duration = Duration::from_secs(10);
/// The most a schema file may hold.
const MAX_SCHEMA: u64 = 16 << 20;
/// The most a downloaded crate may hold, packed and unpacked.
const MAX_CRATE: u64 = 64 << 20;
/// How long a failed resolution is remembered before it is tried again.
const RETRY: Duration = Duration::from_secs(60);
/// The schema path a crate's `[package.metadata.crol]` table defaults to.
const DEFAULT_SCHEMA: &str = "crol/schema.crov";
#[cfg(any(feature = "remote-schemas", feature = "remote-schemas-async"))]
const USER_AGENT: &str = concat!(
    "crol/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/DreamLogics/crol)"
);

/// Where schemas are cached and Cargo keeps its registry.
pub(crate) struct Dirs {
    pub(crate) cache: PathBuf,
    pub(crate) cargo_home: Option<PathBuf>,
}

impl Dirs {
    fn from_env() -> Result<Self, Error> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let cache = match var("CROL_CACHE_DIR") {
            Some(dir) => dir,
            None => {
                let base = if cfg!(windows) {
                    var("LOCALAPPDATA")
                } else if cfg!(target_os = "macos") {
                    var("HOME").map(|h| h.join("Library/Caches"))
                } else {
                    var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
                };
                base.map(|b| b.join("crol")).ok_or_else(|| {
                    Error::new("schema: there is no cache directory; set `CROL_CACHE_DIR`")
                })?
            }
        };
        let cargo_home = var("CARGO_HOME").or_else(|| {
            var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| h.join(".cargo"))
        });
        Ok(Self { cache, cargo_home })
    }
}

/// An HTTPS client.
pub(crate) trait Http: Sync {
    /// Gets a URL, redirects included, reading at most `limit` bytes.
    fn get(&self, url: &str, limit: u64) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

/// The client for when fetching is off: it is never asked.
pub(crate) struct Offline;

impl Http for Offline {
    fn get(&self, _: &str, _: u64) -> impl Future<Output = Result<Vec<u8>, String>> + Send {
        std::future::ready(Err("fetching schemas is off".to_owned()))
    }
}

/// A blocking client, with `ureq`.
#[cfg(feature = "remote-schemas")]
pub(crate) struct Blocking;

#[cfg(feature = "remote-schemas")]
impl Http for Blocking {
    fn get(&self, url: &str, limit: u64) -> impl Future<Output = Result<Vec<u8>, String>> + Send {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let result = agent
            .get(url)
            .call()
            .and_then(|r| r.into_body().with_config().limit(limit).read_to_vec())
            .map_err(|e| e.to_string());
        std::future::ready(result)
    }
}

/// An async client, with `reqwest`; it needs a Tokio runtime.
#[cfg(feature = "remote-schemas-async")]
pub(crate) struct Async(reqwest::Client);

#[cfg(feature = "remote-schemas-async")]
impl Async {
    fn new() -> Result<Self, Error> {
        // reqwest takes rustls's process-wide provider; another may be
        // installed already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .https_only(true)
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map(Self)
            .map_err(|e| Error::new(format!("schema: cannot start an HTTPS client: {e}")))
    }
}

#[cfg(feature = "remote-schemas-async")]
impl Http for Async {
    fn get(&self, url: &str, limit: u64) -> impl Future<Output = Result<Vec<u8>, String>> + Send {
        let request = self.0.get(url);
        async move {
            let mut response = request
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| e.to_string())?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                if (body.len() + chunk.len()) as u64 > limit {
                    return Err(format!("the response is larger than {limit} bytes"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        }
    }
}

/// Runs a future whose client never waits: [`Blocking`] or [`Offline`].
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => unreachable!("a blocking client never waits"),
    }
}

type Memo = Mutex<HashMap<(String, PathBuf, bool), (Instant, Result<PathBuf, Error>)>>;

/// Results are remembered for the process, failures for a minute.
fn memo() -> &'static Memo {
    static MEMO: OnceLock<Memo> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

fn remembered(key: &(String, PathBuf, bool)) -> Option<Result<PathBuf, Error>> {
    let memo = memo().lock().unwrap_or_else(|e| e.into_inner());
    let (at, result) = memo.get(key)?;
    (result.is_ok() || at.elapsed() < RETRY).then(|| result.clone())
}

fn remember(key: (String, PathBuf, bool), result: &Result<PathBuf, Error>) {
    memo()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), result.clone()));
}

/// The schema file a URL or crate names, for a document or project in
/// `dir`, blocking. Without `fetch`, or without a blocking client, only
/// cached copies and local crates are used.
pub(crate) fn resolve(source: &str, dir: &Path, fetch: bool) -> Result<PathBuf, Error> {
    let fetch = fetch
        && cfg!(any(
            feature = "remote-schemas",
            feature = "remote-schemas-async"
        ));
    let key = (source.to_owned(), dir.to_path_buf(), fetch);
    if let Some(result) = remembered(&key) {
        return result;
    }
    let result = Dirs::from_env().and_then(|dirs| {
        #[cfg(feature = "remote-schemas")]
        if fetch {
            return block_on(resolve_in(source, dir, true, &dirs, &Blocking));
        }
        // With only the async client, on a runtime of its own.
        #[cfg(all(feature = "remote-schemas-async", not(feature = "remote-schemas")))]
        if fetch {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::new(format!("schema: cannot start a runtime: {e}")))?;
            let http = Async::new()?;
            return runtime.block_on(resolve_in(source, dir, true, &dirs, &http));
        }
        block_on(resolve_in(source, dir, false, &dirs, &Offline))
    });
    remember_fetched(key, &result);
    result
}

/// Remembers a result; one fetched is also what the cache now gives.
fn remember_fetched(key: (String, PathBuf, bool), result: &Result<PathBuf, Error>) {
    if key.2 && result.is_ok() {
        remember((key.0.clone(), key.1.clone(), false), result);
    }
    remember(key, result);
}

/// [`resolve`], without blocking on the network; it needs a Tokio runtime.
#[cfg(feature = "remote-schemas-async")]
pub(crate) async fn resolve_async(source: &str, dir: &Path, fetch: bool) -> Result<PathBuf, Error> {
    let key = (source.to_owned(), dir.to_path_buf(), fetch);
    if let Some(result) = remembered(&key) {
        return result;
    }
    let result = match (Dirs::from_env(), fetch) {
        (Err(e), _) => Err(e),
        (Ok(dirs), true) => match Async::new() {
            Ok(http) => resolve_in(source, dir, true, &dirs, &http).await,
            Err(e) => Err(e),
        },
        (Ok(dirs), false) => resolve_in(source, dir, false, &dirs, &Offline).await,
    };
    remember_fetched(key, &result);
    result
}

pub(crate) async fn resolve_in(
    source: &str,
    dir: &Path,
    fetch: bool,
    dirs: &Dirs,
    http: &impl Http,
) -> Result<PathBuf, Error> {
    if source.starts_with("https://") {
        url(source, fetch, dirs, http).await
    } else if let Some(spec) = source.strip_prefix("crate:") {
        krate(spec, dir, fetch, dirs, http).await
    } else {
        Err(Error::new(format!(
            "schema: `{source}` is neither a URL (`https://...`) nor a crate (`crate:name@version`)"
        )))
    }
}

/// A URL's schema: fetched when allowed, else, or when that fails, the
/// cached copy.
async fn url(url: &str, fetch: bool, dirs: &Dirs, http: &impl Http) -> Result<PathBuf, Error> {
    Mode::Value.check(url)?;
    let hash = blake3::hash(url.as_bytes()).to_hex();
    let path = dirs.cache.join("urls").join(format!("{hash}.crov"));
    let mut failed = None;
    if fetch {
        match http.get(url, MAX_SCHEMA).await {
            Ok(bytes) => {
                write(&path, &bytes)?;
                return Ok(path);
            }
            Err(e) => failed = Some(e),
        }
    }
    if path.is_file() {
        return Ok(path);
    }
    Err(Error::new(match failed {
        Some(e) => format!("schema: cannot fetch {url}: {e}"),
        None => format!("schema: {url} is not cached, and fetching schemas is off"),
    }))
}

/// A crate's schema, from the first of: this version of crol, for its own
/// schema; the Cargo workspace and its dependencies; Cargo's registry
/// cache; crates.io.
async fn krate(
    spec: &str,
    dir: &Path,
    fetch: bool,
    dirs: &Dirs,
    http: &impl Http,
) -> Result<PathBuf, Error> {
    let (name, req) = spec.split_once('@').unwrap_or((spec, "*"));
    let valid = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if name.is_empty() || name.len() > 64 || !name.chars().all(valid) {
        return Err(Error::new(format!("schema: `{name}` is not a crate name")));
    }
    let req = VersionReq::parse(req)
        .map_err(|e| Error::new(format!("schema: `{req}` is not a version requirement: {e}")))?;
    let own = Version::parse(env!("CARGO_PKG_VERSION")).expect("crol's version");
    if same_name(name, "crol") && req.matches(&own) {
        let path = dirs.cache.join("crates").join(format!("crol-{own}.crov"));
        // A build of this version may have written another schema, so what
        // is there counts only if it is this one.
        let text = crate::schema::build::<crate::schema::SchemaFile>().to_text()?;
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            write(&path, text.as_bytes())?;
        }
        return Ok(path);
    }
    if let Some(found) = in_workspace(name, &req, dir) {
        return found;
    }
    if let Some(found) = in_registry_cache(name, &req, dirs) {
        return found;
    }
    from_crates_io(name, &req, fetch, dirs, http).await
}

/// Cargo treats `-` and `_` in crate names as the same.
fn same_name(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(a, b)| a == b || matches!((a, b), (b'-', b'_') | (b'_', b'-')))
}

/// The schema of the highest matching package `cargo metadata` lists, for
/// a directory in a Cargo workspace.
fn in_workspace(name: &str, req: &VersionReq, dir: &Path) -> Option<Result<PathBuf, Error>> {
    // A relative path has no ancestors above where it starts.
    let dir = std::path::absolute(current_if_empty(dir)).ok()?;
    if !dir.ancestors().any(|d| d.join("Cargo.toml").is_file()) {
        return None;
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let metadata = |extra: &[&str]| {
        let output = Command::new(&cargo)
            .args(["metadata", "--format-version", "1", "--offline"])
            .args(extra)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output.status.success().then_some(output)
    };
    // Without every dependency downloaded, at least the workspace's own.
    let output = metadata(&[]).or_else(|| metadata(&["--no-deps"]))?;
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let (version, package) = metadata["packages"]
        .as_array()?
        .iter()
        .filter(|p| p["name"].as_str().is_some_and(|n| same_name(n, name)))
        .filter_map(|p| {
            let version = Version::parse(p["version"].as_str()?).ok()?;
            req.matches(&version).then_some((version, p))
        })
        .max_by(|a, b| a.0.cmp(&b.0))?;
    let package_dir = Path::new(package["manifest_path"].as_str()?).parent()?;
    let table = package["metadata"].get("crol");
    let schema = table.map(|t| t.get("schema").and_then(|s| s.as_str()));
    Some(schema_path(name, &version, schema).map(|p| package_dir.join(p)))
}

/// The schema of the highest matching version in `~/.cargo/registry/src`.
fn in_registry_cache(name: &str, req: &VersionReq, dirs: &Dirs) -> Option<Result<PathBuf, Error>> {
    let src = dirs.cargo_home.as_ref()?.join("registry").join("src");
    let mut best: Option<(Version, PathBuf)> = None;
    for registry in std::fs::read_dir(src).ok()?.flatten() {
        let Ok(packages) = std::fs::read_dir(registry.path()) else {
            continue;
        };
        for package in packages.flatten() {
            let file_name = package.file_name();
            let Some(dir_name) = file_name.to_str() else {
                continue;
            };
            // `name-1.2.3`; a version may have `-` in it, a name `_`.
            let Some(version) = dir_name
                .get(name.len() + 1..)
                .filter(|_| same_name(&dir_name[..name.len()], name))
                .filter(|_| dir_name.as_bytes()[name.len()] == b'-')
                .and_then(|v| Version::parse(v).ok())
            else {
                continue;
            };
            if req.matches(&version) && best.as_ref().is_none_or(|(b, _)| version > *b) {
                best = Some((version, package.path()));
            }
        }
    }
    let (version, dir) = best?;
    let manifest = match std::fs::read_to_string(dir.join("Cargo.toml")) {
        Ok(text) => text,
        Err(e) => return Some(Err(Error::new(format!("schema: {}: {e}", dir.display())))),
    };
    Some(manifest_schema(name, &version, &manifest).map(|p| dir.join(p)))
}

/// The schema path in a crate's `Cargo.toml`.
fn manifest_schema(name: &str, version: &Version, manifest: &str) -> Result<PathBuf, Error> {
    let manifest: toml::Table = toml::from_str(manifest)
        .map_err(|e| Error::new(format!("schema: the manifest of `{name}` {version}: {e}")))?;
    let table = manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("crol"));
    let schema = table.map(|t| t.get("schema").and_then(|s| s.as_str()));
    schema_path(name, version, schema)
}

/// The schema path a crate's `[package.metadata.crol]` table gives:
/// `None` without the table, `Some(None)` for its default.
fn schema_path(
    name: &str,
    version: &Version,
    table: Option<Option<&str>>,
) -> Result<PathBuf, Error> {
    let Some(schema) = table else {
        return Err(Error::new(format!(
            "schema: `{name}` {version} publishes no schema; it has no `[package.metadata.crol]`"
        )));
    };
    let path = PathBuf::from(schema.unwrap_or(DEFAULT_SCHEMA));
    Mode::Value
        .check_path(&path)
        .map_err(|e| Error::new(format!("schema: `{name}` {version}: {e}")))?;
    if !path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::new(format!(
            "schema: `{name}` {version} names a schema outside its package: `{}`",
            path.display()
        )));
    }
    Ok(path)
}

/// The schema of the highest matching version on crates.io, downloaded
/// once; offline, or without `fetch`, the highest matching one cached.
async fn from_crates_io(
    name: &str,
    req: &VersionReq,
    fetch: bool,
    dirs: &Dirs,
    http: &impl Http,
) -> Result<PathBuf, Error> {
    let cache = dirs.cache.join("crates");
    let mut failed = None;
    if fetch {
        match index_versions(name, http).await {
            Ok(versions) => {
                let Some((name, version)) = versions
                    .into_iter()
                    .filter(|(_, v)| req.matches(v))
                    .max_by(|a, b| a.1.cmp(&b.1))
                else {
                    return Err(Error::new(format!(
                        "schema: no version of `{name}` on crates.io matches `{req}`"
                    )));
                };
                let path = cache.join(format!("{name}-{version}.crov"));
                if path.is_file() {
                    return Ok(path);
                }
                match download(&name, &version, http).await {
                    Ok(text) => {
                        write(&path, &text)?;
                        return Ok(path);
                    }
                    Err(e) => failed = Some(e),
                }
            }
            Err(e) => failed = Some(Error::new(format!("schema: cannot reach crates.io: {e}"))),
        }
    }
    let mut best: Option<(Version, PathBuf)> = None;
    for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".crov"))
        else {
            continue;
        };
        let Some(version) = stem
            .get(name.len() + 1..)
            .filter(|_| same_name(&stem[..name.len()], name))
            .and_then(|v| Version::parse(v).ok())
        else {
            continue;
        };
        if req.matches(&version) && best.as_ref().is_none_or(|(b, _)| version > *b) {
            best = Some((version, path));
        }
    }
    match (best, failed) {
        (Some((_, path)), _) => Ok(path),
        (None, Some(e)) => Err(e),
        (None, None) => Err(Error::new(format!(
            "schema: `{name}` is not cached, and fetching schemas is off"
        ))),
    }
}

/// The versions of a crate that are not yanked, with the name as
/// published, from the crates.io sparse index.
async fn index_versions(name: &str, http: &impl Http) -> Result<Vec<(String, Version)>, String> {
    let lower = name.to_ascii_lowercase();
    let prefix = match lower.len() {
        1 => "1".to_owned(),
        2 => "2".to_owned(),
        3 => format!("3/{}", &lower[..1]),
        _ => format!("{}/{}", &lower[..2], &lower[2..4]),
    };
    let body = http
        .get(
            &format!("https://index.crates.io/{prefix}/{lower}"),
            MAX_CRATE,
        )
        .await?;
    let text = String::from_utf8(body).map_err(|e| e.to_string())?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).ok()?;
            if entry["yanked"].as_bool() == Some(true) {
                return None;
            }
            let version = Version::parse(entry["vers"].as_str()?).ok()?;
            Some((entry["name"].as_str()?.to_owned(), version))
        })
        .collect())
}

/// Downloads a crate and takes its schema out.
async fn download(name: &str, version: &Version, http: &impl Http) -> Result<Vec<u8>, Error> {
    let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
    let packed = http
        .get(&url, MAX_CRATE)
        .await
        .map_err(|e| Error::new(format!("schema: cannot download `{name}` {version}: {e}")))?;
    schema_in_package(name, version, &packed)
}

/// A packaged crate's schema: the file its `Cargo.toml` names. Nothing is
/// unpacked to disk.
pub(crate) fn schema_in_package(
    name: &str,
    version: &Version,
    packed: &[u8],
) -> Result<Vec<u8>, Error> {
    let root = PathBuf::from(format!("{name}-{version}"));
    let broken = |e: std::io::Error| {
        Error::new(format!(
            "schema: the package of `{name}` {version} is broken: {e}"
        ))
    };
    let find = |wanted: &Path| -> Result<Option<Vec<u8>>, Error> {
        let unpacked = flate2::read::GzDecoder::new(packed).take(MAX_CRATE);
        let mut archive = tar::Archive::new(unpacked);
        for entry in archive.entries().map_err(broken)? {
            let entry = entry.map_err(broken)?;
            if entry.path().map_err(broken)? == wanted {
                let mut bytes = Vec::new();
                entry
                    .take(MAX_SCHEMA)
                    .read_to_end(&mut bytes)
                    .map_err(broken)?;
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    };
    let missing = |file: &str| {
        Error::new(format!(
            "schema: the package of `{name}` {version} has no `{file}`"
        ))
    };
    let manifest = find(&root.join("Cargo.toml"))?.ok_or_else(|| missing("Cargo.toml"))?;
    let schema = manifest_schema(name, version, &String::from_utf8_lossy(&manifest))?;
    find(&root.join(&schema))?.ok_or_else(|| missing(&schema.to_string_lossy()))
}

/// Writes a cache file whole: to a temporary file, then renamed.
fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let failed =
        |e: std::io::Error| Error::new(format!("schema: cannot write {}: {e}", path.display()));
    let dir = path.parent().expect("cache files are in a directory");
    std::fs::create_dir_all(dir).map_err(failed)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes).map_err(failed)?;
    std::fs::rename(&temporary, path).map_err(failed)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn dirs(name: &str) -> Dirs {
        let root = std::env::temp_dir().join(format!("crol-remote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Dirs {
            cache: root.join("cache"),
            cargo_home: Some(root.join("cargo")),
        }
    }

    fn file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A directory outside this Cargo workspace.
    fn outside() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn urls_come_from_the_cache_without_fetching() {
        let dirs = dirs("urls");
        let url = "https://example.org/schema.crov";
        let error = block_on(resolve_in(url, &outside(), false, &dirs, &Offline)).unwrap_err();
        assert_eq!(
            error.message(),
            "schema: https://example.org/schema.crov is not cached, and fetching schemas is off"
        );
        let hash = blake3::hash(url.as_bytes()).to_hex();
        let cached = dirs.cache.join("urls").join(format!("{hash}.crov"));
        file(&cached, "Schema {}");
        assert_eq!(
            block_on(resolve_in(url, &outside(), false, &dirs, &Offline)).unwrap(),
            cached
        );
    }

    #[test]
    fn crates_come_from_the_registry_cache() {
        let dirs = dirs("registry");
        let src = dirs
            .cargo_home
            .as_ref()
            .unwrap()
            .join("registry/src/index.crates.io-1");
        for version in ["2.0.0", "2.1.3", "3.0.0"] {
            let dir = src.join(format!("my_app-{version}"));
            file(
                &dir.join("Cargo.toml"),
                "[package]\nname = \"my_app\"\n\n[package.metadata.crol]\n",
            );
            file(&dir.join("crol/schema.crov"), version);
        }
        file(
            &src.join("plain-1.0.0/Cargo.toml"),
            "[package]\nname = \"plain\"\n",
        );
        let resolve = |source| block_on(resolve_in(source, &outside(), false, &dirs, &Offline));
        let expected = src.join("my_app-2.1.3/crol/schema.crov");
        assert_eq!(resolve("crate:my_app@2.1").unwrap(), expected);
        assert_eq!(resolve("crate:my-app@2").unwrap(), expected);
        assert_eq!(
            resolve("crate:my_app").unwrap(),
            src.join("my_app-3.0.0/crol/schema.crov")
        );
        assert_eq!(
            resolve("crate:plain").unwrap_err().message(),
            "schema: `plain` 1.0.0 publishes no schema; it has no `[package.metadata.crol]`"
        );
        assert_eq!(
            resolve("crate:my_app@4").unwrap_err().message(),
            "schema: `my_app` is not cached, and fetching schemas is off"
        );
        assert!(resolve("crate:a/b").is_err());
        assert!(resolve("crate:my_app@not-a-version").is_err());
    }

    #[test]
    fn crates_in_the_workspace_come_first() {
        let dirs = dirs("workspace");
        let error = block_on(resolve_in(
            "crate:crol-eval",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            false,
            &dirs,
            &Offline,
        ))
        .unwrap_err();
        assert!(
            error
                .message()
                .contains("`crol-eval` 0.1.0 publishes no schema"),
            "{error}"
        );
    }

    #[test]
    fn crol_provides_its_own_schema() {
        let dirs = dirs("own");
        let path = block_on(resolve_in("crate:crol", &outside(), false, &dirs, &Offline)).unwrap();
        let schema = crate::schema::SchemaFile::read(&path).unwrap();
        assert_eq!(schema, crate::schema::build::<crate::schema::SchemaFile>());

        // What an earlier build of this version left behind goes.
        std::fs::write(&path, "Schema { format: 1, root: \"x\", types: {} }").unwrap();
        let path = block_on(resolve_in("crate:crol", &outside(), false, &dirs, &Offline)).unwrap();
        let schema = crate::schema::SchemaFile::read(&path).unwrap();
        assert_eq!(schema, crate::schema::build::<crate::schema::SchemaFile>());
    }

    #[cfg(feature = "remote-schemas-async")]
    #[test]
    fn the_async_client_starts_and_resolves_without_fetching() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(Async::new().is_ok());
            let dirs = dirs("async");
            let path = resolve_in("crate:crol", &outside(), false, &dirs, &Offline).await;
            assert!(path.unwrap().is_file());
        });
    }

    #[test]
    fn packages_give_the_file_their_manifest_names() {
        let version = Version::new(1, 2, 3);
        let mut tar = tar::Builder::new(Vec::new());
        let mut add = |path: &str, text: &str| {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, text.as_bytes()).unwrap();
        };
        add("app-1.2.3/src/lib.rs", "");
        add("app-1.2.3/schema/app.crov", "Schema {}");
        add(
            "app-1.2.3/Cargo.toml",
            "[package]\nname = \"app\"\n[package.metadata.crol]\nschema = \"schema/app.crov\"\n",
        );
        let tar = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        let packed = gz.finish().unwrap();
        assert_eq!(
            schema_in_package("app", &version, &packed).unwrap(),
            b"Schema {}"
        );
        assert!(schema_in_package("app", &version, b"not a package").is_err());
    }
}

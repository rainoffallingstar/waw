//! Finding each backend's executable.
//!
//! Neither winget nor Scoop is guaranteed to be on a fresh machine's PATH, and
//! on Windows a shim can exist without being runnable. So this checks the
//! environment override first, then PATH, then the well-known install
//! locations, probing each candidate before trusting it.

use super::*;

pub(crate) fn env_override(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn command_works_or_is_discoverable(program: &str, args: &[String]) -> bool {
    command_works(program, args) || path_looks_present(Path::new(program))
}

pub(crate) fn path_looks_present(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => true,
        Err(_) => false,
    }
}

pub(crate) fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

pub(crate) fn push_unique_path_if_present(items: &mut Vec<String>, path: PathBuf) {
    if path_looks_present(&path) {
        push_unique_string(items, path.display().to_string());
    }
}

pub(crate) fn find_programs_on_path(path_var: Option<&OsStr>, names: &[&str]) -> Vec<String> {
    let mut programs = Vec::new();
    let Some(path_var) = path_var else {
        return programs;
    };

    for dir in env::split_paths(path_var) {
        for name in names {
            let candidate = dir.join(name);
            if path_looks_present(&candidate) {
                push_unique_string(&mut programs, candidate.display().to_string());
            }
        }
    }

    programs
}

pub(crate) fn user_home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
}

pub(crate) fn local_app_data_dir() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join("AppData").join("Local")))
}

pub(crate) fn program_data_dir() -> PathBuf {
    env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

pub(crate) fn winget_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = local_app_data_dir() {
        paths.push(
            local_app_data
                .join("Microsoft")
                .join("WindowsApps")
                .join("winget.exe"),
        );
    }
    paths
}

pub(crate) fn scoop_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(scoop_root) = env::var_os("SCOOP")
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join("scoop")))
    {
        paths.push(scoop_root.join("shims").join("scoop.cmd"));
        paths.push(scoop_root.join("shims").join("scoop.ps1"));
    }
    paths
}

pub(crate) fn choco_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(choco_root) = env::var_os("ChocolateyInstall").map(PathBuf::from) {
        paths.push(choco_root.join("bin").join("choco.exe"));
    }
    if let Some(local_app_data) = local_app_data_dir() {
        paths.push(
            local_app_data
                .join("UniGetUI")
                .join("Chocolatey")
                .join("bin")
                .join("choco.exe"),
        );
    }
    paths.push(
        program_data_dir()
            .join("chocolatey")
            .join("bin")
            .join("choco.exe"),
    );
    paths
}

pub(crate) fn npm_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if cfg!(windows) {
        if let Some(program_files) = env::var_os("ProgramFiles").map(PathBuf::from) {
            paths.push(program_files.join("nodejs").join("npm.cmd"));
        }
        if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)").map(PathBuf::from) {
            paths.push(program_files_x86.join("nodejs").join("npm.cmd"));
        }
    }
    paths
}

pub(crate) fn pip_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = local_app_data_dir() {
        let python_root = local_app_data.join("Programs").join("Python");
        paths.push(python_root.join("Launcher").join("py.exe"));

        if let Ok(entries) = fs::read_dir(&python_root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("python") {
                    let base = entry.path();
                    paths.push(base.join("python.exe"));
                    paths.push(base.join("Scripts").join("pip.exe"));
                }
            }
        }
    }
    paths
}

pub(crate) fn pip_path_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["python.exe", "python3.exe", "py.exe", "pip.exe", "pip3.exe"]
    } else {
        &["python", "python3", "pip", "pip3"]
    }
}

pub(crate) fn is_python_launcher(program: &str) -> bool {
    let lower = program.to_ascii_lowercase();
    lower.ends_with("python")
        || lower.ends_with("python.exe")
        || lower.ends_with("python3")
        || lower.ends_with("python3.exe")
        || lower.ends_with("\\py.exe")
        || lower == "py"
        || lower == "py.exe"
}

pub(crate) fn pip_probe_args(program: &str) -> Vec<String> {
    if is_python_launcher(program) {
        vec!["-m".to_string(), "pip".to_string(), "--version".to_string()]
    } else {
        vec!["--version".to_string()]
    }
}

/// PyPI has no search API: the XML-RPC `search` endpoint was permanently
/// disabled in 2020 because it was hammered into the ground, and the web UI's
/// `/search/` route is not a machine API.
///
/// The previous implementation worked around that by downloading the entire
/// simple index and filtering locally. Measured on 2026-10-27 that was
/// 45,363,902 bytes, 924,326 projects, and roughly 82 seconds per query with
/// no User-Agent, no compression, and no timeout.
///
/// This probes the documented per-project JSON API instead. It only finds
/// projects whose name it can guess from the query, which is stated in the
/// CLI help. In exchange a search costs a handful of small requests.
/// PyPI has no search API: the XML-RPC `search` endpoint was permanently
/// disabled in 2020, and the web UI's `/search/` route is not a machine API.
///
/// This probes the documented per-project JSON API with names derived from the
/// query. It costs a handful of small requests instead of the 45 MB / ~82 s
/// full-index download the previous implementation did.
///
/// Two refinements over naive probing:
///
/// * Version-suffixed names are probed, because several widely used projects
///   were renamed that way (`beautifulsoup` -> `beautifulsoup4`). Without this
///   a query for "beautifulsoup" returns the abandoned 2008 package.
/// * Yanked releases are ranked last, and a package with a strictly newer
///   release wins a tie against a same-named older sibling.
pub(crate) const PIP_SEARCH_SCRIPT: &str = r#"import gzip, json, os, sys, time, urllib.error, urllib.request
query = sys.argv[1].strip()
normalized = query.lower().replace(" ", "-").replace("_", "-")

candidates = []
def add(name):
    lowered = name.lower()
    if lowered and lowered not in candidates:
        candidates.append(lowered)

add(normalized)
# Renamed projects: beautifulsoup -> beautifulsoup4, pymysql -> PyMySQL, etc.
for suffix in ("4", "3", "2"):
    add(normalized + suffix)
for prefix in ("python-", "py-", "django-", "flask-"):
    add(prefix + normalized)
add(normalized + "-cli")
add(normalized + "-api")
add(normalized + "-sdk")
add(normalized + "-tools")
add(normalized + "-client")
add(normalized + "-server")
add(normalized + "-utils")
add(normalized + "-parser")
add(normalized + "-py")
parts = normalized.split("-")
if len(parts) > 1:
    add(parts[0])
    add("".join(parts))
    add("-".join(reversed(parts)))

def fetch(name):
    url = "https://pypi.org/pypi/" + name + "/json"
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": "waw-cli"},
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            data = json.load(response)
    except urllib.error.HTTPError:
        return None
    except Exception:
        return None
    info = data.get("info") or {}
    real_name = info.get("name")
    if not real_name:
        return None

    # Newest upload time across all releases, used to spot abandoned packages.
    newest = ""
    yanked = 0
    total = 0
    for files in (data.get("releases") or {}).values():
        for entry in files:
            total += 1
            if entry.get("yanked"):
                yanked += 1
            uploaded = entry.get("upload_time_iso_8601") or ""
            if uploaded > newest:
                newest = uploaded

    return {
        "name": real_name,
        "version": info.get("version"),
        "summary": info.get("summary"),
        "newest": newest,
        "fully_yanked": total > 0 and yanked == total,
    }

def rank_key(item):
    name = item["name"].lower()
    # Names that are the query or a numeric/word variant of it form one class;
    # among them, recency decides. This is what puts beautifulsoup4 ahead of
    # the abandoned beautifulsoup without demoting an exact match that is
    # itself current.
    same_family = name == normalized or (
        name.startswith(normalized)
        and name[len(normalized):len(normalized) + 1].isdigit()
    )
    # Invert the timestamp so "newer" sorts first.
    inverted_date = "".join(
        chr(255 - ord(c)) if ord(c) < 255 else c for c in item["newest"]
    )
    return (
        # Yanked packages never lead.
        item["fully_yanked"],
        not same_family,
        not name.startswith(normalized),
        inverted_date,
        name != normalized,
        len(name),
        name,
    )

if len(sys.argv) > 2 and sys.argv[2] == "--deep":
    cache_path = sys.argv[3] if len(sys.argv) > 3 else ""
    index_url = "https://pypi.org/simple/"
    index_headers = {
        "Accept": "application/vnd.pypi.simple.v1+json",
        "User-Agent": "waw-cli",
        "Accept-Encoding": "gzip",
    }

    cache = {}
    if cache_path:
        try:
            with open(cache_path, "r", encoding="utf-8") as handle:
                cache = json.load(handle)
        except Exception:
            cache = {}

    names = cache.get("names") or []
    etag = cache.get("etag")
    fetched_at = cache.get("fetched_at") or 0
    # Respect Cache-Control: max-age=600. Revalidating on every query would add
    # a network round trip for a list that cannot have changed yet.
    stale = (time.time() - fetched_at) > 600

    if not names or stale:
        request_headers = dict(index_headers)
        if etag:
            request_headers["If-None-Match"] = etag
        try:
            request = urllib.request.Request(index_url, headers=request_headers)
            with urllib.request.urlopen(request, timeout=60) as response:
                body = response.read()
                if response.headers.get("Content-Encoding") == "gzip":
                    body = gzip.decompress(body)
                payload = json.loads(body)
                names = [
                    project.get("name")
                    for project in payload.get("projects", [])
                    if isinstance(project, dict) and project.get("name")
                ]
                etag = response.headers.get("ETag") or etag
                stale = False
        except urllib.error.HTTPError as error:
            # 304 means the cached list is still current.
            if error.code != 304:
                names = []
        except Exception:
            # Network failure falls back to whatever cache exists.
            pass

        if cache_path and names:
            try:
                os.makedirs(os.path.dirname(cache_path), exist_ok=True)
                with open(cache_path, "w", encoding="utf-8") as handle:
                    json.dump(
                        {"names": names, "etag": etag, "fetched_at": time.time()},
                        handle,
                    )
            except Exception:
                pass

    needle = normalized
    matches = [name for name in names if needle in name.lower()]
    # Prefer names that start with the query, then shorter names, then
    # alphabetical, and cap the detail lookups.
    matches.sort(
        key=lambda name: (
            not name.lower().startswith(needle),
            len(name),
            name.lower(),
        )
    )
    deep_hits = []
    for name in matches[:20]:
        info = fetch(name)
        if info:
            deep_hits.append(info)
    deep_hits.sort(key=rank_key)
    print(json.dumps(deep_hits))
    sys.exit(0)

hits = []
for candidate in candidates:
    info = fetch(candidate)
    if info:
        hits.append(info)

hits.sort(key=rank_key)

# --- --deep: search the full PyPI project index ----------------------------
#
# Probing only finds names the query can predict. With --deep, fetch the
# documented simple index instead and substring-match it.
#
# Measured 2026-10-28: 10 MB gzipped / 45 MB decoded / 925k projects / 2.4 s
# first fetch. The response carries an ETag and max-age=600, so later runs are
# a conditional GET returning 304 in ~0.4 s with no body.

print(json.dumps(hits))
"#;

pub(crate) fn append_source_arg(args: &mut Vec<String>, source: Option<&str>) {
    if let Some(source) = source {
        args.push("--source".to_string());
        args.push(source.to_string());
    }
}

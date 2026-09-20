//! Docker Cleanup — reclaim disk space from Docker images, containers,
//! volumes and build cache by talking to the Docker Engine API over its
//! local endpoint: the unix socket on macOS (via the system curl binary)
//! and the named pipe on Windows — no docker CLI required either way.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::collections::HashSet;

#[cfg(target_os = "macos")]
use std::process::Command;

use tauri::{AppHandle, Emitter};

use super::CleanOutcome;
use crate::scanner::ScanProgress;
use crate::utils::history;
#[cfg(target_os = "windows")]
use crate::utils::permissions;
use crate::utils::{WindleError, Result};

pub const PROGRESS_EVENT: &str = "docker://progress";

/// Pinned Engine API version. Everything we need (volume usage data,
/// build-cache details, prune filters) exists in 1.43, and every Docker
/// Desktop from the last three years speaks it.
const API: &str = "v1.43";

#[cfg(target_os = "macos")]
const CURL: &str = "/usr/bin/curl";
#[cfg(target_os = "macos")]
const DOCKER_APP: &str = "/Applications/Docker.app";

/// Docker Desktop's launchable program. The Windows edition installs
/// machine-wide and documents this path, so there is nothing to discover.
#[cfg(target_os = "windows")]
const DOCKER_DESKTOP_EXE: &str = "%ProgramFiles%\\Docker\\Docker\\Docker Desktop.exe";
/// The Engine API endpoint that Docker Desktop serves on Windows.
#[cfg(target_os = "windows")]
const DOCKER_PIPE: &str = r"\\.\pipe\docker_engine";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockerEnvState {
    NotInstalled,
    NotRunning,
    Ready,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerStatus {
    pub state: DockerEnvState,
    pub engine_version: Option<String>,
    pub socket_path: Option<String>,
}

/// `GET /version` 响应（仅取所需字段）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VersionResponse {
    version: String,
}

/// Run curl against the daemon unix socket and return `(http_status, body)`.
/// `-w "\n%{http_code}"` appends the numeric status as the final line.
#[cfg(target_os = "macos")]
fn engine_request(socket: &str, method: &str, path_and_query: &str) -> Result<(u16, String)> {
    let url = format!("http://localhost/{API}/{path_and_query}");
    let output = Command::new(CURL)
        .args([
            "-sS",
            "--connect-timeout",
            "3",
            "--max-time",
            "15",
            "--unix-socket",
            socket,
            "-X",
            method,
            "-w",
            "\n%{http_code}",
            &url,
        ])
        .output()
        .map_err(|e| WindleError::Command {
            command: "curl".into(),
            message: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(WindleError::Command {
            command: "curl".into(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (code, body) = split_status(&stdout);
    if code == 0 {
        return Err(WindleError::Command {
            command: "curl".into(),
            message: "malformed response (no http status)".into(),
        });
    }
    Ok((code, body))
}

/// Exchange one request with the daemon over the named pipe and return
/// `(http_status, body)`.
///
/// The request is deliberately HTTP/1.0 with `Connection: close`: the Go HTTP
/// server dockerd embeds never chunk-encodes an HTTP/1.0 response, so the
/// body arrives as plain text delimited by the connection close — which
/// `read_to_end` reports as EOF, because `std` maps the pipe's
/// `ERROR_BROKEN_PIPE` to a zero-length read.
#[cfg(target_os = "windows")]
fn engine_request(pipe: &str, method: &str, path_and_query: &str) -> Result<(u16, String)> {
    use std::io::{Read, Write};

    let failed = |what: String, error: std::io::Error| WindleError::Command {
        command: "docker".into(),
        message: format!("{what}: {error}"),
    };

    let mut stream = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe)
        .map_err(|e| failed(format!("open {pipe}"), e))?;
    stream
        .write_all(pipe_request(method, path_and_query).as_bytes())
        .map_err(|e| failed(format!("write {method} /{path_and_query}"), e))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| failed(format!("read {method} /{path_and_query}"), e))?;
    parse_response(&String::from_utf8_lossy(&raw))
}

/// The exact bytes of one pipe request. Pure — unit tested.
#[cfg(target_os = "windows")]
fn pipe_request(method: &str, path_and_query: &str) -> String {
    format!(
        "{method} /{API}/{path_and_query} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
}

/// Split a raw HTTP response into `(status, body)`: the status comes from the
/// first line (`HTTP/1.0 200 OK`), the body is everything after the blank
/// line. Pure — unit tested.
#[cfg(target_os = "windows")]
fn parse_response(raw: &str) -> Result<(u16, String)> {
    let malformed = || WindleError::Command {
        command: "docker".into(),
        message: "malformed HTTP response from the Docker daemon".into(),
    };
    let (head, body) = raw.split_once("\r\n\r\n").ok_or_else(malformed)?;
    let code = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(malformed)?;
    Ok((code, body.to_string()))
}

/// Split curl stdout into `(body, http_status)`. Pure — unit tested.
#[cfg(target_os = "macos")]
fn split_status(raw: &str) -> (u16, String) {
    let trimmed = raw.strip_suffix('\n').unwrap_or(raw);
    match trimmed.rfind('\n') {
        Some(idx) => {
            let body = trimmed[..idx].to_string();
            let code = trimmed[idx + 1..].trim().parse().unwrap_or(0);
            (code, body)
        }
        None => (0, trimmed.to_string()),
    }
}

/// Map a non-2xx response onto `WindleError::Command`, preferring the
/// daemon's own `message` field when the body is JSON.
fn ensure_ok(status: u16, body: &str, what: &str) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(WindleError::Command {
        command: what.to_string(),
        message: format!("HTTP {status}: {}", error_text(status, body)),
    })
}

/// Extract a human-readable failure text from an HTTP error body.
fn error_text(status: u16, body: &str) -> String {
    let _ = status;
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Candidate daemon sockets, in probe order. Pure — unit tested.
#[cfg(target_os = "macos")]
fn socket_candidates(docker_host: Option<&str>, home: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(path) = docker_host.and_then(|h| h.strip_prefix("unix://")) {
        out.push(path.to_string());
    }
    out.push("/var/run/docker.sock".to_string());
    if let Some(home) = home {
        out.push(format!("{home}/.docker/run/docker.sock"));
    }
    out
}

/// Candidate daemon endpoints, in probe order. A `DOCKER_HOST` naming the
/// Docker Desktop pipe wins; it is written as `npipe:////./pipe/docker_engine`,
/// so the leading slashes are lowered to the backslashes `CreateFile` needs.
/// Other schemes (a `tcp://` daemon) are out of scope and ignored. Pure —
/// unit tested.
#[cfg(target_os = "windows")]
fn socket_candidates(docker_host: Option<&str>, _home: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(path) = docker_host.and_then(|h| h.strip_prefix("npipe://")) {
        out.push(path.replace('/', "\\"));
    }
    out.push(DOCKER_PIPE.to_string());
    out
}

/// First candidate that answers a cheap API call.
/// NOTE: `/_ping` returns 404 on newer Docker Desktop builds (Engine 29+,
/// both versioned and unversioned), so probe `GET /version` — the most
/// stable versioned endpoint — instead.
fn detect_socket() -> Option<String> {
    let home = dirs::home_dir().map(|p| p.display().to_string());
    let host = std::env::var("DOCKER_HOST").ok();
    for socket in socket_candidates(host.as_deref(), home.as_deref()) {
        if !endpoint_exists(&socket) {
            continue;
        }
        if let Ok((code, _)) = engine_request(&socket, "GET", "version") {
            if code == 200 {
                return Some(socket);
            }
        }
    }
    None
}

/// A unix socket is a file, so its presence can be checked before spending a
/// request on it.
#[cfg(target_os = "macos")]
fn endpoint_exists(socket: &str) -> bool {
    PathBuf::from(socket).exists()
}

/// A named pipe has no directory entry, so only the probe request can tell
/// whether it is there; a missing pipe fails it immediately.
#[cfg(target_os = "windows")]
fn endpoint_exists(_pipe: &str) -> bool {
    true
}

/// Whether Docker Desktop is present: the app bundle on macOS, the launcher
/// under the documented install directory on Windows. Only consulted when no
/// engine answered, to tell "not running" from "not installed".
#[cfg(target_os = "macos")]
fn docker_desktop_present() -> bool {
    PathBuf::from(DOCKER_APP).exists()
}

#[cfg(target_os = "windows")]
fn docker_desktop_present() -> bool {
    docker_desktop_exe().is_some()
}

#[cfg(target_os = "windows")]
fn docker_desktop_exe() -> Option<PathBuf> {
    let exe = permissions::expand(DOCKER_DESKTOP_EXE);
    exe.exists().then_some(exe)
}

/// Probe daemon reachability and Docker Desktop's presence. A reachable
/// engine counts as ready no matter where it came from; otherwise Docker
/// Desktop's presence separates "not running" from "not installed".
#[tauri::command]
pub async fn docker_status() -> Result<DockerStatus> {
    if let Some(socket) = detect_socket() {
        let version = engine_request(&socket, "GET", "version")
            .ok()
            .and_then(|(code, body)| (code == 200).then_some(body))
            .and_then(|body| serde_json::from_str::<VersionResponse>(&body).ok())
            .map(|v| v.version);
        return Ok(DockerStatus {
            state: DockerEnvState::Ready,
            engine_version: version,
            socket_path: Some(socket),
        });
    }
    Ok(DockerStatus {
        state: if docker_desktop_present() {
            DockerEnvState::NotRunning
        } else {
            DockerEnvState::NotInstalled
        },
        engine_version: None,
        socket_path: None,
    })
}

/// Launch Docker Desktop; the frontend polls `docker_status` for readiness.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn docker_start_desktop() -> Result<()> {
    super::run_tool("open", &["-a", "Docker"]).map(|_| ())
}

/// Launch Docker Desktop; the frontend polls `docker_status` for readiness.
/// The child handle is dropped at once — Docker Desktop runs on its own.
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn docker_start_desktop() -> Result<()> {
    let exe = docker_desktop_exe().ok_or_else(|| WindleError::NotFound("Docker Desktop".into()))?;
    std::process::Command::new(exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| WindleError::Command {
            command: "Docker Desktop".into(),
            message: e.to_string(),
        })
}

const STOPPED_STATES: [&str; 3] = ["exited", "created", "dead"];

/* ---- /system/df 与 /containers/json 原始响应 ---- */

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct SystemDfResponse {
    #[serde(default)]
    images: Vec<DfImage>,
    #[serde(default)]
    containers: Vec<DfContainerSummary>,
    #[serde(default)]
    volumes: Vec<DfVolume>,
    #[serde(default)]
    build_cache: Vec<DfBuildCache>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DfImage {
    id: String,
    #[serde(default)]
    repo_tags: Vec<String>,
    size: u64,
    /// Unix seconds — the API returns a number here, NOT an RFC3339 string
    /// (that format is only used by volume / build-cache timestamps).
    #[serde(default)]
    created: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DfContainerSummary {
    #[serde(default)]
    size_rw: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DfVolume {
    pub name: String,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub usage_data: Option<DfVolumeUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DfVolumeUsage {
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub ref_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DfBuildCache {
    #[serde(default, rename = "ID")]
    pub id: String,
    #[serde(default, rename = "Type")]
    pub cache_type: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub in_use: bool,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GET /containers/json?all=1&size=1` 条目。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerListEntry {
    pub id: String,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default, rename = "ImageID")]
    image_id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    size_rw: Option<u64>,
}

/* ---- 领域类型（serde camelCase 对齐前端） ---- */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageKind {
    Dangling,
    Unused,
    InUse,
    BlockedByStoppedContainer,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryUsage {
    pub total_count: u64,
    pub active_count: u64,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerImageInfo {
    pub id: String,
    pub full_id: String,
    pub repo_tags: Vec<String>,
    pub size_bytes: u64,
    pub created_at: i64,
    pub kind: ImageKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerContainerInfo {
    pub id: String,
    pub name: String,
    pub image_id: String,
    pub state: String,
    pub exit_code: Option<i64>,
    pub status_text: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerVolumeInfo {
    pub name: String,
    pub driver: String,
    pub anonymous: bool,
    pub size_bytes: u64,
    pub in_use: bool,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerBuildCacheInfo {
    pub id: String,
    pub cache_type: String,
    pub size_bytes: u64,
    pub in_use: bool,
    pub last_used_at: Option<i64>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerOverview {
    pub images: CategoryUsage,
    pub containers: CategoryUsage,
    pub volumes: CategoryUsage,
    pub build_cache: CategoryUsage,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerScanReport {
    pub overview: DockerOverview,
    pub images: Vec<DockerImageInfo>,
    pub stopped_containers: Vec<DockerContainerInfo>,
    pub unused_volumes: Vec<DockerVolumeInfo>,
    pub build_cache: Vec<DockerBuildCacheInfo>,
    pub scanned_at: i64,
}

/// Pure classification: turn the two API payloads into the domain report.
/// Unit tested — performs no I/O.
pub(crate) fn classify(
    df: &SystemDfResponse,
    containers: &[ContainerListEntry],
) -> DockerScanReport {
    // Image reference sets, split by whether the referencing container is
    // still active (running / paused / restarting …). Anything not in
    // STOPPED_STATES counts as active — protects paused containers' images.
    let mut running_refs: HashSet<&str> = HashSet::new();
    let mut stopped_refs: HashSet<&str> = HashSet::new();
    for c in containers {
        if STOPPED_STATES.contains(&c.state.as_str()) {
            stopped_refs.insert(c.image_id.as_str());
        } else {
            running_refs.insert(c.image_id.as_str());
        }
    }

    // --- images：InUse > BlockedByStoppedContainer > Dangling > Unused ---
    let mut images: Vec<DockerImageInfo> = df
        .images
        .iter()
        .map(|img| {
            let kind = if running_refs.contains(img.id.as_str()) {
                ImageKind::InUse
            } else if stopped_refs.contains(img.id.as_str()) {
                ImageKind::BlockedByStoppedContainer
            } else if img.repo_tags.is_empty() {
                ImageKind::Dangling
            } else {
                ImageKind::Unused
            };
            DockerImageInfo {
                id: img.id.trim_start_matches("sha256:").chars().take(12).collect(),
                full_id: img.id.clone(),
                repo_tags: img.repo_tags.clone(),
                size_bytes: img.size,
                created_at: img.created.saturating_mul(1000),
                kind,
            }
        })
        .collect();
    images.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    // --- stopped containers：名称去前导 "/"，退出码从 Status 解析 ---
    let stopped_containers: Vec<DockerContainerInfo> = containers
        .iter()
        .filter(|c| STOPPED_STATES.contains(&c.state.as_str()))
        .map(|c| DockerContainerInfo {
            id: c.id.chars().take(12).collect(),
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_else(|| c.id.chars().take(12).collect()),
            image_id: c
                .image_id
                .trim_start_matches("sha256:")
                .chars()
                .take(12)
                .collect(),
            state: c.state.clone(),
            exit_code: parse_exit_code(&c.status),
            status_text: c.status.clone(),
            size_bytes: c.size_rw.unwrap_or(0),
        })
        .collect();

    // --- volumes：仅未使用卷（RefCount == 0）进入列表 ---
    let unused_volumes: Vec<DockerVolumeInfo> = df
        .volumes
        .iter()
        .filter_map(|v| {
            let usage = v.usage_data.as_ref()?;
            if usage.ref_count > 0 {
                return None;
            }
            Some(DockerVolumeInfo {
                name: v.name.clone(),
                driver: v.driver.clone(),
                anonymous: is_anonymous(&v.name),
                size_bytes: usage.size,
                in_use: false,
                created_at: v.created_at.as_deref().and_then(parse_rfc3339_ms),
            })
        })
        .collect();

    // --- build cache（in_use 条目保留在列表中供 UI 禁选） ---
    let build_cache: Vec<DockerBuildCacheInfo> = df
        .build_cache
        .iter()
        .map(|b| DockerBuildCacheInfo {
            id: b.id.clone(),
            cache_type: b.cache_type.clone().unwrap_or_else(|| "regular".into()),
            size_bytes: b.size,
            in_use: b.in_use,
            last_used_at: b.last_used_at.as_deref().and_then(parse_rfc3339_ms),
            description: b.description.clone().filter(|d| !d.is_empty()),
        })
        .collect();

    // --- overview：全部由明细推导（/system/df 无顶层汇总字段） ---
    let images_usage = CategoryUsage {
        total_count: images.len() as u64,
        active_count: images
            .iter()
            .filter(|i| {
                matches!(
                    i.kind,
                    ImageKind::InUse | ImageKind::BlockedByStoppedContainer
                )
            })
            .count() as u64,
        total_bytes: images.iter().map(|i| i.size_bytes).sum(),
        reclaimable_bytes: images
            .iter()
            .filter(|i| matches!(i.kind, ImageKind::Dangling | ImageKind::Unused))
            .map(|i| i.size_bytes)
            .sum(),
    };

    let containers_usage = CategoryUsage {
        total_count: containers.len() as u64,
        active_count: containers
            .iter()
            .filter(|c| !STOPPED_STATES.contains(&c.state.as_str()))
            .count() as u64,
        // /system/df 与 /containers/json 的 SizeRw 同源，只取其一避免双份相加；
        // /system/df 缺字段时回退到容器列表求和。
        total_bytes: if df.containers.is_empty() {
            containers.iter().map(|c| c.size_rw.unwrap_or(0)).sum()
        } else {
            df.containers.iter().map(|c| c.size_rw.unwrap_or(0)).sum()
        },
        reclaimable_bytes: stopped_containers.iter().map(|c| c.size_bytes).sum(),
    };

    let volumes_usage = {
        let mut usage = CategoryUsage {
            total_count: df.volumes.len() as u64,
            active_count: 0,
            total_bytes: 0,
            reclaimable_bytes: unused_volumes.iter().map(|v| v.size_bytes).sum(),
        };
        for v in &df.volumes {
            if let Some(u) = &v.usage_data {
                usage.total_bytes += u.size;
                if u.ref_count > 0 {
                    usage.active_count += 1;
                }
            }
        }
        usage
    };

    let build_cache_usage = CategoryUsage {
        total_count: build_cache.len() as u64,
        active_count: build_cache.iter().filter(|b| b.in_use).count() as u64,
        total_bytes: build_cache.iter().map(|b| b.size_bytes).sum(),
        reclaimable_bytes: build_cache
            .iter()
            .filter(|b| !b.in_use)
            .map(|b| b.size_bytes)
            .sum(),
    };

    DockerScanReport {
        overview: DockerOverview {
            images: images_usage,
            containers: containers_usage,
            volumes: volumes_usage,
            build_cache: build_cache_usage,
        },
        images,
        stopped_containers,
        unused_volumes,
        build_cache,
        scanned_at: now_ms(),
    }
}

/// Pull the exit code out of a status string like `"Exited (0) 3 days ago"`.
fn parse_exit_code(status: &str) -> Option<i64> {
    let start = status.find('(')?;
    let rest = &status[start + 1..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

/// Anonymous volumes are named with a 64-char hex id.
fn is_anonymous(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Minimal RFC3339 parser (no chrono): `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)`
/// → unix milliseconds. Unit tested.
fn parse_rfc3339_ms(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.len() < 20 {
        return None;
    }
    let num =
        |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse().ok() };
    if s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let sep = *s.as_bytes().get(10)?;
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    if s.as_bytes().get(13) != Some(&b':') || s.as_bytes().get(16) != Some(&b':') {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    // 可选小数秒 → 毫秒（补零/截断到 3 位）
    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits = stripped
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(stripped.len());
        if digits == 0 {
            return None;
        }
        let mut padded = stripped[..digits].to_string();
        while padded.len() < 3 {
            padded.push('0');
        }
        padded.truncate(3);
        millis = padded.parse().ok()?;
        rest = &stripped[digits..];
    }

    // 时区：Z 或 ±HH:MM
    let offset_ms = match rest.as_bytes().first()? {
        b'Z' | b'z' => 0i64,
        sign @ (b'+' | b'-') => {
            if rest.as_bytes().get(3) != Some(&b':') {
                return None;
            }
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6)?.parse().ok()?;
            let magnitude = (oh * 3600 + om * 60) * 1000;
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86400 + hour * 3600 + minute * 60 + second;
    Some(secs * 1000 + millis - offset_ms)
}

/// Days since 1970-01-01 — Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Fetch both payloads needed for classification.
fn fetch_state(socket: &str) -> Result<(SystemDfResponse, Vec<ContainerListEntry>)> {
    let (code, body) = engine_request(socket, "GET", "system/df")?;
    ensure_ok(code, &body, "GET /system/df")?;
    let df: SystemDfResponse =
        serde_json::from_str(&body).map_err(|e| WindleError::Command {
            command: "docker system/df".into(),
            message: e.to_string(),
        })?;

    let (code, body) = engine_request(socket, "GET", "containers/json?all=1&size=1")?;
    ensure_ok(code, &body, "GET /containers/json")?;
    let containers: Vec<ContainerListEntry> =
        serde_json::from_str(&body).map_err(|e| WindleError::Command {
            command: "docker containers/json".into(),
            message: e.to_string(),
        })?;
    Ok((df, containers))
}

fn not_reachable() -> WindleError {
    WindleError::Command {
        command: "docker".into(),
        message: "Docker daemon is not reachable".into(),
    }
}

/// Scan images / containers / volumes / build cache and classify them.
#[tauri::command]
pub async fn docker_scan() -> Result<DockerScanReport> {
    let socket = detect_socket().ok_or_else(not_reachable)?;
    let (df, containers) = fetch_state(&socket)?;
    Ok(classify(&df, &containers))
}

/// Frontend → backend clean request. Ids come from a prior `docker_scan`
/// report; the backend re-validates everything against a fresh scan so a
/// stale or bypassed frontend can never delete protected items.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerCleanRequest {
    #[serde(default)]
    pub image_ids: Vec<String>,
    #[serde(default)]
    pub container_ids: Vec<String>,
    #[serde(default)]
    pub volume_names: Vec<String>,
    /// Go duration："0s"（全部未使用）/ "72h" / "168h" / "720h"。
    /// `None` 跳过构建缓存。
    #[serde(default)]
    pub build_cache_until: Option<String>,
}

/// Post-validation execution plan.
#[derive(Debug, Default)]
struct CleanPlan {
    images: Vec<DockerImageInfo>,
    containers: Vec<DockerContainerInfo>,
    volumes: Vec<DockerVolumeInfo>,
    build_cache: Vec<DockerBuildCacheInfo>,
    /// (id/name, reason) — 经 `CleanOutcome::failed_paths` 呈现给用户。
    rejects: Vec<(String, String)>,
}

/// Cross-check the request against a fresh scan. Pure — unit tested.
fn validate_clean(report: &DockerScanReport, req: &DockerCleanRequest, now: i64) -> CleanPlan {
    let mut plan = CleanPlan::default();

    for id in &req.image_ids {
        match report.images.iter().find(|i| &i.full_id == id) {
            None => plan.rejects.push((id.clone(), "image not found".into())),
            Some(img) => match img.kind {
                ImageKind::Dangling | ImageKind::Unused => plan.images.push(img.clone()),
                ImageKind::InUse => plan
                    .rejects
                    .push((img.full_id.clone(), "image is in use".into())),
                ImageKind::BlockedByStoppedContainer => plan.rejects.push((
                    img.full_id.clone(),
                    "image is referenced by a stopped container".into(),
                )),
            },
        }
    }

    for id in &req.container_ids {
        match report.stopped_containers.iter().find(|c| &c.id == id) {
            None => plan.rejects.push((id.clone(), "container not found".into())),
            Some(c) => plan.containers.push(c.clone()),
        }
    }

    for name in &req.volume_names {
        match report.unused_volumes.iter().find(|v| &v.name == name) {
            None => plan
                .rejects
                .push((name.clone(), "volume not found or in use".into())),
            Some(v) => plan.volumes.push(v.clone()),
        }
    }

    if let Some(until) = &req.build_cache_until {
        match until_seconds(until) {
            None => plan
                .rejects
                .push((until.clone(), "invalid build cache window".into())),
            Some(secs) => {
                let cutoff = now - secs * 1000;
                for entry in &report.build_cache {
                    if entry.in_use {
                        continue;
                    }
                    let old_enough = entry.last_used_at.map(|t| t <= cutoff).unwrap_or(true);
                    if old_enough {
                        plan.build_cache.push(entry.clone());
                    }
                }
            }
        }
    }

    plan
}

/// "0s" / "72h" / "168h" / "720h" → 秒。格式非法返回 None。
fn until_seconds(until: &str) -> Option<i64> {
    if let Some(h) = until.strip_suffix('h') {
        return h.parse::<i64>().ok().map(|n| n * 3600);
    }
    if let Some(s) = until.strip_suffix('s') {
        return s.parse::<i64>().ok();
    }
    None
}

/// Percent-encode the prune filters JSON for a query parameter. Unit tested.
fn encode_prune_filters(until: &str) -> String {
    let raw = format!("{{\"until\":{{\"{until}\":true}}}}");
    let mut out = String::with_capacity(raw.len() * 3);
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Build-cache prune path. `all=true` is REQUIRED: without it the endpoint
/// only drops *dangling* records and silently reclaims nothing for unused
/// regular cache on buildkit-backed Docker Desktop builds (verified against
/// Engine 29: HTTP 200, `SpaceReclaimed: 0`). "0s" (everything unused)
/// omits the filter entirely — equivalent semantics, one less thing for the
/// daemon to misinterpret.
fn build_prune_path(until: &str) -> String {
    if until == "0s" {
        "build/prune?all=true".to_string()
    } else {
        format!("build/prune?all=true&filters={}", encode_prune_filters(until))
    }
}

/// 四类 total 之和——精确字节记账的基线。
fn sum_total_bytes(overview: &DockerOverview) -> u64 {
    overview.images.total_bytes
        + overview.containers.total_bytes
        + overview.volumes.total_bytes
        + overview.build_cache.total_bytes
}

/// 后置 /system/df 不可用时的回退估算：计划条目扫描时尺寸求和。
fn fallback_freed_bytes(plan: &CleanPlan) -> u64 {
    plan.images.iter().map(|i| i.size_bytes).sum::<u64>()
        + plan.containers.iter().map(|c| c.size_bytes).sum::<u64>()
        + plan.volumes.iter().map(|v| v.size_bytes).sum::<u64>()
        + plan.build_cache.iter().map(|b| b.size_bytes).sum::<u64>()
}

fn emit_progress(app: &AppHandle, step: usize, total: usize, label: &str) {
    let _ = app.emit(
        PROGRESS_EVENT,
        ScanProgress {
            progress: Some(step as f32 / total.max(1) as f32),
            current_path: label.to_string(),
            items_scanned: step as u64,
            bytes_found: 0,
        },
    );
}

/// Delete the validated selection. Order is fixed — containers first so they
/// stop blocking the images that reference them — then images, volumes and
/// build cache. Freed bytes come from a before/after `/system/df` diff, with
/// a scan-size sum as fallback if the daemon disappears mid-run.
#[tauri::command]
pub async fn docker_clean(
    app: AppHandle,
    request: DockerCleanRequest,
) -> Result<CleanOutcome> {
    let socket = detect_socket().ok_or_else(not_reachable)?;

    let (df, containers) = fetch_state(&socket)?;
    let report = classify(&df, &containers);
    let before = sum_total_bytes(&report.overview);
    let plan = validate_clean(&report, &request, now_ms());

    let mut outcome = CleanOutcome::default();
    for (id, reason) in &plan.rejects {
        outcome.fail(id.clone(), reason.clone());
    }

    let total = plan.containers.len() + plan.images.len() + plan.volumes.len()
        + usize::from(!plan.build_cache.is_empty());
    let mut step = 0usize;

    // 1) stopped containers（先删容器，解锁被引用的镜像）
    for c in &plan.containers {
        step += 1;
        emit_progress(&app, step, total, &format!("container {}", c.id));
        match engine_request(&socket, "DELETE", &format!("containers/{}", c.id)) {
            Ok((code, _)) if (200..300).contains(&code) => {
                outcome.removed_paths.push(c.id.clone());
            }
            Ok((code, body)) => outcome.fail(c.id.clone(), &error_text(code, &body)),
            Err(e) => outcome.fail(c.id.clone(), &e.to_string()),
        }
    }

    // 2) images（不 force；仍被引用的镜像会以 409 失败并带原因返回）
    for img in &plan.images {
        step += 1;
        emit_progress(&app, step, total, &format!("image {}", img.id));
        match engine_request(&socket, "DELETE", &format!("images/{}", img.full_id)) {
            Ok((code, _)) if (200..300).contains(&code) => {
                outcome.removed_paths.push(img.id.clone());
            }
            Ok((code, body)) => outcome.fail(img.id.clone(), &error_text(code, &body)),
            Err(e) => outcome.fail(img.id.clone(), &e.to_string()),
        }
    }

    // 3) unused volumes
    for v in &plan.volumes {
        step += 1;
        emit_progress(&app, step, total, &format!("volume {}", v.name));
        match engine_request(&socket, "DELETE", &format!("volumes/{}", v.name)) {
            Ok((code, _)) if (200..300).contains(&code) => {
                outcome.removed_paths.push(v.name.clone());
            }
            Ok((code, body)) => outcome.fail(v.name.clone(), &error_text(code, &body)),
            Err(e) => outcome.fail(v.name.clone(), &e.to_string()),
        }
    }

    // 4) build cache prune（不带 all=true — 只清未使用记录）
    if !plan.build_cache.is_empty() {
        if let Some(until) = &request.build_cache_until {
            step += 1;
            emit_progress(&app, step, total, "build cache");
            let path = build_prune_path(until);
            match engine_request(&socket, "POST", &path) {
                Ok((code, _)) if (200..300).contains(&code) => {
                    for entry in &plan.build_cache {
                        outcome.removed_paths.push(entry.id.clone());
                    }
                }
                Ok((code, body)) => {
                    outcome.fail("build-cache".to_string(), &error_text(code, &body))
                }
                Err(e) => outcome.fail("build-cache".to_string(), &e.to_string()),
            }
        }
    }

    // 记账：前后 /system/df 差值（精确字节）；daemon 中途不可用时回退求和
    outcome.freed_bytes = match fetch_state(&socket) {
        Ok((df, containers)) => {
            let after = sum_total_bytes(&classify(&df, &containers).overview);
            before.saturating_sub(after)
        }
        Err(_) => fallback_freed_bytes(&plan),
    };

    history::record_outcome(history::Operation::Docker, &outcome);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn split_status_parses_body_and_code() {
        assert_eq!(split_status("OK\n200").0, 200);
        assert_eq!(split_status("OK\n200").1, "OK");
        assert_eq!(split_status("{\"a\":1}\n204").1, "{\"a\":1}");
        // 204 无 body：stdout 只有 "\n204"
        assert_eq!(split_status("\n204").0, 204);
        assert_eq!(split_status("\n204").1, "");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn socket_candidates_probe_order() {
        let c = socket_candidates(Some("unix:///tmp/custom.sock"), Some("/Users/x"));
        assert_eq!(
            c,
            vec![
                "/tmp/custom.sock".to_string(),
                "/var/run/docker.sock".to_string(),
                "/Users/x/.docker/run/docker.sock".to_string(),
            ]
        );
        // 非 unix:// 的 DOCKER_HOST（如 tcp://）被忽略
        let c = socket_candidates(Some("tcp://1.2.3.4:2375"), Some("/Users/x"));
        assert_eq!(c[0], "/var/run/docker.sock");
        // 无环境变量、无 home 时只剩 /var/run/docker.sock 一个候选
        assert_eq!(socket_candidates(None, None).len(), 1);
    }

    /// `npipe:////./pipe/docker_engine` 是 Docker Desktop 写进 DOCKER_HOST
    /// 的官方形式：斜杠换反斜杠后与之等价，且排在默认管道之前。
    #[cfg(target_os = "windows")]
    #[test]
    fn socket_candidates_probe_order() {
        let c = socket_candidates(Some("npipe:////./pipe/docker_engine"), None);
        assert_eq!(
            c,
            vec![r"\\.\pipe\docker_engine".to_string(), DOCKER_PIPE.to_string()]
        );
        // 非 npipe:// 的 DOCKER_HOST（如 tcp://）被忽略
        let c = socket_candidates(Some("tcp://1.2.3.4:2375"), None);
        assert_eq!(c, vec![DOCKER_PIPE.to_string()]);
        assert_eq!(socket_candidates(None, None).len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn pipe_request_is_http_1_0_with_close() {
        // HTTP/1.0 + Connection: close 是响应不带 chunked 编码的前提
        assert_eq!(
            pipe_request("GET", "version"),
            "GET /v1.43/version HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(
            pipe_request("POST", "build/prune?all=true"),
            "POST /v1.43/build/prune?all=true HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parse_response_splits_status_and_body() {
        let (code, body) = parse_response(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"a\":1}",
        )
        .unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{\"a\":1}");
        // 204 无 body：空行之后没有内容
        let (code, body) = parse_response("HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!((code, body.as_str()), (204, ""));
        // 错误响应同样要取出状态码与消息体（error_text 依赖后者）
        let (code, body) =
            parse_response("HTTP/1.0 500 Internal Server Error\r\n\r\n{\"message\":\"boom\"}")
                .unwrap();
        assert_eq!(code, 500);
        assert!(body.contains("boom"));
        // 没有空行分隔的响应直接报错，而不是猜
        assert!(parse_response("garbage").is_err());
    }

    #[test]
    fn ensure_ok_maps_http_errors_with_message_field() {
        assert!(ensure_ok(200, "OK", "GET /x").is_ok());
        assert!(ensure_ok(204, "", "DELETE /y").is_ok());
        let err = ensure_ok(409, "{\"message\":\"conflict\"}", "DELETE /z").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("409"), "unexpected: {msg}");
        assert!(msg.contains("conflict"), "unexpected: {msg}");
    }

    const DF_TYPICAL: &str = r#"{"LayersSize":0,"Images":[
        {"Containers":1,"Created":1767225600,"Id":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","RepoDigests":[],"RepoTags":["nginx:1.25"],"SharedSize":0,"Size":1000},
        {"Containers":0,"Created":1769904000,"Id":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","RepoDigests":[],"RepoTags":[],"SharedSize":0,"Size":500},
        {"Containers":0,"Created":1772323200,"Id":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","RepoDigests":[],"RepoTags":["redis:7"],"SharedSize":0,"Size":800},
        {"Containers":1,"Created":1774992000,"Id":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","RepoDigests":[],"RepoTags":["postgres:16"],"SharedSize":0,"Size":900}],
        "Containers":[
        {"Id":"aaaa1111aaaa","Names":["/web"],"ImageID":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","State":"running","Status":"Up 2 hours","SizeRw":30,"SizeRootFs":1030},
        {"Id":"dddd2222dddd","Names":["/db"],"ImageID":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","State":"exited","Status":"Exited (0) 3 days ago","SizeRw":120,"SizeRootFs":1020}],
        "Volumes":[
        {"Name":"data","Driver":"local","Scope":"local","CreatedAt":"2026-01-01T00:00:00Z","UsageData":{"Size":300,"RefCount":1}},
        {"Name":"cache","Driver":"local","Scope":"local","CreatedAt":"2026-01-01T00:00:00Z","UsageData":{"Size":200,"RefCount":0}},
        {"Name":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","Driver":"local","Scope":"local","UsageData":{"Size":100,"RefCount":0}}],
        "BuildCache":[
        {"ID":"cache1","Type":"regular","Size":50,"InUse":true,"LastUsedAt":"2026-08-16T00:00:00Z","Description":"base"},
        {"ID":"cache2","Type":"regular","Size":70,"InUse":false,"LastUsedAt":"2026-01-01T00:00:00Z","Description":""}]}"#;

    const CONTAINERS_TYPICAL: &str = r#"[{"Id":"aaaa1111aaaa","Names":["/web"],"ImageID":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","State":"running","Status":"Up 2 hours","SizeRw":30,"SizeRootFs":1030},{"Id":"dddd2222dddd","Names":["/db"],"ImageID":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","State":"exited","Status":"Exited (0) 3 days ago","SizeRw":120,"SizeRootFs":1020}]"#;

    #[test]
    fn classify_typical_report() {
        let df: SystemDfResponse = serde_json::from_str(DF_TYPICAL).unwrap();
        let containers: Vec<ContainerListEntry> =
            serde_json::from_str(CONTAINERS_TYPICAL).unwrap();
        let report = classify(&df, &containers);

        // 镜像分类：aaa=InUse（运行中引用）、bbb=Dangling、ccc=Unused、ddd=Blocked
        let kind_of = |tag: &str| {
            report
                .images
                .iter()
                .find(|i| i.repo_tags.first().map(|t| t.as_str()) == Some(tag))
                .unwrap()
                .kind
        };
        assert_eq!(kind_of("nginx:1.25"), ImageKind::InUse);
        assert_eq!(kind_of("redis:7"), ImageKind::Unused);
        assert_eq!(kind_of("postgres:16"), ImageKind::BlockedByStoppedContainer);
        assert_eq!(
            report.images.iter().find(|i| i.repo_tags.is_empty()).unwrap().kind,
            ImageKind::Dangling
        );
        assert_eq!(report.images.len(), 4);
        assert_eq!(report.images.iter().next().unwrap().id.len(), 12); // 短 id 用于展示
        assert_eq!(report.images.iter().next().unwrap().full_id.len(), 71); // "sha256:" + 64

        // 停止容器：名称去掉前导 "/"，退出码从 Status 解析
        assert_eq!(report.stopped_containers.len(), 1);
        let c = &report.stopped_containers[0];
        assert_eq!(c.name, "db");
        assert_eq!(c.exit_code, Some(0));
        assert_eq!(c.size_bytes, 120);

        // 卷：仅未使用卷进入列表；hex 名为匿名卷
        assert_eq!(report.unused_volumes.len(), 2);
        let anon = report
            .unused_volumes
            .iter()
            .find(|v| v.anonymous)
            .unwrap();
        assert_eq!(anon.size_bytes, 100);

        // 概览
        assert_eq!(report.overview.images.total_count, 4);
        assert_eq!(report.overview.images.active_count, 2);
        assert_eq!(report.overview.images.total_bytes, 3200);
        assert_eq!(report.overview.images.reclaimable_bytes, 1300);
        assert_eq!(report.overview.containers.total_count, 2);
        assert_eq!(report.overview.containers.active_count, 1);
        assert_eq!(report.overview.containers.total_bytes, 150);
        assert_eq!(report.overview.containers.reclaimable_bytes, 120);
        assert_eq!(report.overview.volumes.total_count, 3);
        assert_eq!(report.overview.volumes.reclaimable_bytes, 300);
        assert_eq!(report.overview.build_cache.total_count, 2);
        assert_eq!(report.overview.build_cache.active_count, 1);
        assert_eq!(report.overview.build_cache.reclaimable_bytes, 70);
    }

    #[test]
    fn classify_empty_daemon() {
        let df: SystemDfResponse =
            serde_json::from_str(r#"{"LayersSize":0,"Images":[],"Containers":[],"Volumes":[],"BuildCache":[]}"#).unwrap();
        let report = classify(&df, &[]);
        assert!(report.images.is_empty());
        assert!(report.stopped_containers.is_empty());
        assert!(report.unused_volumes.is_empty());
        assert!(report.build_cache.is_empty());
        assert_eq!(report.overview.images.total_bytes, 0);
    }

    #[test]
    fn parse_timestamps() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_rfc3339_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00-01:00"), Some(3_600_000));
        // 真实构建缓存时间戳带 9 位小数秒 — 截断到毫秒
        assert_eq!(
            parse_rfc3339_ms("2026-08-06T08:59:31.828565469Z"),
            parse_rfc3339_ms("2026-08-06T08:59:31.828Z")
        );
        // 一天差值
        let a = parse_rfc3339_ms("2026-08-15T00:00:00Z").unwrap();
        let b = parse_rfc3339_ms("2026-08-16T00:00:00Z").unwrap();
        assert_eq!(b - a, 86_400_000);
        assert_eq!(parse_rfc3339_ms("not a date"), None);
    }

    fn typical_clean_request() -> DockerCleanRequest {
        serde_json::from_str(
            r#"{
                "imageIds": [
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                ],
                "containerIds": ["dddd2222dddd", "ffff0000ffff"],
                "volumeNames": ["cache", "data"],
                "buildCacheUntil": "72h"
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn validate_clean_rejects_in_use_and_unknown() {
        let df: SystemDfResponse = serde_json::from_str(DF_TYPICAL).unwrap();
        let containers: Vec<ContainerListEntry> =
            serde_json::from_str(CONTAINERS_TYPICAL).unwrap();
        let report = classify(&df, &containers);
        let now = parse_rfc3339_ms("2026-08-16T00:00:00Z").unwrap();

        let plan = validate_clean(&report, &typical_clean_request(), now);

        // 通过：悬空 bbbb + 未使用 cccc；停止容器 dddd2222dddd；
        // 未使用卷 cache；72h 窗口内未使用的构建缓存仅 cache2
        assert_eq!(plan.images.len(), 2);
        assert_eq!(plan.containers.len(), 1);
        assert_eq!(plan.volumes.len(), 1);
        assert_eq!(plan.build_cache.len(), 1);
        assert_eq!(plan.build_cache[0].id, "cache2");

        // 拒绝 5 项：InUse 镜像 aaaa、Blocked 镜像 dddd、未知镜像 eeee、
        // 未知容器 ffff、使用中卷 data
        assert_eq!(plan.rejects.len(), 5);
        let reasons: Vec<&str> = plan.rejects.iter().map(|(_, r)| r.as_str()).collect();
        assert!(reasons.iter().any(|r| r.contains("in use")));
        assert!(reasons.iter().any(|r| r.contains("stopped container")));
        assert!(reasons.iter().any(|r| r.contains("not found")));
    }

    #[test]
    fn encode_prune_filters_percent_encodes() {
        assert_eq!(
            encode_prune_filters("72h"),
            "%7B%22until%22%3A%7B%2272h%22%3Atrue%7D%7D"
        );
        assert_eq!(
            encode_prune_filters("0s"),
            "%7B%22until%22%3A%7B%220s%22%3Atrue%7D%7D"
        );
    }

    #[test]
    fn build_prune_path_requires_all_and_skips_filter_for_0s() {
        // "全部"窗口：不带 filters，只带 all=true（已实测：无 all 时静默零删除）
        assert_eq!(build_prune_path("0s"), "build/prune?all=true");
        // 带窗口：all=true + percent-encoded until filter
        assert_eq!(
            build_prune_path("72h"),
            "build/prune?all=true&filters=%7B%22until%22%3A%7B%2272h%22%3Atrue%7D%7D"
        );
        assert_eq!(
            build_prune_path("720h"),
            "build/prune?all=true&filters=%7B%22until%22%3A%7B%22720h%22%3Atrue%7D%7D"
        );
    }

    #[test]
    fn sum_total_and_fallback_accounting() {
        let df: SystemDfResponse = serde_json::from_str(DF_TYPICAL).unwrap();
        let containers: Vec<ContainerListEntry> =
            serde_json::from_str(CONTAINERS_TYPICAL).unwrap();
        let report = classify(&df, &containers);
        let now = parse_rfc3339_ms("2026-08-16T00:00:00Z").unwrap();

        // 四类 total 之和：镜像 3200 + 容器 150 + 卷 600 + 构建缓存 120 = 4070
        assert_eq!(sum_total_bytes(&report.overview), 4070);

        // 回退记账 = 计划条目扫描时尺寸之和：
        // 悬空 500 + 未使用 800 + 停止容器 120 + 卷 200 + 构建缓存 70 = 1690
        let plan = validate_clean(&report, &typical_clean_request(), now);
        assert_eq!(fallback_freed_bytes(&plan), 1690);
    }
}

//! Automation — cleanup without anyone asking: a weekday/time schedule,
//! disk- and memory-usage triggers, and a local webhook other tools can call.
//!
//! Three moving parts share one rule set. A scheduler thread looks at the
//! clock and the machine every [`TICK_INTERVAL`]; a webhook thread listens on
//! loopback; both hand work to the same single-run executor, so there is never
//! more than one automated cleanup in flight. Every finished run lands in the
//! log file the UI reads and is emitted as a Tauri event for an open window.
//!
//! What a run does is deliberately narrow. The automated deep clean removes
//! exactly what the Deep Clean page checks by default — safe categories, safe
//! items — and the memory clean is the same in-process purge the System
//! Optimize page uses. Neither needs a password, which matters because an
//! unattended run cannot ask for one.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::System;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{analyze, clean, optimize};
use crate::scanner::walker::CancelToken;
use crate::utils::{format, platform, WindleError, Result};

/// Fired with each finished run, so an open window refreshes its log.
pub const LOG_EVENT: &str = "automation://log";

/// How often the scheduler looks at the clock and the machine.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// A schedule slot still runs when found within this long after its time —
/// covers short sleeps and app restarts without running hours-old slots.
const SLOT_GRACE_SECS: i64 = 10 * 60;

/// Quiet period after a usage-triggered run before the same trigger may fire
/// again; without it a disk that stays full would clean in a loop.
const DISK_COOLDOWN_MS: u64 = 30 * 60 * 1000;
const MEMORY_COOLDOWN_MS: u64 = 30 * 60 * 1000;

/// The webhook accepts only tiny requests; bigger ones are refused before
/// they are read.
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 4 * 1024;

/// How long a request may take to arrive before the connection is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Sleep between accept attempts on the non-blocking webhook listener.
const ACCEPT_POLL: Duration = Duration::from_millis(150);

/// How long a restart waits for the previous listener to release the port.
const WEBHOOK_STOP_WAIT: Duration = Duration::from_secs(2);

/// Runs kept in the on-disk log.
const MAX_LOG_ENTRIES: usize = 200;

/// Port the webhook listener uses until the user picks another one.
const DEFAULT_WEBHOOK_PORT: u16 = 17830;

/* ------------------------------- Settings -------------------------------- */

/// The weekly cleanup window: which days, which hours of the day, how often.
///
/// Hours are local time — "8:00" means the user's 8:00 — which is why
/// [`local_now`] exists at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScheduleConfig {
    pub enabled: bool,
    /// Days the schedule may run on, 0 = Sunday … 6 = Saturday. The numbering
    /// matches `libc::tm_wday` / `SYSTEMTIME.wDayOfWeek` and JavaScript's
    /// `Date.getDay`, so neither side needs a translation table.
    pub days: Vec<u8>,
    /// First hour of the daily window (inclusive), 0–23.
    pub start_hour: u8,
    /// Last hour of the daily window (inclusive), 0–23.
    pub end_hour: u8,
    /// Hours between runs inside the window.
    pub interval_hours: u8,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            days: (0..=6).collect(),
            start_hour: 8,
            end_hour: 20,
            interval_hours: 2,
        }
    }
}

/// Fire an action once a resource's usage crosses a percentage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThresholdTrigger {
    pub enabled: bool,
    /// Percentage of the resource in use that fires the trigger.
    pub threshold_percent: u8,
}

impl Default for ThresholdTrigger {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_percent: 90,
        }
    }
}

/// The loopback webhook the outside world triggers cleanups through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub port: u16,
    /// Shared secret the caller must present. Generated on first load, so the
    /// field is never empty in the UI.
    pub token: String,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_WEBHOOK_PORT,
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationConfig {
    pub schedule: ScheduleConfig,
    /// Deep clean once the boot volume is this full.
    pub disk_trigger: ThresholdTrigger,
    /// Purge memory once usage crosses this mark.
    pub memory_trigger: ThresholdTrigger,
    /// Empty the trash after every automated deep clean — what a clean moves
    /// there still occupies the disk.
    pub empty_trash: bool,
    pub webhook: WebhookConfig,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            schedule: ScheduleConfig::default(),
            disk_trigger: ThresholdTrigger {
                enabled: false,
                threshold_percent: 95,
            },
            memory_trigger: ThresholdTrigger {
                enabled: false,
                threshold_percent: 90,
            },
            empty_trash: false,
            webhook: WebhookConfig::default(),
        }
    }
}

/// Bring a config into range, whichever door it came through — a hand-edited
/// file or the frontend.
fn normalize(config: &mut AutomationConfig) {
    let schedule = &mut config.schedule;
    schedule.days.retain(|day| *day <= 6);
    schedule.days.sort_unstable();
    schedule.days.dedup();
    schedule.start_hour = schedule.start_hour.min(23);
    schedule.end_hour = schedule.end_hour.min(23);
    if schedule.start_hour > schedule.end_hour {
        std::mem::swap(&mut schedule.start_hour, &mut schedule.end_hour);
    }
    schedule.interval_hours = schedule.interval_hours.clamp(1, 24);
    config.disk_trigger.threshold_percent = config.disk_trigger.threshold_percent.clamp(1, 100);
    config.memory_trigger.threshold_percent = config.memory_trigger.threshold_percent.clamp(1, 100);
    config.webhook.port = if config.webhook.port < 1024 {
        DEFAULT_WEBHOOK_PORT
    } else {
        config.webhook.port
    };
}

/// What [`normalize`] cannot fix: settings that would misbehave as written.
fn validate(config: &AutomationConfig) -> std::result::Result<(), String> {
    if config.schedule.enabled && config.schedule.days.is_empty() {
        return Err("the schedule needs at least one weekday".into());
    }
    if config.webhook.enabled && config.webhook.token.trim().len() < 8 {
        return Err("the webhook token must be at least 8 characters".into());
    }
    Ok(())
}

/// A fresh 128-bit token as hex. `RandomState` is seeded from the OS entropy
/// pool — the only randomness std hands out — and the loop runs twice for the
/// two halves; the time and stack address mixed in make consecutive calls
/// differ even if the pool ever repeated itself.
fn generate_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut token = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        let address = &hasher as *const _ as usize;
        hasher.write_u64(crate::commands::clean::now_millis());
        hasher.write_usize(address);
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token
}

/* ------------------------------- Persistence ----------------------------- */

fn config_path() -> PathBuf {
    platform::app_support_dir().join("automation.json")
}

fn log_path() -> PathBuf {
    platform::app_support_dir().join("automation-log.json")
}

fn ran_at_path() -> PathBuf {
    platform::app_support_dir().join("automation-state.json")
}

/// Write JSON next to the app's other state files, creating the directory on
/// the way. Every failure is swallowed: none of this is worth an error.
fn save_to<T: Serialize>(file: &Path, value: &T) {
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(file, json);
    }
}

/// Read the user's settings; anything unreadable means "defaults". A missing
/// token is filled in on the spot, so the webhook section always has one to
/// show and enabling it just works.
fn load_config() -> AutomationConfig {
    let mut config = load_from(&config_path());
    normalize(&mut config);

    if config.webhook.token.is_empty() {
        config.webhook.token = generate_token();
        save_config(&config);
    }

    config
}

fn load_from(file: &Path) -> AutomationConfig {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_config(config: &AutomationConfig) {
    save_to(&config_path(), config);
}

/// When each automatic source last fired, persisted so cooldowns and the
/// schedule keep holding across a restart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RanAt {
    scheduled: Option<u64>,
    disk: Option<u64>,
    memory: Option<u64>,
}

fn load_ran_at() -> RanAt {
    std::fs::read_to_string(ran_at_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_ran_at(ran_at: &RanAt) {
    save_to(&ran_at_path(), ran_at);
}

/* --------------------------------- The log ------------------------------- */

/// What an automated run does. Same spelling as the webhook routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutomationAction {
    DeepClean,
    MemoryClean,
}

/// What asked for a run, so the log shows where a cleanup came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerSource {
    Schedule,
    Disk,
    Memory,
    Webhook,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationLogEntry {
    /// Epoch milliseconds, when the run finished.
    pub at: u64,
    pub source: TriggerSource,
    pub action: AutomationAction,
    pub succeeded: bool,
    pub freed_bytes: u64,
    pub item_count: usize,
    pub failed_count: usize,
    pub duration_ms: u64,
    /// Detail worth keeping: failure reasons, or the platform's own summary.
    pub message: String,
}

/// Read the log, oldest entry first.
fn load_log() -> Vec<AutomationLogEntry> {
    load_log_from(&log_path())
}

fn load_log_from(file: &Path) -> Vec<AutomationLogEntry> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Append one finished run, trimming the oldest entries beyond the cap.
fn append_log(entry: &AutomationLogEntry) {
    append_log_to(&log_path(), entry);
}

fn append_log_to(file: &Path, entry: &AutomationLogEntry) {
    let mut entries = load_log_from(file);
    entries.push(entry.clone());

    if entries.len() > MAX_LOG_ENTRIES {
        let excess = entries.len() - MAX_LOG_ENTRIES;
        entries.drain(..excess);
    }

    save_to(file, &entries);
}

/* ------------------------------ Local wall clock -------------------------- */

/// The wall clock the schedule is expressed in: local time, because "8:00"
/// means the user's 8:00. `std` has no timezone support, so each platform
/// lends its own primitive.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LocalNow {
    /// Seconds since the Unix epoch — what the arithmetic runs on.
    epoch_secs: i64,
    hour: u32,
    minute: u32,
    second: u32,
    /// 0 = Sunday … 6 = Saturday.
    weekday: u32,
}

#[cfg(unix)]
fn local_now() -> Option<LocalNow> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let time = elapsed.as_secs() as libc::time_t;

    // SAFETY: `localtime_r` fills the `tm` handed to it and returns either
    // that pointer or null; the value is only read back through that pointer.
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    let filled = unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) };
    if filled.is_null() {
        return None;
    }
    // SAFETY: `localtime_r` succeeded, so `tm` is initialised.
    let tm = unsafe { tm.assume_init() };

    Some(LocalNow {
        epoch_secs: elapsed.as_secs() as i64,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
        weekday: tm.tm_wday as u32,
    })
}

#[cfg(target_os = "windows")]
fn local_now() -> Option<LocalNow> {
    use windows::Win32::System::SystemInformation::GetLocalTime;

    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;

    // SAFETY: `GetLocalTime` fills the struct it returns and cannot fail.
    let time = unsafe { GetLocalTime() };

    Some(LocalNow {
        epoch_secs,
        hour: time.wHour as u32,
        minute: time.wMinute as u32,
        second: time.wSecond as u32,
        // SYSTEMTIME counts 0 = Sunday, the same as the config.
        weekday: time.wDayOfWeek as u32,
    })
}

/// The hours a day runs at: `start`, `start + interval`, … up to `end`.
fn slot_hours(schedule: &ScheduleConfig) -> Vec<u32> {
    let step = u32::from(schedule.interval_hours.max(1));
    let mut hours = Vec::new();
    let mut hour = u32::from(schedule.start_hour);
    while hour <= u32::from(schedule.end_hour) {
        hours.push(hour);
        hour += step;
    }
    hours
}

/// The most recent slot that is due and unclaimed: today, on a selected
/// weekday, no longer than `grace_secs` past its hour, and after the last run
/// (epoch milliseconds). Returns the slot in epoch seconds.
fn due_slot(
    schedule: &ScheduleConfig,
    now: LocalNow,
    last_run_at: Option<u64>,
    grace_secs: i64,
) -> Option<u64> {
    if !schedule.days.contains(&(now.weekday as u8)) {
        return None;
    }

    let seconds_into_day = i64::from(now.hour * 3600 + now.minute * 60 + now.second);
    let day_start = now.epoch_secs - seconds_into_day;

    slot_hours(schedule).into_iter().rev().find_map(|hour| {
        let slot = day_start + i64::from(hour) * 3600;
        let age = now.epoch_secs - slot;
        let unclaimed = last_run_at.map_or(true, |last| ((last / 1000) as i64) < slot);
        (age >= 0 && age <= grace_secs && unclaimed).then_some(slot as u64)
    })
}

/// When the schedule next fires, in epoch milliseconds, for the UI to show.
fn next_run_at(schedule: &ScheduleConfig, now: LocalNow) -> Option<u64> {
    let seconds_into_day = i64::from(now.hour * 3600 + now.minute * 60 + now.second);
    let day_start = now.epoch_secs - seconds_into_day;

    // Today plus the next full week covers every selected day at least once.
    for offset in 0..8 {
        let weekday = ((now.weekday + offset) % 7) as u8;
        if !schedule.days.contains(&weekday) {
            continue;
        }

        for hour in slot_hours(schedule) {
            let slot = day_start + offset as i64 * 86_400 + i64::from(hour) * 3600;
            if slot > now.epoch_secs {
                return Some(slot as u64 * 1000);
            }
        }
    }

    None
}

/* -------------------------------- The engine ------------------------------ */

/// Everything the automation engine owns at runtime.
pub struct AutomationState {
    config: Mutex<AutomationConfig>,
    ran_at: Mutex<RanAt>,
    webhook: Mutex<WebhookRuntime>,
    /// One automated cleanup at a time, whatever asked for it.
    running: AtomicBool,
    /// Set once the background threads have been started.
    started: AtomicBool,
}

impl Default for AutomationState {
    fn default() -> Self {
        Self {
            config: Mutex::new(load_config()),
            ran_at: Mutex::new(load_ran_at()),
            webhook: Mutex::new(WebhookRuntime::default()),
            running: AtomicBool::new(false),
            started: AtomicBool::new(false),
        }
    }
}

/// Start the scheduler and the webhook listener. Called once, from setup; the
/// threads outlive every window, which is what "runs in the background" means
/// here.
pub fn start(app: &AppHandle) {
    let Some(state) = app.try_state::<AutomationState>() else {
        return;
    };
    if state.started.swap(true, Ordering::SeqCst) {
        return;
    }

    let webhook = state
        .config
        .lock()
        .map(|config| config.webhook.clone())
        .unwrap_or_default();
    apply_webhook(app, &webhook);

    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK_INTERVAL);
        tick(&app);
    });
}

/// One look at the clock and the machine: fire whatever is due.
fn tick(app: &AppHandle) {
    let Some(state) = app.try_state::<AutomationState>() else {
        return;
    };
    let config = match state.config.lock() {
        Ok(config) => config.clone(),
        Err(_) => return,
    };

    // One run at a time. A slot skipped this way is not lost: its grace
    // window lets it fire again on one of the next ticks.
    if state.running.load(Ordering::SeqCst) {
        return;
    }

    if config.schedule.enabled {
        let last_run = state.ran_at.lock().ok().and_then(|ran_at| ran_at.scheduled);
        if let Some(now) = local_now() {
            if due_slot(&config.schedule, now, last_run, SLOT_GRACE_SECS).is_some() {
                start_run(app, AutomationAction::DeepClean, TriggerSource::Schedule);
            }
        }
    }

    if config.disk_trigger.enabled && cooldown_over(&state, TriggerSource::Disk, DISK_COOLDOWN_MS) {
        let in_use = analyze::boot_volume().map(|disk| {
            f64::from(format::ratio(disk.used_bytes, disk.total_bytes)) * 100.0
        });
        if in_use.is_some_and(|percent| percent >= f64::from(config.disk_trigger.threshold_percent))
        {
            start_run(app, AutomationAction::DeepClean, TriggerSource::Disk);
        }
    }

    if config.memory_trigger.enabled
        && cooldown_over(&state, TriggerSource::Memory, MEMORY_COOLDOWN_MS)
    {
        // Only the global memory figures are wanted, so this stays clear of
        // the process-table walk a full sampler would do.
        let mut system = System::new();
        system.refresh_memory();
        let in_use =
            f64::from(format::ratio(system.used_memory(), system.total_memory())) * 100.0;
        if in_use >= f64::from(config.memory_trigger.threshold_percent) {
            start_run(app, AutomationAction::MemoryClean, TriggerSource::Memory);
        }
    }
}

/// Whether a usage trigger is allowed to fire: it never has, or its quiet
/// period has passed.
fn cooldown_over(state: &AutomationState, source: TriggerSource, cooldown_ms: u64) -> bool {
    let last = state.ran_at.lock().ok().and_then(|ran_at| match source {
        TriggerSource::Disk => ran_at.disk,
        TriggerSource::Memory => ran_at.memory,
        _ => None,
    });

    last.map_or(true, |at| {
        crate::commands::clean::now_millis().saturating_sub(at) >= cooldown_ms
    })
}

/// Note when an automatic source last fired, in memory and on disk.
fn remember_run(state: &AutomationState, source: TriggerSource) {
    let at = crate::commands::clean::now_millis();
    let Ok(mut ran_at) = state.ran_at.lock() else {
        return;
    };

    match source {
        TriggerSource::Schedule => ran_at.scheduled = Some(at),
        TriggerSource::Disk => ran_at.disk = Some(at),
        TriggerSource::Memory => ran_at.memory = Some(at),
        TriggerSource::Webhook | TriggerSource::Manual => return,
    }
    save_ran_at(&ran_at);
}

/// Claim the single run slot and start the run on its own thread. Returns
/// `false` when a run is already in flight — the caller decides how to say so.
fn start_run(app: &AppHandle, action: AutomationAction, source: TriggerSource) -> bool {
    let Some(state) = app.try_state::<AutomationState>() else {
        return false;
    };
    if state
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }

    remember_run(&state, source);
    let empty_trash = state
        .config
        .lock()
        .map(|config| config.empty_trash)
        .unwrap_or(false);

    let app = app.clone();
    std::thread::spawn(move || {
        // A thread of its own: runs take minutes, and the scheduler and the
        // webhook have to keep answering meanwhile.
        let entry = tauri::async_runtime::block_on(run_action(&app, action, source, empty_trash));
        append_log(&entry);
        let _ = app.emit(LOG_EVENT, &entry);

        if let Some(state) = app.try_state::<AutomationState>() {
            state.running.store(false, Ordering::SeqCst);
        }
    });

    true
}

/// Perform one automated cleanup and describe it. Never fails: a failure is
/// part of the report.
async fn run_action(
    app: &AppHandle,
    action: AutomationAction,
    source: TriggerSource,
    empty_trash: bool,
) -> AutomationLogEntry {
    let started = Instant::now();
    let mut entry = AutomationLogEntry {
        at: 0,
        source,
        action,
        succeeded: false,
        freed_bytes: 0,
        item_count: 0,
        failed_count: 0,
        duration_ms: 0,
        message: String::new(),
    };

    match action {
        AutomationAction::DeepClean => {
            // The selection matches the Deep Clean page's own default: safe
            // categories, safe items. An unattended run cannot ask questions,
            // so it does not gamble on the caution categories either.
            let paths = clean::auto_clean_paths(&CancelToken::default());

            match clean::clean_paths(paths, false).await {
                Ok(outcome) => {
                    entry.freed_bytes = outcome.freed_bytes;
                    entry.item_count = outcome.removed_paths.len();
                    entry.failed_count = outcome.failed_paths.len();
                    entry.succeeded =
                        outcome.failed_paths.is_empty() || !outcome.removed_paths.is_empty();

                    if empty_trash {
                        match clean::empty_trash().await {
                            Ok(trash) => {
                                entry.freed_bytes += trash.freed_bytes;
                                entry.item_count += trash.removed_paths.len();
                                entry.failed_count += trash.failed_paths.len();
                            }
                            Err(error) => entry.message = format!("trash not emptied: {error}"),
                        }
                    }
                }
                Err(error) => entry.message = error.to_string(),
            }
        }
        AutomationAction::MemoryClean => {
            // The same in-process purge the System Optimize page runs; it
            // needs no password, which an unattended run could not supply.
            let outcome = optimize::purge_memory(app).await;
            entry.succeeded = outcome.succeeded;
            entry.message = outcome.message;
        }
    }

    entry.duration_ms = started.elapsed().as_millis() as u64;
    entry.at = crate::commands::clean::now_millis();
    entry
}

/* --------------------------------- Webhook -------------------------------- */

#[derive(Debug, Default)]
struct WebhookRuntime {
    /// The live listener, while one is up.
    listener: Option<RunningWebhook>,
    address: Option<String>,
    /// Why the last start attempt failed, if it did (port in use, …).
    error: Option<String>,
}

#[derive(Debug)]
struct RunningWebhook {
    shutdown: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

/// Shared with the listener thread for the life of one server.
struct WebhookContext {
    token: String,
    app: AppHandle,
}

/// Point the listener at `config`, replacing whatever was running — called at
/// startup and on every settings save. A bind failure does not fail the save;
/// it is stored and shown next to the enable switch.
fn apply_webhook(app: &AppHandle, config: &WebhookConfig) {
    let Some(state) = app.try_state::<AutomationState>() else {
        return;
    };

    stop_webhook(&state);

    let Ok(mut runtime) = state.webhook.lock() else {
        return;
    };
    runtime.error = None;
    runtime.address = None;
    drop(runtime);

    if !config.enabled {
        return;
    }

    // The listener is created here, on the caller's thread, so a port clash
    // is known before the thread starts and can be reported to the UI.
    let address = format!("127.0.0.1:{}", config.port);
    let listener = match TcpListener::bind(&address) {
        Ok(listener) => listener,
        Err(error) => {
            if let Ok(mut runtime) = state.webhook.lock() {
                runtime.error = Some(error.to_string());
            }
            return;
        }
    };
    let _ = listener.set_nonblocking(true);

    // Held across the spawn and both stores, so a poisoned lock cannot leave
    // the listener thread running with no way to stop it.
    let mut runtime = match state.webhook.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let context = WebhookContext {
        token: config.token.clone(),
        app: app.clone(),
    };

    std::thread::spawn({
        let shutdown = Arc::clone(&shutdown);
        let done = Arc::clone(&done);
        move || {
            while !shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle_connection(stream, &context),
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(ACCEPT_POLL);
                    }
                    Err(_) => std::thread::sleep(ACCEPT_POLL),
                }
            }
            done.store(true, Ordering::SeqCst);
        }
    });

    runtime.address = Some(address);
    runtime.listener = Some(RunningWebhook { shutdown, done });
}

/// Signal the listener thread and wait for it to release the port, so a
/// restart on the same port can bind right away.
fn stop_webhook(state: &AutomationState) {
    let running = state
        .webhook
        .lock()
        .ok()
        .and_then(|mut runtime| runtime.listener.take());
    let Some(running) = running else { return };

    running.shutdown.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + WEBHOOK_STOP_WAIT;
    while !running.done.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn webhook_status(state: &AutomationState) -> WebhookStatus {
    state
        .webhook
        .lock()
        .map(|runtime| WebhookStatus {
            running: runtime.listener.is_some(),
            address: runtime.address.clone(),
            error: runtime.error.clone(),
        })
        .unwrap_or_default()
}

/// Serve one request and close: the endpoints are tiny, keep-alive buys
/// nothing.
fn handle_connection(mut stream: TcpStream, context: &WebhookContext) {
    let head = match read_head(&mut stream) {
        Ok(head) => head,
        Err(status) => {
            respond(
                &mut stream,
                status,
                &serde_json::json!({ "ok": false, "error": "malformed request" }),
            );
            return;
        }
    };

    // Everything behind the token can delete files, so nothing is answered
    // before the check.
    let authorized = request_token(&head)
        .is_some_and(|given| tokens_match(&context.token, given));
    if !authorized {
        respond(
            &mut stream,
            401,
            &serde_json::json!({ "ok": false, "error": "invalid or missing token" }),
        );
        return;
    }

    let busy = context
        .app
        .try_state::<AutomationState>()
        .is_some_and(|state| state.running.load(Ordering::SeqCst));

    match route(&head.method, &head.path, busy) {
        Route::Start(action) => {
            if start_run(&context.app, action, TriggerSource::Webhook) {
                respond(
                    &mut stream,
                    202,
                    &serde_json::json!({ "ok": true, "action": action, "status": "started" }),
                );
            } else {
                respond(
                    &mut stream,
                    409,
                    &serde_json::json!({ "ok": false, "error": "a cleanup is already running" }),
                );
            }
        }
        Route::Reply(status, body) => respond(&mut stream, status, &body),
    }
}

/// The request line and headers. The body is read but never stored — no
/// endpoint takes one.
#[derive(Debug, PartialEq)]
struct RequestHead {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    content_length: usize,
}

/// Read the head of one HTTP request, plus whatever body it promised. The
/// returned status is what to answer with when the bytes cannot be understood.
fn read_head(stream: &mut TcpStream) -> std::result::Result<RequestHead, u16> {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];

    let header_end = loop {
        if let Some(end) = find_subsequence(&buffer, b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(413);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(400),
        }
    };

    let head = std::str::from_utf8(&buffer[..header_end]).map_err(|_| 400u16)?;
    let head = parse_head(head).ok_or(400u16)?;

    if head.content_length > MAX_BODY_BYTES {
        return Err(413);
    }

    // Drain the promised body so the client is not cut off mid-send; what
    // arrives is discarded.
    let wanted = header_end + 4 + head.content_length;
    while buffer.len() < wanted {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(400),
        }
    }

    Ok(head)
}

/// Split the request line and headers out. `None` when the bytes are not an
/// HTTP/1.x request at all.
fn parse_head(head: &str) -> Option<RequestHead> {
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_ascii_uppercase();
    let target = request_line.next().unwrap_or("/");
    // An absolute-form target (`http://host/path`) is legal; the authority is
    // dropped and only the path kept.
    let target = match target.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((_, path)) => format!("/{path}"),
            None => "/".to_string(),
        },
        None => target.to_string(),
    };
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    let mut content_length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }

    Some(RequestHead {
        method,
        path,
        query,
        headers,
        content_length,
    })
}

/// What a request maps to, before the run slot is claimed.
enum Route {
    Start(AutomationAction),
    Reply(u16, serde_json::Value),
}

/// The whole routing table. The token check happened before this.
fn route(method: &str, path: &str, busy: bool) -> Route {
    match (method, path) {
        ("GET", "/api/status") => Route::Reply(200, serde_json::json!({ "ok": true, "busy": busy })),
        ("POST", "/api/deep-clean") => Route::Start(AutomationAction::DeepClean),
        ("POST", "/api/memory-clean") => Route::Start(AutomationAction::MemoryClean),
        ("GET", _) | ("POST", _) => {
            Route::Reply(404, serde_json::json!({ "ok": false, "error": "not found" }))
        }
        _ => Route::Reply(
            405,
            serde_json::json!({ "ok": false, "error": "method not allowed" }),
        ),
    }
}

/// The token a request presents: `Authorization: Bearer …` or a `token=` query
/// parameter, whichever was easier for the caller.
fn request_token(head: &RequestHead) -> Option<&str> {
    if let Some(value) = head
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str())
    {
        let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
        if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
            return Some(token.trim());
        }
    }

    head.query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "token" && !value.is_empty()).then_some(value)
    })
}

/// Length-checked, branch-free comparison, so a wrong token cannot be
/// narrowed down by watching response times.
fn tokens_match(expected: &str, given: &str) -> bool {
    let (expected, given) = (expected.as_bytes(), given.as_bytes());
    if expected.len() != given.len() {
        return false;
    }

    let mut difference = 0u8;
    for (a, b) in expected.iter().zip(given) {
        difference |= a ^ b;
    }
    difference == 0
}

/// First index of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Answer with a small JSON body. One write, so the client never sees a torn
/// response.
fn respond(stream: &mut TcpStream, status: u16, body: &serde_json::Value) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        _ => "Error",
    };
    let text = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/* -------------------------------- Commands -------------------------------- */

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookStatus {
    pub running: bool,
    pub address: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationStatus {
    pub config: AutomationConfig,
    /// Epoch milliseconds of the next scheduled run, while the schedule is on.
    pub next_run_at: Option<u64>,
    /// A run — automatic or manual — is in flight right now.
    pub running: bool,
    pub webhook: WebhookStatus,
}

fn status_of(state: &AutomationState) -> AutomationStatus {
    let config = state
        .config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_default();

    let next_run_at = if config.schedule.enabled {
        local_now().and_then(|now| next_run_at(&config.schedule, now))
    } else {
        None
    };

    AutomationStatus {
        config,
        next_run_at,
        running: state.running.load(Ordering::SeqCst),
        webhook: webhook_status(state),
    }
}

/// The automation settings, plus what the engine is doing right now.
#[tauri::command]
pub async fn automation_status(state: State<'_, AutomationState>) -> Result<AutomationStatus> {
    Ok(status_of(&state))
}

/// Replace the settings: normalised, validated, persisted and applied — which
/// includes moving the webhook listener if its settings changed.
#[tauri::command]
pub async fn automation_set_config(
    app: AppHandle,
    state: State<'_, AutomationState>,
    mut config: AutomationConfig,
) -> Result<AutomationStatus> {
    normalize(&mut config);
    if let Err(message) = validate(&config) {
        return Err(WindleError::Command {
            command: "automation".into(),
            message,
        });
    }

    if let Ok(mut stored) = state.config.lock() {
        *stored = config.clone();
    }
    save_config(&config);
    apply_webhook(&app, &config.webhook);

    Ok(status_of(&state))
}

/// Recent runs, newest first.
#[tauri::command]
pub async fn automation_logs(limit: Option<usize>) -> Vec<AutomationLogEntry> {
    let mut entries = load_log();
    entries.reverse();

    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    entries
}

#[tauri::command]
pub async fn automation_clear_logs() {
    save_to(&log_path(), &Vec::<AutomationLogEntry>::new());
}

/// Run one action right now, on top of whatever the automatic rules are.
#[tauri::command]
pub async fn automation_run_now(app: AppHandle, action: AutomationAction) -> Result<()> {
    if start_run(&app, action, TriggerSource::Manual) {
        Ok(())
    } else {
        Err(WindleError::Command {
            command: "automation".into(),
            message: "a cleanup is already running".into(),
        })
    }
}

/// A fresh webhook token for the UI to store. Not saved here — saving the
/// settings is what makes it real.
#[tauri::command]
pub async fn automation_generate_token() -> String {
    generate_token()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support;

    /// A private file per test, so parallel runs cannot collide and the
    /// user's real settings are never touched.
    fn scratch(name: &str) -> PathBuf {
        let file = test_support::scratch_base().join(format!(
            "windle-automation-{name}-{}.json",
            std::process::id()
        ));
        std::fs::remove_file(&file).ok();
        file
    }

    /// A local-time reading built by hand, so the schedule arithmetic can be
    /// checked without depending on when the test runs.
    fn local(hour: u32, minute: u32, weekday: u32) -> LocalNow {
        LocalNow {
            // An arbitrary Monday: 2024-01-01 00:00:00 UTC.
            epoch_secs: 1_704_067_200 + i64::from(hour) * 3600 + i64::from(minute) * 60,
            hour,
            minute,
            second: 0,
            weekday,
        }
    }

    fn schedule() -> ScheduleConfig {
        ScheduleConfig {
            enabled: true,
            days: (0..=6).collect(),
            start_hour: 8,
            end_hour: 20,
            interval_hours: 2,
        }
    }

    #[test]
    fn slots_step_from_start_to_end_inclusive() {
        let mut config = schedule();
        assert_eq!(slot_hours(&config), vec![8, 10, 12, 14, 16, 18, 20]);

        config.interval_hours = 6;
        assert_eq!(slot_hours(&config), vec![8, 14, 20]);

        // An interval longer than the window leaves a single run at the start.
        config.interval_hours = 24;
        assert_eq!(slot_hours(&config), vec![8]);

        // start == end is a once-a-day schedule.
        config.start_hour = 12;
        config.end_hour = 12;
        assert_eq!(slot_hours(&config), vec![12]);
    }

    #[test]
    fn a_due_slot_fires_once_within_its_grace_window() {
        let config = schedule();

        // Three minutes after the 10:00 slot, nothing having run yet.
        let now = local(10, 3, 1);
        assert_eq!(due_slot(&config, now, None, SLOT_GRACE_SECS), Some(1_704_067_200 + 10 * 3600));

        // Half an hour late is too late.
        assert_eq!(due_slot(&config, local(10, 30, 1), None, SLOT_GRACE_SECS), None);

        // Exactly at the end of the grace window still counts.
        assert!(due_slot(&config, local(10, 10, 1), None, SLOT_GRACE_SECS).is_some());

        // Already run inside this slot: next tick must not fire it again.
        let ran_at = (1_704_067_200 + 10 * 3600) * 1000 + 30_000;
        assert_eq!(due_slot(&config, local(10, 3, 1), Some(ran_at), SLOT_GRACE_SECS), None);

        // A day the schedule does not cover never fires.
        let mut weekdays_only = config.clone();
        weekdays_only.days = vec![1, 2, 3, 4, 5];
        assert_eq!(
            due_slot(&weekdays_only, local(10, 3, 0), None, SLOT_GRACE_SECS),
            None
        );

        // Just before a slot it is not due yet.
        assert_eq!(due_slot(&config, local(9, 59, 1), None, SLOT_GRACE_SECS), None);
    }

    #[test]
    fn the_next_run_is_the_upcoming_slot() {
        let mut config = schedule();
        config.days = vec![2];

        // Tuesday 7:00: today's first slot is still ahead.
        assert_eq!(
            next_run_at(&config, local(7, 0, 2)),
            Some((1_704_067_200 + 8 * 3600) as u64 * 1000)
        );

        // Tuesday 19:30: the 20:00 slot is next.
        assert_eq!(
            next_run_at(&config, local(19, 30, 2)),
            Some((1_704_067_200 + 20 * 3600) as u64 * 1000)
        );

        // Tuesday 21:00: the window is over, next Tuesday it is.
        assert_eq!(
            next_run_at(&config, local(21, 0, 2)),
            Some((1_704_067_200 + 7 * 86_400 + 8 * 3600) as u64 * 1000)
        );

        // A day set that includes today again after the window still lands
        // within the week.
        let every_day = schedule();
        assert_eq!(
            next_run_at(&every_day, local(21, 0, 2)),
            Some((1_704_067_200 + 86_400 + 8 * 3600) as u64 * 1000)
        );
    }

    #[test]
    fn normalisation_brings_out_of_range_values_back() {
        let mut config = AutomationConfig {
            schedule: ScheduleConfig {
                enabled: true,
                days: vec![5, 1, 1, 9],
                start_hour: 20,
                end_hour: 8,
                interval_hours: 0,
            },
            disk_trigger: ThresholdTrigger {
                enabled: true,
                threshold_percent: 0,
            },
            memory_trigger: ThresholdTrigger {
                enabled: true,
                threshold_percent: 120,
            },
            empty_trash: true,
            webhook: WebhookConfig {
                enabled: false,
                port: 80,
                token: "x".repeat(16),
            },
        };

        normalize(&mut config);

        assert_eq!(config.schedule.days, vec![1, 5]);
        assert_eq!((config.schedule.start_hour, config.schedule.end_hour), (8, 20));
        assert_eq!(config.schedule.interval_hours, 1);
        assert_eq!(config.disk_trigger.threshold_percent, 1);
        assert_eq!(config.memory_trigger.threshold_percent, 100);
        assert_eq!(config.webhook.port, DEFAULT_WEBHOOK_PORT);
    }

    #[test]
    fn validation_refuses_settings_that_would_misbehave() {
        let mut config = AutomationConfig::default();

        // A disabled schedule may keep an empty day set.
        config.schedule.days.clear();
        assert!(validate(&config).is_ok());

        // Enabling it without a day would never fire.
        config.schedule.enabled = true;
        assert!(validate(&config).is_err());

        config.schedule.days = vec![1];
        assert!(validate(&config).is_ok());

        // An enabled webhook with a placeholder token is refused.
        config.webhook.enabled = true;
        config.webhook.token = "short".into();
        assert!(validate(&config).is_err());

        config.webhook.token = generate_token();
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn generated_tokens_are_hex_and_fresh() {
        let first = generate_token();
        let second = generate_token();

        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn the_config_round_trips_through_the_file() {
        let file = scratch("config");

        let mut config = AutomationConfig::default();
        config.schedule.enabled = true;
        config.disk_trigger.enabled = true;
        config.webhook.token = "0123456789abcdef".into();
        save_to(&file, &config);

        assert_eq!(load_from(&file), config);

        // A missing or corrupt file reads as defaults rather than failing.
        std::fs::remove_file(&file).ok();
        assert_eq!(load_from(&file), AutomationConfig::default());

        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load_from(&file), AutomationConfig::default());

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let file = scratch("log");
        let entry = |at: u64| AutomationLogEntry {
            at,
            source: TriggerSource::Schedule,
            action: AutomationAction::DeepClean,
            succeeded: true,
            freed_bytes: 10,
            item_count: 1,
            failed_count: 0,
            duration_ms: 5,
            message: String::new(),
        };

        for at in 0..(MAX_LOG_ENTRIES as u64 + 10) {
            append_log_to(&file, &entry(at));
        }

        let entries = load_log_from(&file);
        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(entries.first().unwrap().at, 10);
        assert_eq!(entries.last().unwrap().at, MAX_LOG_ENTRIES as u64 + 9);

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn the_wire_shape_matches_the_frontend_types() {
        let json = serde_json::to_value(AutomationConfig::default()).unwrap();
        assert_eq!(json["schedule"]["startHour"], 8);
        assert_eq!(json["schedule"]["intervalHours"], 2);
        assert_eq!(json["diskTrigger"]["thresholdPercent"], 95);
        assert_eq!(json["memoryTrigger"]["thresholdPercent"], 90);
        assert_eq!(json["emptyTrash"], false);
        assert_eq!(json["webhook"]["port"], DEFAULT_WEBHOOK_PORT);

        let entry = AutomationLogEntry {
            at: 5,
            source: TriggerSource::Disk,
            action: AutomationAction::DeepClean,
            succeeded: true,
            freed_bytes: 1,
            item_count: 2,
            failed_count: 0,
            duration_ms: 3,
            message: String::new(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["source"], "disk");
        assert_eq!(json["action"], "deep-clean");
        assert_eq!(json["freedBytes"], 1);
        assert_eq!(json["itemCount"], 2);
    }

    #[test]
    fn a_request_head_is_parsed_into_method_path_and_headers() {
        let head = "POST /api/deep-clean?token=abc HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nX-Test: Value";

        let parsed = parse_head(head).expect("the head must parse");
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/api/deep-clean");
        assert_eq!(parsed.query, "token=abc");
        assert_eq!(parsed.content_length, 2);
        assert_eq!(
            parsed.headers,
            vec![
                ("host".to_string(), "127.0.0.1".to_string()),
                ("content-length".to_string(), "2".to_string()),
                ("x-test".to_string(), "Value".to_string()),
            ]
        );

        // Absolute-form targets keep only the path.
        let absolute = parse_head("GET http://127.0.0.1:1/api/status HTTP/1.1").unwrap();
        assert_eq!(absolute.path, "/api/status");
        assert_eq!(absolute.query, "");

        assert!(parse_head("").is_none());
    }

    #[test]
    fn the_token_comes_from_the_header_or_the_query() {
        let bearer = parse_head("POST /x HTTP/1.1\r\nAuthorization: Bearer sec-ret").unwrap();
        assert_eq!(request_token(&bearer), Some("sec-ret"));

        let query = parse_head("POST /x?first=1&token=sec-ret HTTP/1.1").unwrap();
        assert_eq!(request_token(&query), Some("sec-ret"));

        let both = parse_head("POST /x?token=query HTTP/1.1\r\nAuthorization: bearer header").unwrap();
        assert_eq!(request_token(&both), Some("header"));

        let neither = parse_head("POST /x HTTP/1.1").unwrap();
        assert_eq!(request_token(&neither), None);

        // A different scheme is not a token.
        let basic = parse_head("POST /x HTTP/1.1\r\nAuthorization: Basic dXNlcg==").unwrap();
        assert_eq!(request_token(&basic), None);
    }

    #[test]
    fn tokens_must_match_exactly() {
        assert!(tokens_match("abc123", "abc123"));
        assert!(!tokens_match("abc123", "abc124"));
        assert!(!tokens_match("abc123", "abc1234"));
        assert!(!tokens_match("abc123", ""));
    }

    #[test]
    fn the_routes_are_the_documented_ones() {
        assert!(matches!(
            route("POST", "/api/deep-clean", false),
            Route::Start(AutomationAction::DeepClean)
        ));
        assert!(matches!(
            route("POST", "/api/memory-clean", false),
            Route::Start(AutomationAction::MemoryClean)
        ));

        match route("GET", "/api/status", true) {
            Route::Reply(200, body) => assert_eq!(body["busy"], true),
            _ => panic!("status must answer 200"),
        }

        match route("GET", "/api/nope", false) {
            Route::Reply(404, _) => {}
            _ => panic!("unknown paths must 404"),
        }

        match route("PUT", "/api/deep-clean", false) {
            Route::Reply(405, _) => {}
            _ => panic!("unknown methods must 405"),
        }
    }

    #[test]
    fn subsequence_search_finds_the_separator() {
        assert_eq!(find_subsequence(b"ab\r\n\r\ncd", b"\r\n\r\n"), Some(2));
        assert_eq!(find_subsequence(b"abcd", b"\r\n\r\n"), None);
    }
}

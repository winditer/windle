//! Live Monitor — CPU, memory, network, disk I/O and processes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{Components, Networks, ProcessesToUpdate, System, Users};
use tauri::{AppHandle, Emitter, State};

use crate::utils::{format, WindleError, Result};

pub const SNAPSHOT_EVENT: &str = "monitor://snapshot";

/// `sysinfo` needs this much time between two CPU refreshes before the usage
/// figures mean anything.
const MIN_CPU_INTERVAL: Duration = Duration::from_millis(200);

/// Shelling out to `pmset`/`ioreg` every second would be wasteful, so battery
/// readings are cached for this long.
const BATTERY_TTL: Duration = Duration::from_secs(10);

/// How many processes a snapshot carries.
const TOP_PROCESS_COUNT: usize = 8;

/// Clamp the streaming interval so a bad value cannot spin the CPU.
const MIN_INTERVAL_MS: u64 = 250;
const MAX_INTERVAL_MS: u64 = 60_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuStats {
    /// Overall usage, 0–1.
    pub usage: f32,
    pub per_core: Vec<f32>,
    pub load_average: [f64; 3],
    pub temperature_c: Option<f32>,
    /// Fan speed (RPM) read from the AppleSMC IOKit service. `None` on
    /// machines without a fan (Apple Silicon laptops, Mac mini, etc.).
    pub fan_speed_rpm: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    /// Rough stand-in for macOS memory pressure, 0–1.
    pub pressure: f32,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStats {
    pub interface_name: String,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskIoStats {
    pub read_bytes_per_sec: u64,
    pub write_bytes_per_sec: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
    pub user: Option<String>,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatteryStats {
    pub level: f32,
    pub is_charging: bool,
    pub cycle_count: Option<u32>,
    pub health_percent: Option<f32>,
    pub time_remaining_minutes: Option<u32>,
}

/// Static facts about the machine, sent along with every snapshot so the header
/// can label the readings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub hostname: Option<String>,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub cpu_brand: Option<String>,
    pub physical_cores: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemSnapshot {
    pub timestamp: u64,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub network: Vec<NetworkStats>,
    pub disk_io: DiskIoStats,
    pub top_processes: Vec<ProcessInfo>,
    pub battery: Option<BatteryStats>,
    pub uptime_seconds: u64,
    pub host: HostInfo,
}

/// Owns the `sysinfo` handles and the timestamps needed to turn the counters
/// into per-second rates.
pub struct Sampler {
    system: System,
    networks: Networks,
    users: Users,
    /// Thermal sensors. Refreshed in place instead of rebuilding every second.
    components: Components,
    /// When the counters were last read, so rates can be scaled properly.
    last_sampled: Instant,
    battery: Option<BatteryStats>,
    battery_read_at: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();

        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            last_sampled: Instant::now(),
            battery: None,
            battery_read_at: None,
        }
    }

    /// Refresh every counter and derive one snapshot from the delta since the
    /// previous call.
    pub fn sample(&mut self) -> SystemSnapshot {
        // CPU percentages are computed against the previous refresh, so give
        // the kernel a moment when we are called back-to-back.
        let waited = self.last_sampled.elapsed();
        if waited < MIN_CPU_INTERVAL {
            std::thread::sleep(MIN_CPU_INTERVAL - waited);
        }

        self.system.refresh_cpu_all();
        self.system.refresh_memory();
        self.system
            .refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh();

        let elapsed = self.last_sampled.elapsed();
        self.last_sampled = Instant::now();
        // Guard the divisor: a sub-millisecond gap would inflate every rate.
        let seconds = elapsed.as_secs_f64().max(0.001);

        SystemSnapshot {
            timestamp: super::clean::now_millis(),
            cpu: self.cpu(),
            memory: self.memory(),
            network: self.network(seconds),
            disk_io: self.disk_io(seconds),
            top_processes: self.top_processes(TOP_PROCESS_COUNT),
            battery: self.battery(),
            uptime_seconds: System::uptime(),
            host: host_info(&self.system),
        }
    }

    fn cpu(&mut self) -> CpuStats {
        let per_core: Vec<f32> = self
            .system
            .cpus()
            .iter()
            .map(|cpu| (cpu.cpu_usage() / 100.0).clamp(0.0, 1.0))
            .collect();

        let usage = if per_core.is_empty() {
            0.0
        } else {
            per_core.iter().sum::<f32>() / per_core.len() as f32
        };

        let load = System::load_average();

        CpuStats {
            usage,
            per_core,
            load_average: [load.one, load.five, load.fifteen],
            temperature_c: self.hottest_component(),
            fan_speed_rpm: read_fan_speed_rpm(),
        }
    }

    fn memory(&self) -> MemoryStats {
        let total = self.system.total_memory();
        let available = self.system.available_memory();

        MemoryStats {
            total_bytes: total,
            used_bytes: self.system.used_memory(),
            available_bytes: available,
            // Closer to the Activity Monitor reading than used/total, because
            // macOS counts cached files as "used".
            pressure: format::ratio(total.saturating_sub(available), total),
            swap_used_bytes: self.system.used_swap(),
            swap_total_bytes: self.system.total_swap(),
        }
    }

    fn network(&self, seconds: f64) -> Vec<NetworkStats> {
        let mut interfaces: Vec<NetworkStats> = self
            .networks
            .iter()
            .map(|(name, data)| NetworkStats {
                interface_name: name.clone(),
                rx_bytes_per_sec: per_second(data.received(), seconds),
                tx_bytes_per_sec: per_second(data.transmitted(), seconds),
                total_rx_bytes: data.total_received(),
                total_tx_bytes: data.total_transmitted(),
            })
            // Skip the many idle interfaces macOS keeps around (utun, bridge…).
            .filter(|stats| stats.total_rx_bytes > 0 || stats.total_tx_bytes > 0)
            .collect();

        interfaces.sort_by(|a, b| {
            (b.rx_bytes_per_sec + b.tx_bytes_per_sec)
                .cmp(&(a.rx_bytes_per_sec + a.tx_bytes_per_sec))
                .then(a.interface_name.cmp(&b.interface_name))
        });

        interfaces
    }

    /// macOS exposes no cheap system-wide disk counter, so the per-process
    /// deltas are summed instead.
    fn disk_io(&self, seconds: f64) -> DiskIoStats {
        let (read, written) = self
            .system
            .processes()
            .values()
            .map(|process| {
                let usage = process.disk_usage();
                (usage.read_bytes, usage.written_bytes)
            })
            .fold((0u64, 0u64), |(read, written), (r, w)| {
                (read.saturating_add(r), written.saturating_add(w))
            });

        DiskIoStats {
            read_bytes_per_sec: per_second(read, seconds),
            write_bytes_per_sec: per_second(written, seconds),
        }
    }

    fn top_processes(&self, limit: usize) -> Vec<ProcessInfo> {
        let core_count = self.system.cpus().len().max(1) as f32;

        let mut processes: Vec<ProcessInfo> = self
            .system
            .processes()
            .values()
            .map(|process| ProcessInfo {
                pid: process.pid().as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                // sysinfo reports 100% per saturated core; normalise to 0–1
                // across the whole machine.
                cpu_usage: (process.cpu_usage() / 100.0 / core_count).clamp(0.0, 1.0),
                memory_bytes: process.memory(),
                user: process
                    .user_id()
                    .and_then(|uid| self.users.get_user_by_id(uid))
                    .map(|user| user.name().to_string()),
                command: process
                    .exe()
                    .map(|exe| exe.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
            .collect();

        processes.sort_by(|a, b| {
            b.cpu_usage
                .total_cmp(&a.cpu_usage)
                .then(b.memory_bytes.cmp(&a.memory_bytes))
        });
        processes.truncate(limit);
        processes
    }

    /// Cached battery reading; `pmset` and `ioreg` are only consulted every
    /// [`BATTERY_TTL`].
    fn battery(&mut self) -> Option<BatteryStats> {
        let stale = self
            .battery_read_at
            .is_none_or(|read_at| read_at.elapsed() >= BATTERY_TTL);

        if stale {
            self.battery = read_battery();
            self.battery_read_at = Some(Instant::now());
        }

        self.battery.clone()
    }

    /// Refresh and read the highest thermal sensor, reusing the cached
    /// `Components` handle instead of rebuilding it every call.
    fn hottest_component(&mut self) -> Option<f32> {
        self.components.refresh();
        hottest_component(&self.components)
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// `sysinfo` handles are expensive to build, so they live in managed state and
/// are refreshed in place.
pub struct MonitorState {
    pub sampler: Mutex<Sampler>,
    /// Incremented every time a stream starts or stops. A polling thread
    /// captures the value at spawn time and exits as soon as it no longer
    /// matches, so a superseded or stopped thread always terminates.
    pub generation: Arc<AtomicU64>,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            sampler: Mutex::new(Sampler::new()),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// One-shot reading of every metric.
#[tauri::command]
pub async fn get_snapshot(state: State<'_, MonitorState>) -> Result<SystemSnapshot> {
    let mut sampler = state
        .sampler
        .lock()
        .map_err(|_| WindleError::Command {
            command: "get_snapshot".into(),
            message: "the system sampler is unavailable".into(),
        })?;

    Ok(sampler.sample())
}

/// Start pushing snapshots on [`SNAPSHOT_EVENT`].
///
/// Each call gets its own generation number. A previous polling thread — if
/// any — sees that its generation no longer matches and exits, so there is
/// never more than one live thread at a time.
#[tauri::command]
pub async fn start_monitor(
    app: AppHandle,
    state: State<'_, MonitorState>,
    interval_ms: u64,
) -> Result<()> {
    let interval = interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
    let generation = Arc::clone(&state.generation);

    // Bump the generation so any running thread is invalidated, then capture
    // our own number.
    let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;

    // The task samples with its own handles: holding the managed state's mutex
    // for the lifetime of the stream would block `get_snapshot`.
    std::thread::spawn(move || {
        let mut sampler = Sampler::new();

        loop {
            // A newer monitor started or stop was called — bow out.
            if generation.load(Ordering::SeqCst) != my_gen {
                break;
            }

            let snapshot = sampler.sample();

            // A failed emit means the window is gone; stop rather than spin.
            if app.emit(SNAPSHOT_EVENT, snapshot).is_err() {
                generation.fetch_add(1, Ordering::SeqCst);
                break;
            }

            std::thread::sleep(Duration::from_millis(interval));
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn stop_monitor(state: State<'_, MonitorState>) -> Result<()> {
    // Incrementing invalidates every running thread, which exits on its next
    // generation check.
    state.generation.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

/// Processes sorted by CPU usage, then memory.
#[tauri::command]
pub async fn list_processes(limit: usize) -> Result<Vec<ProcessInfo>> {
    let mut system = System::new();
    let users = Users::new_with_refreshed_list();

    // Two passes: the first gives sysinfo a baseline to compare CPU against.
    system.refresh_processes(ProcessesToUpdate::All, true);
    std::thread::sleep(MIN_CPU_INTERVAL);
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.refresh_cpu_all();

    let core_count = system.cpus().len().max(1) as f32;

    let mut processes: Vec<ProcessInfo> = system
        .processes()
        .values()
        .map(|process| ProcessInfo {
            pid: process.pid().as_u32(),
            name: process.name().to_string_lossy().into_owned(),
            cpu_usage: (process.cpu_usage() / 100.0 / core_count).clamp(0.0, 1.0),
            memory_bytes: process.memory(),
            user: process
                .user_id()
                .and_then(|uid| users.get_user_by_id(uid))
                .map(|user| user.name().to_string()),
            command: process
                .exe()
                .map(|exe| exe.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
        .collect();

    processes.sort_by(|a, b| {
        b.cpu_usage
            .total_cmp(&a.cpu_usage)
            .then(b.memory_bytes.cmp(&a.memory_bytes))
    });
    processes.truncate(limit);

    Ok(processes)
}

/// List top memory-consuming processes. Unlike [`list_processes`], this does NOT
/// sleep 200ms for CPU baseline — it only needs a single refresh for memory data.
#[tauri::command]
pub async fn list_top_memory_processes(limit: usize) -> Result<Vec<ProcessInfo>> {
    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let mut procs: Vec<ProcessInfo> = system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessInfo {
            pid: pid.as_u32(),
            name: process.name().to_string_lossy().into_owned(),
            cpu_usage: 0.0, // Not computed — no baseline
            memory_bytes: process.memory(),
            user: None,
            command: process
                .exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
        .collect();

    // Sort by memory descending
    procs.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));
    procs.truncate(limit);
    Ok(procs)
}

/// Ask a process to quit, or kill it outright when `force` is set.
#[tauri::command]
pub async fn kill_process(pid: u32, force: bool) -> Result<()> {
    if pid <= 1 {
        return Err(WindleError::Protected(format!("pid {pid}")));
    }

    let signal = if force { "-KILL" } else { "-TERM" };
    super::run_tool("kill", &[signal, &pid.to_string()])
        .map(|_| ())
        .map_err(|error| match error {
            WindleError::Command { message, .. }
                if message.contains("not permitted") || message.contains("Operation not") =>
            {
                WindleError::NeedsElevation(format!("killing pid {pid}"))
            }
            other => other,
        })
}

/// Read every metric with a freshly built sampler. Used by the dashboard, which
/// has no long-lived stream of its own.
pub fn sample_system() -> SystemSnapshot {
    Sampler::new().sample()
}

fn host_info(system: &System) -> HostInfo {
    HostInfo {
        hostname: System::host_name(),
        os_version: System::long_os_version().or_else(System::os_version),
        kernel_version: System::kernel_version(),
        cpu_brand: system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_string())
            .filter(|brand| !brand.is_empty()),
        physical_cores: system.physical_core_count(),
    }
}

/// Highest thermal sensor reading, when the platform exposes any. Apple Silicon
/// keeps its sensors behind a private IOKit interface, so this is usually
/// `None` there and populated on Intel Macs.
fn hottest_component(components: &Components) -> Option<f32> {
    components
        .iter()
        .filter_map(|component| {
            let value = component.temperature();
            (value.is_finite() && value > 0.0).then_some(value)
        })
        .max_by(f32::total_cmp)
}

/// Scale a counter delta to a per-second rate.
fn per_second(delta: u64, seconds: f64) -> u64 {
    (delta as f64 / seconds).round().max(0.0) as u64
}

// ---------------------------------------------------------------------------
// SMC (System Management Controller) fan speed reading via IOKit FFI.
// ---------------------------------------------------------------------------

/// The default IOKit master port — 0 on modern macOS.
#[cfg(target_os = "macos")]
const K_IOMASTER_PORT_DEFAULT: u32 = 0;

/// `IOConnectCallStructMethod` selector for the AppleSMC user client.
#[cfg(target_os = "macos")]
const KERNEL_INDEX_SMC: u32 = 2;

/// SMC command: get key info (data type + size).
#[cfg(target_os = "macos")]
const SMC_CMD_READ_KEYINFO: u8 = 9;

/// SMC command: read key bytes.
#[cfg(target_os = "macos")]
const SMC_CMD_READ_BYTES: u8 = 5;

/// SMC key "F0Ac" — fan 0 actual RPM, encoded as a big-endian `OSType` (u32).
#[cfg(target_os = "macos")]
const SMC_KEY_F0AC: u32 =
    (b'F' as u32) << 24 | (b'0' as u32) << 16 | (b'A' as u32) << 8 | (b'c' as u32);

/// SMC data type "fpe2" — fixed-point, 2 decimal places.
#[cfg(target_os = "macos")]
const TYPE_FPE2: u32 =
    (b'f' as u32) << 24 | (b'p' as u32) << 16 | (b'e' as u32) << 8 | (b'2' as u32);

/// SMC data type "flt " — 32-bit IEEE float.
#[cfg(target_os = "macos")]
const TYPE_FLT: u32 =
    (b'f' as u32) << 24 | (b'l' as u32) << 16 | (b't' as u32) << 8 | (b' ' as u32);

/// Mirrors the `SMCParamStruct` expected by the AppleSMC IOKit user client.
/// Must be exactly 80 bytes to match the kernel's expected layout.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcParamStruct {
    key: u32,            // offset  0 (4 bytes)
    vers: [u8; 6],      // offset  4 (6 bytes, SMCVersion)
    _pad1: [u8; 2],     // offset 10 (2 bytes padding for alignment)
    p_limit: [u8; 16],   // offset 12 (16 bytes, SMCPLimitData)
    data_size: u32,      // offset 28 (4 bytes, keyInfo.dataSize)
    data_type: u32,      // offset 32 (4 bytes, keyInfo.dataType)
    data_attrs: u8,      // offset 36 (1 byte, keyInfo.dataAttributes)
    _pad2: [u8; 3],     // offset 37 (3 bytes padding)
    result: u8,          // offset 40 (1 byte)
    status: u8,          // offset 41 (1 byte)
    data8: u8,           // offset 42 (1 byte) — THE SMC COMMAND BYTE
    _pad3: [u8; 1],     // offset 43 (1 byte padding for u32 alignment)
    data32: u32,         // offset 44 (4 bytes)
    bytes: [u8; 32],     // offset 48 (32 bytes)
}                       // Total: 80 bytes

/// Compile-time check that `SmcParamStruct` matches the kernel's 80-byte layout.
#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<SmcParamStruct>() == 80);

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingService(master_port: u32, matching: *mut std::ffi::c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, type_: u32, connect: *mut u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input_structure: *const SmcParamStruct,
        input_structure_cnt: usize,
        output_structure: *mut SmcParamStruct,
        output_structure_cnt: *mut usize,
    ) -> i32;
}

#[cfg(target_os = "macos")]
extern "C" {
    static mach_task_self_: u32;
}

/// Read fan 0 RPM via the AppleSMC IOKit user client. Returns `None` when the
/// service is unavailable (Apple Silicon laptops, Mac mini, etc.) or the
/// reading is implausible.
#[cfg(target_os = "macos")]
fn read_fan_speed_rpm() -> Option<f32> {
    unsafe {
        // 1. Find the AppleSMC service.
        let name = std::ffi::CString::new("AppleSMC").ok()?;
        let matching = IOServiceMatching(name.as_ptr());
        if matching.is_null() {
            eprintln!("[Windle SMC] IOServiceMatching returned null");
            return None;
        }
        let service = IOServiceGetMatchingService(K_IOMASTER_PORT_DEFAULT, matching);
        if service == 0 {
            eprintln!("[Windle SMC] IOServiceGetMatchingService returned 0");
            return None;
        }

        // 2. Open a user-client connection.
        let mut conn: u32 = 0;
        let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
        IOObjectRelease(service); // release the service regardless of open result
        if kr != 0 {
            eprintln!("[Windle SMC] IOServiceOpen failed: kr={}", kr);
            return None;
        }
        if conn == 0 {
            eprintln!("[Windle SMC] IOServiceOpen returned conn=0");
            return None;
        }

        // 3. READ_KEYINFO — get the data type and size for the F0Ac key.
        let mut input = SmcParamStruct::default();
        input.key = SMC_KEY_F0AC;
        input.data8 = SMC_CMD_READ_KEYINFO;

        let mut output = SmcParamStruct::default();
        let mut output_size = std::mem::size_of::<SmcParamStruct>();

        let kr = IOConnectCallStructMethod(
            conn,
            KERNEL_INDEX_SMC,
            &input,
            std::mem::size_of::<SmcParamStruct>(),
            &mut output,
            &mut output_size,
        );

        if kr != 0 {
            eprintln!("[Windle SMC] READ_KEYINFO failed: kr={}", kr);
            IOObjectRelease(conn);
            return None;
        }

        let data_size = output.data_size;
        let data_type = output.data_type;

        // 4. READ_BYTES — read the actual fan speed bytes.
        let mut input = SmcParamStruct::default();
        input.key = SMC_KEY_F0AC;
        input.data8 = SMC_CMD_READ_BYTES;
        input.data_size = data_size;

        let mut output = SmcParamStruct::default();
        let mut output_size = std::mem::size_of::<SmcParamStruct>();

        let kr = IOConnectCallStructMethod(
            conn,
            KERNEL_INDEX_SMC,
            &input,
            std::mem::size_of::<SmcParamStruct>(),
            &mut output,
            &mut output_size,
        );

        if kr != 0 {
            eprintln!("[Windle SMC] READ_BYTES failed: kr={}", kr);
            IOObjectRelease(conn);
            return None;
        }

        if output.result != 0 {
            eprintln!("[Windle SMC] READ_BYTES returned result={}", output.result);
            IOObjectRelease(conn);
            return None;
        }

        // 5. Parse the result based on the SMC data type.
        let rpm = if data_type == TYPE_FPE2 && data_size >= 2 {
            // fpe2: big-endian 16-bit integer, actual RPM = raw / 4.0.
            let raw = ((output.bytes[0] as u16) << 8) | (output.bytes[1] as u16);
            if raw == 0 {
                None
            } else {
                let val = raw as f32 / 4.0;
                if val > 0.0 && val < 50000.0 { Some(val) } else { None }
            }
        } else if data_type == TYPE_FLT && data_size >= 4 {
            // flt: little-endian 32-bit float.
            let raw = f32::from_le_bytes([
                output.bytes[0],
                output.bytes[1],
                output.bytes[2],
                output.bytes[3],
            ]);
            if raw > 0.0 && raw < 50000.0 { Some(raw) } else { None }
        } else {
            eprintln!(
                "[Windle SMC] Unknown data type: 0x{:08X}, size: {}",
                data_type, data_size
            );
            None
        };

        // 6. Cleanup and return.
        IOObjectRelease(conn);
        rpm
    }
}

/// Stub for non-macOS builds.
#[cfg(not(target_os = "macos"))]
fn read_fan_speed_rpm() -> Option<f32> {
    None
}

/// Battery level and charge state from `pmset`, enriched with the cycle count
/// and health that only `ioreg` reports. Returns `None` on a desktop Mac.
fn read_battery() -> Option<BatteryStats> {
    let output = super::run_tool("pmset", &["-g", "batt"]).ok()?;
    let line = output.lines().find(|line| line.contains('%'))?;

    let level = line
        .split(';')
        .next()
        .and_then(|part| part.rsplit('\t').next())
        .and_then(|part| part.trim().strip_suffix('%'))
        .and_then(|percent| percent.trim().parse::<f32>().ok())
        .or_else(|| {
            // Fall back to scanning for the first `NN%` token.
            line.split_whitespace()
                .find_map(|token| token.trim_end_matches(';').strip_suffix('%'))
                .and_then(|percent| percent.parse::<f32>().ok())
        })?;

    let lowered = line.to_lowercase();
    let is_charging = lowered.contains("charging") && !lowered.contains("discharging");

    let time_remaining_minutes = line
        .split(';')
        .find(|part| part.contains(':'))
        .and_then(parse_remaining_minutes);

    let (cycle_count, health_percent) = read_battery_health();

    Some(BatteryStats {
        level: (level / 100.0).clamp(0.0, 1.0),
        is_charging,
        cycle_count,
        health_percent,
        time_remaining_minutes,
    })
}

/// `pmset` reports the estimate as `H:MM`; `0:00` means "still calculating".
fn parse_remaining_minutes(part: &str) -> Option<u32> {
    let clock = part.split_whitespace().find(|token| token.contains(':'))?;
    let (hours, minutes) = clock.split_once(':')?;
    let total = hours.trim().parse::<u32>().ok()? * 60 + minutes.trim().parse::<u32>().ok()?;

    (total > 0).then_some(total)
}

/// Cycle count and health from the SMC's IORegistry entry.
fn read_battery_health() -> (Option<u32>, Option<f32>) {
    let Ok(output) = super::run_tool("ioreg", &["-rc", "AppleSmartBattery"]) else {
        return (None, None);
    };

    let cycle_count = ioreg_number(&output, "CycleCount").map(|value| value as u32);
    let design = ioreg_number(&output, "DesignCapacity");
    let current = ioreg_number(&output, "AppleRawMaxCapacity")
        .or_else(|| ioreg_number(&output, "NominalChargeCapacity"))
        .or_else(|| ioreg_number(&output, "MaxCapacity"));

    let health = match (current, design) {
        (Some(current), Some(design)) if design > 0 => {
            Some((current as f32 / design as f32 * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    };

    (cycle_count, health)
}

/// Pull `"Key" = 123` out of `ioreg` output.
fn ioreg_number(output: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");

    output.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with(&needle) {
            return None;
        }
        line.split('=').nth(1)?.trim().parse::<i64>().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_scale_to_one_second() {
        assert_eq!(per_second(1_000, 1.0), 1_000);
        assert_eq!(per_second(1_000, 2.0), 500);
        assert_eq!(per_second(0, 0.5), 0);
    }

    #[test]
    fn parses_ioreg_numbers() {
        let output = "    \"CycleCount\" = 142\n    \"DesignCapacity\" = 4790\n";

        assert_eq!(ioreg_number(output, "CycleCount"), Some(142));
        assert_eq!(ioreg_number(output, "DesignCapacity"), Some(4790));
        assert_eq!(ioreg_number(output, "Missing"), None);
    }

    #[test]
    fn parses_the_pmset_time_estimate() {
        assert_eq!(parse_remaining_minutes(" 3:25 remaining present: true"), Some(205));
        // Still calculating.
        assert_eq!(parse_remaining_minutes(" 0:00 remaining present: true"), None);
    }

    #[test]
    fn a_snapshot_reports_plausible_values() {
        let snapshot = sample_system();

        assert!(snapshot.memory.total_bytes > 0, "memory must be readable");
        assert!(!snapshot.cpu.per_core.is_empty(), "cpus must be readable");
        assert!((0.0..=1.0).contains(&snapshot.cpu.usage));
        assert!((0.0..=1.0).contains(&snapshot.memory.pressure));
        assert!(snapshot.timestamp > 0);
    }
}

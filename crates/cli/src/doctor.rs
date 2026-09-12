//! `harness doctor`: everything a bug report needs, collected without
//! requiring a running daemon (but using one when it is there).

use std::path::{Path, PathBuf};
use std::time::Duration;

use apprentice_api::methods::{AuthStatus, DaemonStatus, Empty};
use apprentice_client::{ClientError, ClientOptions, DaemonClient, DaemonInfo};
use apprentice_common::paths::{HOME_ENV, Paths};
use apprentice_common::telemetry::Handle;
use serde::Serialize;
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Time allowed for the daemon to answer during `doctor`.
const DAEMON_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize)]
pub struct Report {
    pub harness: Versions,
    pub system: SystemInfo,
    pub paths: PathsInfo,
    pub daemon: DaemonReport,
    pub auth: AuthReport,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Versions {
    pub cli: &'static str,
    pub client: &'static str,
    pub api_version: u32,
}

#[derive(Debug, Serialize)]
pub struct SystemInfo {
    pub os: String,
    pub os_version: Option<String>,
    pub kernel: Option<String>,
    pub arch: String,
    pub cpu: String,
    pub cores_physical: Option<usize>,
    pub cores_logical: usize,
    pub memory_total_mb: u64,
    pub memory_available_mb: u64,
    /// From `nvidia-smi` when present; empty means none or unknown.
    pub gpus: Vec<Gpu>,
    pub gpu_probe: String,
}

#[derive(Debug, Serialize)]
pub struct Gpu {
    pub name: String,
    pub memory_mb: Option<u64>,
    pub driver: String,
}

#[derive(Debug, Serialize)]
pub struct PathsInfo {
    pub home_env: Option<String>,
    pub config_file: PathBuf,
    pub config_file_exists: bool,
    pub data_dir: PathBuf,
    pub data_dir_exists: bool,
    pub data_dir_bytes: u64,
    pub log_dir: PathBuf,
    pub log_files: usize,
    pub cli_log_file: Option<PathBuf>,
    pub disk_free_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DaemonReport {
    pub info_file: PathBuf,
    /// `not_running` | `running` | `stale` | `unreadable`
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid_alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthReport {
    /// `daemon` | `env` | `unknown`
    pub checked_via: &'static str,
    pub providers: Vec<ProviderReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderReport {
    pub name: String,
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

pub fn run(paths: &Paths, log: Option<&Handle>, json: bool) -> anyhow::Result<()> {
    let report = collect(paths, log);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

pub fn collect(paths: &Paths, log: Option<&Handle>) -> Report {
    let mut warnings = Vec::new();
    let system = system_info();
    let paths_info = paths_info(paths, log);
    let (daemon, auth_from_daemon) = daemon_report(paths, &mut warnings);
    let auth = auth_from_daemon.unwrap_or_else(|| auth_without_daemon(daemon.state));

    if !paths_info.config_file_exists {
        warnings.push("no user config file yet (defaults apply)".into());
    }
    if system.gpus.is_empty() {
        warnings.push(format!("no GPU detected ({})", system.gpu_probe));
    }
    if let Some(free) = paths_info.disk_free_bytes
        && free < 2 * 1024 * 1024 * 1024
    {
        warnings.push(format!("low disk space: {} free", human_bytes(free)));
    }

    Report {
        harness: Versions {
            cli: apprentice_client::VERSION,
            client: apprentice_client::VERSION,
            api_version: apprentice_api::API_VERSION,
        },
        system,
        paths: paths_info,
        daemon,
        auth,
        warnings,
    }
}

fn system_info() -> SystemInfo {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let (gpus, gpu_probe) = probe_gpus();
    SystemInfo {
        os: System::name().unwrap_or_else(|| std::env::consts::OS.to_owned()),
        os_version: System::long_os_version(),
        kernel: System::kernel_version(),
        arch: System::cpu_arch(),
        cpu,
        cores_physical: System::physical_core_count(),
        cores_logical: sys.cpus().len(),
        memory_total_mb: sys.total_memory() / (1024 * 1024),
        memory_available_mb: sys.available_memory() / (1024 * 1024),
        gpus,
        gpu_probe,
    }
}

/// `nvidia-smi` is the only probe for M00; the hardware profile of M02
/// replaces this.
fn probe_gpus() -> (Vec<Gpu>, String) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.total,driver_version",
                "--format=csv,noheader,nounits",
            ])
            .output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let gpus = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let mut parts = l.split(',').map(str::trim);
                    Gpu {
                        name: parts.next().unwrap_or("?").to_owned(),
                        memory_mb: parts.next().and_then(|m| m.parse().ok()),
                        driver: parts.next().unwrap_or("?").to_owned(),
                    }
                })
                .collect();
            (gpus, "nvidia-smi".into())
        }
        Ok(Ok(out)) => (Vec::new(), format!("nvidia-smi exited with {}", out.status)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            (Vec::new(), "nvidia-smi not found".into())
        }
        Ok(Err(e)) => (Vec::new(), format!("nvidia-smi failed: {e}")),
        Err(_) => (Vec::new(), "nvidia-smi timed out".into()),
    }
}

fn paths_info(paths: &Paths, log: Option<&Handle>) -> PathsInfo {
    let config_file = paths.config_file();
    let log_dir = paths.data_dir.join("logs");
    let log_files = std::fs::read_dir(&log_dir).map_or(0, |rd| rd.filter_map(Result::ok).count());
    PathsInfo {
        home_env: std::env::var(HOME_ENV).ok().filter(|s| !s.is_empty()),
        config_file_exists: config_file.is_file(),
        config_file,
        data_dir_exists: paths.data_dir.is_dir(),
        data_dir_bytes: dir_size(&paths.data_dir),
        disk_free_bytes: disk_free(&paths.data_dir),
        data_dir: paths.data_dir.clone(),
        log_dir,
        log_files,
        cli_log_file: log.map(|h| h.log_file.clone()),
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map_or(0, |m| m.len())
            }
        })
        .sum()
}

fn disk_free(dir: &Path) -> Option<u64> {
    let disks = Disks::new_with_refreshed_list();
    let probe = if dir.exists() {
        dir.to_path_buf()
    } else {
        dir.ancestors().find(|a| a.exists())?.to_path_buf()
    };
    let probe = std::fs::canonicalize(&probe).unwrap_or(probe);
    disks
        .list()
        .iter()
        .filter(|d| {
            let mp = std::fs::canonicalize(d.mount_point())
                .unwrap_or_else(|_| d.mount_point().to_path_buf());
            probe.starts_with(mp)
        })
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

fn pid_alive(pid: u32) -> bool {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some()
}

fn daemon_report(paths: &Paths, warnings: &mut Vec<String>) -> (DaemonReport, Option<AuthReport>) {
    let info_file = DaemonInfo::path(&paths.data_dir);
    let mut report = DaemonReport {
        info_file: info_file.clone(),
        state: "not_running",
        pid: None,
        pid_alive: None,
        endpoint: None,
        version: None,
        api_version: None,
        uptime_s: None,
        log_file: None,
        error: None,
    };
    let info = match DaemonInfo::read(&paths.data_dir) {
        Ok(Some(info)) => info,
        Ok(None) => return (report, None),
        Err(e) => {
            report.state = "unreadable";
            report.error = Some(e.to_string());
            warnings.push(format!("{} is unreadable: {e}", info_file.display()));
            return (report, None);
        }
    };
    report.pid = Some(info.pid);
    report.pid_alive = Some(pid_alive(info.pid));
    report.endpoint = Some(info.endpoint.to_string());
    report.version = Some(info.version.clone());
    report.api_version = Some(info.api_version);

    match query_daemon(&info) {
        Ok((status, auth)) => {
            report.state = "running";
            report.version = Some(status.version);
            report.uptime_s = Some(status.uptime_s);
            report.log_file = status.log_file;
            let auth = AuthReport {
                checked_via: "daemon",
                providers: auth
                    .providers
                    .into_iter()
                    .map(|p| ProviderReport {
                        name: p.name,
                        configured: p.configured,
                        source: p.source,
                    })
                    .collect(),
                note: None,
            };
            (report, Some(auth))
        }
        Err(e) => {
            report.state = "stale";
            report.error = Some(e.to_string());
            warnings.push(format!(
                "daemon.json exists (pid {}, alive: {}) but the daemon did not answer: {e}",
                info.pid,
                report.pid_alive.unwrap_or(false)
            ));
            (report, None)
        }
    }
}

fn query_daemon(
    info: &DaemonInfo,
) -> Result<
    (
        apprentice_api::methods::DaemonStatusResult,
        apprentice_api::methods::AuthStatusResult,
    ),
    ClientError,
> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ClientError::Io)?;
    rt.block_on(async {
        let options = ClientOptions {
            timeout: Some(DAEMON_TIMEOUT),
            ..ClientOptions::default()
        };
        let client = tokio::time::timeout(
            DAEMON_TIMEOUT,
            DaemonClient::connect_endpoint(&info.endpoint, options),
        )
        .await
        .map_err(|_| ClientError::Timeout(DAEMON_TIMEOUT))??;
        client
            .hello(
                "harness-doctor",
                apprentice_client::VERSION,
                Some(info.token.clone()),
            )
            .await?;
        let status = client.call::<DaemonStatus>(Empty {}).await?;
        let auth = client.call::<AuthStatus>(Empty {}).await?;
        Ok((status, auth))
    })
}

fn auth_without_daemon(daemon_state: &str) -> AuthReport {
    // Only the environment can be checked without the core; the keychain
    // and file stores are the daemon's business.
    let env_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let configured = env_key.is_some();
    AuthReport {
        checked_via: if configured { "env" } else { "unknown" },
        providers: vec![ProviderReport {
            name: "anthropic".into(),
            configured,
            source: configured.then(|| "env".to_owned()),
        }],
        note: (!configured).then(|| {
            format!(
                "keychain/file stores not checked (daemon {daemon_state}); \
                 run `harness auth status` with the daemon up"
            )
        }),
    }
}

#[allow(clippy::cast_precision_loss)] // display only
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn print_human(r: &Report) {
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    println!("apprentice-harness doctor");
    println!();
    println!("Versions");
    println!("  cli            {}", r.harness.cli);
    println!("  api version    {}", r.harness.api_version);
    println!();
    println!("System");
    println!(
        "  os             {} {}",
        r.system.os,
        r.system.os_version.as_deref().unwrap_or("")
    );
    println!("  kernel         {}", opt(&r.system.kernel));
    println!("  arch           {}", r.system.arch);
    println!(
        "  cpu            {} ({} physical / {} logical)",
        r.system.cpu,
        r.system
            .cores_physical
            .map_or("?".to_owned(), |n| n.to_string()),
        r.system.cores_logical
    );
    println!(
        "  memory         {} MiB total, {} MiB available",
        r.system.memory_total_mb, r.system.memory_available_mb
    );
    if r.system.gpus.is_empty() {
        println!("  gpu            none detected ({})", r.system.gpu_probe);
    }
    for g in &r.system.gpus {
        println!(
            "  gpu            {} {} driver {}",
            g.name,
            g.memory_mb.map_or(String::new(), |m| format!("{m} MiB")),
            g.driver
        );
    }
    println!();
    println!("Paths");
    println!("  HARNESS_HOME   {}", opt(&r.paths.home_env));
    println!(
        "  config file    {}{}",
        r.paths.config_file.display(),
        if r.paths.config_file_exists {
            ""
        } else {
            " (missing)"
        }
    );
    println!(
        "  data dir       {} ({}{})",
        r.paths.data_dir.display(),
        human_bytes(r.paths.data_dir_bytes),
        if r.paths.data_dir_exists {
            ""
        } else {
            ", missing"
        }
    );
    println!(
        "  log dir        {} ({} files)",
        r.paths.log_dir.display(),
        r.paths.log_files
    );
    println!(
        "  disk free      {}",
        r.paths
            .disk_free_bytes
            .map_or("unknown".to_owned(), human_bytes)
    );
    println!();
    println!("Daemon");
    println!("  state          {}", r.daemon.state);
    if let Some(pid) = r.daemon.pid {
        println!(
            "  pid            {} (alive: {})",
            pid,
            r.daemon.pid_alive.unwrap_or(false)
        );
    }
    println!("  endpoint       {}", opt(&r.daemon.endpoint));
    println!("  version        {}", opt(&r.daemon.version));
    if let Some(u) = r.daemon.uptime_s {
        println!("  uptime         {u} s");
    }
    println!("  log file       {}", opt(&r.daemon.log_file));
    if let Some(e) = &r.daemon.error {
        println!("  error          {e}");
    }
    println!();
    println!("Auth (via {})", r.auth.checked_via);
    for p in &r.auth.providers {
        println!(
            "  {:<14} {}",
            p.name,
            if p.configured {
                format!("configured ({})", p.source.as_deref().unwrap_or("?"))
            } else {
                "not configured".to_owned()
            }
        );
    }
    if let Some(n) = &r.auth.note {
        println!("  note           {n}");
    }
    if !r.warnings.is_empty() {
        println!();
        println!("Warnings");
        for w in &r.warnings {
            println!("  - {w}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn report_without_daemon_serialises() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(dir.path());
        let r = collect(&paths, None);
        assert_eq!(r.daemon.state, "not_running");
        assert!(!r.paths.config_file_exists);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["harness"]["api_version"], apprentice_api::API_VERSION);
        assert!(v["system"]["cores_logical"].as_u64().unwrap() >= 1);
    }
}

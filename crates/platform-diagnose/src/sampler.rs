//! Live signal capture from `/proc` and sysfs (Linux).
//!
//! The sampler holds the *previous* cumulative counters and reduces each new
//! reading to per-second rates / percentages, so a [`Sample`] carries dynamics
//! (swap-out rate, per-process CPU, retransmit delta) rather than raw counters.
//!
//! Everything degrades: unreadable files become `0`/`None`, a missing `/proc`
//! entry never panics. Non-Linux returns empty samples (the engine still runs
//! but most rules will find nothing).

use std::collections::HashMap;
use std::time::Instant;

use crate::sample::{
    DiskIoSample, DnsProbe, IoSample, MemSample, NetSample, ProbeResult, ProcSample, Sample,
};

/// Page size used to convert `/proc` RSS pages to bytes. Standard on x86/ARM64.
const PAGE_SIZE: u64 = 4096;
/// Clock ticks per second for `/proc/<pid>/stat` utime/stime (Linux default).
const CLOCK_TICKS: f64 = 100.0;

/// Cumulative state from the previous capture, used to compute deltas.
struct Prev {
    at: Instant,
    cpu_total: u64,
    cpu_idle: u64,
    cpu_iowait: u64,
    cpu_user: u64,
    cpu_system: u64,
    cpu_steal: u64,
    ctx_switches: u64,
    interrupts: u64,
    vmstat: VmstatCounters,
    net: NetCounters,
    procs: HashMap<u32, PrevProc>,
}

#[derive(Default, Clone, Copy)]
struct VmstatCounters {
    pswpin: u64,
    pswpout: u64,
    pgmajfault: u64,
}

#[derive(Default, Clone, Copy)]
struct NetCounters {
    tcp_retrans: u64,
}

#[derive(Default, Clone, Copy)]
struct PrevProc {
    cpu_ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
}

/// Captures [`Sample`]s, tracking counters between calls for rate computation.
pub struct Sampler {
    prev: Option<Prev>,
    /// Optional active-probe settings (network RTT/DNS). `None` = passive only.
    probe: Option<ProbeConfig>,
}

/// Configuration for active network probes.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// `host:port` targets to TCP-connect probe (RTT).
    pub targets: Vec<String>,
    /// Reference hostname to time DNS resolution for.
    pub dns_name: Option<String>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    /// Passive-only sampler (no network probes).
    pub fn new() -> Self {
        Sampler {
            prev: None,
            probe: None,
        }
    }

    /// Sampler that also runs active network probes on each capture.
    pub fn with_probes(config: ProbeConfig) -> Self {
        Sampler {
            prev: None,
            probe: Some(config),
        }
    }

    /// Capture one sample. The first call establishes a baseline (rates are 0
    /// because there is no prior interval); subsequent calls report real rates.
    pub fn sample(&mut self) -> Sample {
        let start = Instant::now();
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let cpu = read_cpu_stat();
        let vmstat = read_vmstat();
        let net_raw = read_net();
        let load = read_loadavg();
        let run_queue = read_run_queue();
        let mem = read_meminfo();
        let procs_raw = read_processes();
        let thermal = read_thermal();
        let io = read_disk_io();

        // Elapsed wall time since the previous capture (for rate scaling).
        let dt = self
            .prev
            .as_ref()
            .map(|p| p.at.elapsed().as_secs_f64().max(1e-6))
            .unwrap_or(0.0);

        let have_prev = self.prev.is_some();
        let mut sample = Sample {
            at_unix: now_unix,
            capture_ms: 0,
            cpu: build_cpu(&cpu, load, run_queue, have_prev, dt, self.prev.as_ref()),
            mem: build_mem(&mem, &vmstat, have_prev, dt, self.prev.as_ref()),
            io,
            net: build_net(&net_raw, have_prev, self.prev.as_ref()),
            thermal,
            procs: build_procs(&procs_raw, have_prev, dt, self.prev.as_ref()),
        };

        // Active network probes (only when configured).
        if let Some(cfg) = &self.probe {
            sample.net.probes = run_probes(&cfg.targets);
            sample.net.dns = cfg.dns_name.as_deref().and_then(run_dns_probe);
        }

        // Store current counters as the baseline for the next delta.
        self.prev = Some(Prev {
            at: start,
            cpu_total: cpu.total,
            cpu_idle: cpu.idle + cpu.iowait,
            cpu_iowait: cpu.iowait,
            cpu_user: cpu.user + cpu.nice,
            cpu_system: cpu.system + cpu.irq + cpu.softirq,
            cpu_steal: cpu.steal,
            ctx_switches: cpu.ctx_switches,
            interrupts: cpu.interrupts,
            vmstat,
            net: NetCounters {
                tcp_retrans: net_raw.tcp_retrans,
            },
            procs: procs_raw
                .iter()
                .map(|p| {
                    (
                        p.pid,
                        PrevProc {
                            cpu_ticks: p.cpu_ticks,
                            read_bytes: p.read_bytes,
                            write_bytes: p.write_bytes,
                        },
                    )
                })
                .collect(),
        });

        sample.capture_ms = start.elapsed().as_millis() as u64;
        sample
    }
}

// ---------------------------------------------------------------------------
// /proc readers (raw cumulative counters)
// ---------------------------------------------------------------------------

struct CpuStat {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
    total: u64,
    ctx_switches: u64,
    interrupts: u64,
}

fn read_cpu_stat() -> CpuStat {
    let mut stat = CpuStat {
        user: 0,
        nice: 0,
        system: 0,
        idle: 0,
        iowait: 0,
        irq: 0,
        softirq: 0,
        steal: 0,
        total: 0,
        ctx_switches: 0,
        interrupts: 0,
    };
    let Ok(text) = std::fs::read_to_string("/proc/stat") else {
        return stat;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let v: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if v.len() >= 8 {
                stat.user = v[0];
                stat.nice = v[1];
                stat.system = v[2];
                stat.idle = v[3];
                stat.iowait = v[4];
                stat.irq = v[5];
                stat.softirq = v[6];
                stat.steal = v[7];
                stat.total = v[..8].iter().sum();
            }
        } else if let Some(rest) = line.strip_prefix("ctxt ") {
            stat.ctx_switches = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("intr ") {
            stat.interrupts = rest
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
        }
    }
    stat
}

fn read_vmstat() -> VmstatCounters {
    let mut c = VmstatCounters::default();
    let Ok(text) = std::fs::read_to_string("/proc/vmstat") else {
        return c;
    };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let v: u64 = val.parse().unwrap_or(0);
        match key {
            "pswpin" => c.pswpin = v,
            "pswpout" => c.pswpout = v,
            "pgmajfault" => c.pgmajfault = v,
            _ => {}
        }
    }
    c
}

struct NetRaw {
    rx_errors: u64,
    tx_errors: u64,
    rx_dropped: u64,
    tx_dropped: u64,
    tcp_retrans: u64,
    sockets: std::collections::BTreeMap<String, u64>,
}

fn read_net() -> NetRaw {
    let mut raw = NetRaw {
        rx_errors: 0,
        tx_errors: 0,
        rx_dropped: 0,
        tx_dropped: 0,
        tcp_retrans: 0,
        sockets: std::collections::BTreeMap::new(),
    };

    // Interface error/drop counters.
    if let Ok(text) = std::fs::read_to_string("/proc/net/dev") {
        for line in text.lines().skip(2) {
            let Some((iface, rest)) = line.split_once(':') else {
                continue;
            };
            let iface = iface.trim();
            if iface == "lo" {
                continue;
            }
            let v: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if v.len() >= 12 {
                raw.rx_errors += v[2];
                raw.rx_dropped += v[3];
                raw.tx_errors += v[10];
                raw.tx_dropped += v[11];
            }
        }
    }

    // TCP retransmit segment count (from /proc/net/snmp).
    raw.tcp_retrans = read_tcp_retransmits();

    // Socket state counts.
    for (path, proto) in [("/proc/net/tcp", "tcp"), ("/proc/net/tcp6", "tcp6")] {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > 3 {
                    let state = fields[3].to_uppercase();
                    let name = socket_state_name(&state);
                    *raw.sockets.entry(format!("{proto}.{name}")).or_insert(0) += 1;
                }
            }
        }
    }
    raw
}

fn read_tcp_retransmits() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/net/snmp") else {
        return 0;
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut header_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("Tcp:") && line.split_whitespace().any(|t| t == "RetransSegs") {
            header_idx = Some(i);
            break;
        }
    }
    let Some(h) = header_idx else { return 0 };
    let cols: Vec<&str> = lines[h].split_whitespace().collect();
    let Some(pos) = cols.iter().position(|c| *c == "RetransSegs") else {
        return 0;
    };
    if let Some(data) = lines.get(h + 1) {
        let vals: Vec<&str> = data.split_whitespace().collect();
        if let Some(v) = vals.get(pos) {
            return v.parse().unwrap_or(0);
        }
    }
    0
}

fn socket_state_name(hex: &str) -> String {
    match hex {
        "01" => "estab",
        "02" => "syn_sent",
        "03" => "syn_recv",
        "04" => "fin_wait1",
        "05" => "fin_wait2",
        "06" => "time_wait",
        "07" => "close",
        "08" => "close_wait",
        "09" => "last_ack",
        "0A" => "listen",
        "0B" => "closing",
        _ => "other",
    }
    .to_string()
}

struct MemRaw {
    total_kb: u64,
    free_kb: u64,
    available_kb: u64,
    cached_kb: u64,
    swap_total_kb: u64,
    swap_free_kb: u64,
}

fn read_meminfo() -> MemRaw {
    let mut m = MemRaw {
        total_kb: 0,
        free_kb: 0,
        available_kb: 0,
        cached_kb: 0,
        swap_total_kb: 0,
        swap_free_kb: 0,
    };
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return m;
    };
    for line in text.lines() {
        let mut it = line.split(':');
        let (Some(key), Some(rest)) = (it.next(), it.next()) else {
            continue;
        };
        let val: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => m.total_kb = val,
            "MemFree" => m.free_kb = val,
            "MemAvailable" => m.available_kb = val,
            "Cached" => m.cached_kb = val,
            "SwapTotal" => m.swap_total_kb = val,
            "SwapFree" => m.swap_free_kb = val,
            _ => {}
        }
    }
    m
}

fn read_loadavg() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

/// Runnable tasks currently on the run queue (loadavg 4th field numerator).
fn read_run_queue() -> u64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .nth(3)
                .and_then(|f| f.split('/').next())
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

struct RawProc {
    pid: u32,
    name: String,
    state: String,
    cpu_ticks: u64,
    threads: u32,
    rss_pages: u64,
    read_bytes: u64,
    write_bytes: u64,
    fds: Option<u32>,
}

fn read_processes() -> Vec<RawProc> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let base = entry.path();
        let Ok(stat) = std::fs::read_to_string(base.join("stat")) else {
            continue;
        };
        let Some((comm, rest)) = parse_stat(&stat) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 22 {
            continue;
        }
        let state = fields[0].to_string();
        let utime: u64 = fields[11].parse().unwrap_or(0);
        let stime: u64 = fields[12].parse().unwrap_or(0);
        let threads: u32 = fields[17].parse().unwrap_or(0);
        let rss_pages: u64 = fields[21].parse().unwrap_or(0);

        let (read_bytes, write_bytes) = read_proc_io(&base);
        let fds = read_fd_count(&base);

        out.push(RawProc {
            pid,
            name: comm,
            state,
            cpu_ticks: utime + stime,
            threads,
            rss_pages,
            read_bytes,
            write_bytes,
            fds,
        });
    }
    out
}

/// Split `pid (comm) state ...` robustly (comm may contain spaces/parens).
fn parse_stat(line: &str) -> Option<(String, &str)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let pid = line[..open].trim();
    let comm = line[open + 1..close].to_string();
    let rest = &line[close + 1..];
    let _ = pid;
    Some((comm, rest))
}

fn read_proc_io(base: &std::path::Path) -> (u64, u64) {
    let Ok(text) = std::fs::read_to_string(base.join("io")) else {
        return (0, 0);
    };
    let mut read = 0;
    let mut write = 0;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("read_bytes:") {
            read = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("write_bytes:") {
            write = v.trim().parse().unwrap_or(0);
        }
    }
    (read, write)
}

fn read_fd_count(base: &std::path::Path) -> Option<u32> {
    let entries = std::fs::read_dir(base.join("fd")).ok()?;
    Some(entries.flatten().count() as u32)
}

fn read_thermal() -> crate::sample::ThrottleSample {
    let mut t = crate::sample::ThrottleSample {
        max_temp_c: None,
        hottest_label: None,
        throttled: false,
        freq_mhz: None,
        max_freq_mhz: None,
    };

    // Temperatures from hwmon.
    if let Ok(dirs) = std::fs::read_dir("/sys/class/hwmon") {
        for entry in dirs.flatten() {
            let d = entry.path();
            let chip = std::fs::read_to_string(d.join("name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if let Ok(files) = std::fs::read_dir(&d) {
                for f in files.flatten() {
                    let fname = f.file_name().to_string_lossy().to_string();
                    if !fname.starts_with("temp") || !fname.ends_with("_input") {
                        continue;
                    }
                    let idx = fname.trim_start_matches("temp").trim_end_matches("_input");
                    let millideg: f32 = std::fs::read_to_string(f.path())
                        .ok()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0.0);
                    let deg = millideg / 1000.0;
                    if deg <= 0.0 || deg > 150.0 {
                        continue;
                    }
                    if t.max_temp_c.map(|m| deg > m).unwrap_or(true) {
                        t.max_temp_c = Some(deg);
                        let label = std::fs::read_to_string(d.join(format!("temp{idx}_label")))
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|_| format!("temp{idx}"));
                        t.hottest_label = Some(format!("{chip} {label}").trim().to_string());
                    }
                }
            }
        }
    }

    // CPU frequency + throttle detection.
    let cur_avg = avg_cpu_freq("/sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq");
    let max_freq = avg_cpu_freq("/sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq");
    t.freq_mhz = cur_avg.map(|k| k / 1000.0); // kHz -> MHz
    t.max_freq_mhz = max_freq.map(|k| k / 1000.0);
    if let (Some(cur), Some(max)) = (cur_avg, max_freq) {
        // Running well below max while not idle suggests thermal/power limiting.
        t.throttled = max > 0.0 && cur < max * 0.85;
    }
    t
}

/// Average a glob of CPU freq files (kHz). The glob is expanded manually for
/// cpu0..cpuN; returns the mean of readable values.
fn avg_cpu_freq(pattern: &str) -> Option<f64> {
    let prefix = pattern.split('*').next().unwrap_or("");
    let suffix = pattern.split('*').nth(1).unwrap_or("");
    let cpu_root = std::path::Path::new("/sys/devices/system/cpu");
    let mut sum = 0.0;
    let mut n = 0.0;
    for i in 0..256 {
        let path = format!("{prefix}{i}{suffix}");
        if let Ok(v) = std::fs::read_to_string(&path)
            && let Ok(khz) = v.trim().parse::<f64>()
        {
            sum += khz;
            n += 1.0;
        }
    }
    let _ = cpu_root;
    if n > 0.0 { Some(sum / n) } else { None }
}

fn read_disk_io() -> IoSample {
    let mut io = IoSample::default();
    let Ok(text) = std::fs::read_to_string("/proc/diskstats") else {
        return io;
    };
    // diskstats: major minor name reads ... sectors_read ... writes sectors_written ...
    // Field indices (0-based after splitting): 0 major,1 minor,2 name,
    // 3 reads_completed, 5 sectors_read, 7 ms_reading, 9 writes_completed,
    // 11 sectors_written, 13 ms_writing, 14 ios_in_progress, 15 ms_doing_io.
    // We compute util% and await from the deltas the *sampler* would need;
    // here we capture instantaneous queue + cumulative fields are handled by
    // taking a single reading (util/await need prev; left 0 for Phase 1 core
    // and refined when the sampler tracks diskstats deltas).
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let name = f[2].to_string();
        // Only whole disks (skip partitions: names ending in a digit that are
        // partitions are hard to detect portably; keep names without a partition
        // suffix heuristic — nvme0n1, sda, vda, mmcblk0).
        if is_partition(&name) {
            continue;
        }
        let queue: f32 = f.get(14).and_then(|q| q.parse().ok()).unwrap_or(0.0);
        io.disks.push(DiskIoSample {
            device: name,
            read_bytes_per_sec: 0.0,
            write_bytes_per_sec: 0.0,
            await_ms: 0.0,
            util_percent: 0.0,
            queue_depth: queue,
        });
    }
    io
}

fn is_partition(name: &str) -> bool {
    // sda1, vda2, nvme0n1p1, mmcblk0p1 → partitions; sda, vda, nvme0n1, mmcblk0 → disks.
    if let Some(rest) = name.strip_prefix("nvme") {
        return rest.contains('p')
            && rest
                .chars()
                .last()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
    }
    if name.starts_with("mmcblk") {
        return name.contains('p')
            && name
                .chars()
                .last()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
    }
    // sdX / vdX / hdX: partition if it ends with digits after letters.
    let digits = name.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Builders: turn cumulative prev + current into a rates-bearing Sample
// ---------------------------------------------------------------------------

fn build_cpu(
    cur: &CpuStat,
    load: f64,
    run_queue: u64,
    have_prev: bool,
    dt: f64,
    prev: Option<&Prev>,
) -> crate::sample::CpuSample {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mut s = crate::sample::CpuSample {
        logical_cores: cores,
        load_one: load,
        run_queue,
        ..Default::default()
    };
    if !have_prev {
        return s;
    }
    let p = prev.unwrap();
    let d_total = cur.total.saturating_sub(p.cpu_total) as f64;
    if d_total <= 0.0 {
        return s;
    }
    let d_user = (cur.user + cur.nice).saturating_sub(p.cpu_user) as f64;
    let d_system = (cur.system + cur.irq + cur.softirq).saturating_sub(p.cpu_system) as f64;
    let d_idle = (cur.idle + cur.iowait).saturating_sub(p.cpu_idle) as f64;
    let d_iowait = cur.iowait.saturating_sub(p.cpu_iowait) as f64;
    let d_steal = cur.steal.saturating_sub(p.cpu_steal) as f64;

    s.usage_percent = ((d_total - d_idle) / d_total * 100.0).clamp(0.0, 100.0) as f32;
    s.user_percent = (d_user / d_total * 100.0) as f32;
    s.system_percent = (d_system / d_total * 100.0) as f32;
    s.iowait_percent = (d_iowait / d_total * 100.0) as f32;
    s.steal_percent = (d_steal / d_total * 100.0) as f32;
    s.idle_percent = (d_idle / d_total * 100.0) as f32;
    s.ctx_switches_per_sec = rate(cur.ctx_switches, p.ctx_switches, dt);
    s.interrupts_per_sec = rate(cur.interrupts, p.interrupts, dt);
    s
}

fn rate(cur: u64, prev: u64, dt: f64) -> f64 {
    if dt <= 0.0 {
        return 0.0;
    }
    cur.saturating_sub(prev) as f64 / dt
}

fn build_mem(
    m: &MemRaw,
    v: &VmstatCounters,
    have_prev: bool,
    dt: f64,
    prev: Option<&Prev>,
) -> MemSample {
    let total = m.total_kb * 1024;
    let available = m.available_kb * 1024;
    let mut s = MemSample {
        total_bytes: total,
        available_bytes: available,
        available_percent: if total > 0 {
            (available as f64 / total as f64 * 100.0) as f32
        } else {
            0.0
        },
        free_bytes: m.free_kb * 1024,
        swap_total_bytes: m.swap_total_kb * 1024,
        swap_used_bytes: (m.swap_total_kb.saturating_sub(m.swap_free_kb)) * 1024,
        cached_bytes: m.cached_kb * 1024,
        ..Default::default()
    };
    if have_prev {
        let pv = &prev.unwrap().vmstat;
        s.swap_out_pages_per_sec = rate(v.pswpout, pv.pswpout, dt);
        s.swap_in_pages_per_sec = rate(v.pswpin, pv.pswpin, dt);
        s.major_faults_per_sec = rate(v.pgmajfault, pv.pgmajfault, dt);
    }
    s
}

fn build_net(cur: &NetRaw, have_prev: bool, prev: Option<&Prev>) -> NetSample {
    let mut s = NetSample {
        rx_errors: cur.rx_errors,
        tx_errors: cur.tx_errors,
        rx_dropped: cur.rx_dropped,
        tx_dropped: cur.tx_dropped,
        sockets: cur.sockets.clone(),
        ..Default::default()
    };
    if have_prev {
        s.tcp_retransmits = cur
            .tcp_retrans
            .saturating_sub(prev.unwrap().net.tcp_retrans);
    } else {
        s.tcp_retransmits = 0;
    }
    s
}

fn build_procs(raw: &[RawProc], have_prev: bool, dt: f64, prev: Option<&Prev>) -> Vec<ProcSample> {
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        let mut cpu_percent = 0.0;
        let mut rps = 0.0;
        let mut wps = 0.0;
        if have_prev && let Some(pp) = prev.unwrap().procs.get(&p.pid) {
            let ticks = p.cpu_ticks.saturating_sub(pp.cpu_ticks) as f64;
            cpu_percent = (ticks / (CLOCK_TICKS * dt) * 100.0).clamp(0.0, 100.0) as f32;
            rps = rate(p.read_bytes, pp.read_bytes, dt);
            wps = rate(p.write_bytes, pp.write_bytes, dt);
        }
        out.push(ProcSample {
            pid: p.pid,
            name: p.name.clone(),
            state: p.state.clone(),
            cpu_percent,
            memory_bytes: p.rss_pages * PAGE_SIZE,
            read_bytes_per_sec: rps,
            write_bytes_per_sec: wps,
            threads: p.threads,
            fds: p.fds,
            connections: None,
            user: None,
        });
    }
    // Keep the busiest by CPU first; ties broken by memory.
    out.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
    });
    out
}

// ---------------------------------------------------------------------------
// Active probes (network) — std-only, no root, no extra deps
// ---------------------------------------------------------------------------

fn run_probes(targets: &[String]) -> Vec<ProbeResult> {
    targets
        .iter()
        .map(|t| {
            let start = Instant::now();
            let reachable = tcp_connect_probe(t);
            let rtt = if reachable {
                Some(start.elapsed().as_secs_f64() * 1000.0)
            } else {
                None
            };
            ProbeResult {
                label: label_for_target(t),
                target: t.clone(),
                rtt_ms: rtt,
                reachable,
            }
        })
        .collect()
}

fn label_for_target(t: &str) -> String {
    if t.starts_with("1.1.1.1") || t.starts_with("8.8.8.8") {
        "internet".to_string()
    } else if t.starts_with("192.168.") || t.starts_with("10.") || t.starts_with("172.") {
        "gateway_or_lan".to_string()
    } else {
        "target".to_string()
    }
}

/// Attempt a TCP connect with a short timeout; success = reachable.
fn tcp_connect_probe(target: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let Ok(mut addrs) = target.to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).is_ok()
}

fn run_dns_probe(name: &str) -> Option<DnsProbe> {
    use std::net::ToSocketAddrs;
    use std::time::Instant;
    let start = Instant::now();
    let resolved = format!("{name}:80")
        .to_socket_addrs()
        .map(|a| a.len())
        .unwrap_or(0)
        > 0;
    Some(DnsProbe {
        name: name.to_string(),
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        resolved,
    })
}

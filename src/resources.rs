//! Low-frequency, unelevated resource sampling for Echo and its child helpers.
use slint::ComponentHandle;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub struct Monitor(std::sync::mpsc::Sender<()>);
impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub fn install(ui: &crate::AppWindow) -> Monitor {
    let weak = ui.as_weak();
    let (stop, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut sampler = platform::Sampler::new();
        loop {
            let stats = sampler.sample();
            if weak
                .upgrade_in_event_loop(move |ui| {
                    ui.set_cpu_usage_text(stats.cpu.into());
                    ui.set_gpu_usage_text(stats.gpu.into());
                    ui.set_ram_usage_text(stats.ram.into());
                    ui.set_vram_usage_text(stats.vram.into());
                })
                .is_err()
            {
                break;
            }
            if receiver.recv_timeout(Duration::from_secs(1))
                != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                break;
            }
        }
    });
    Monitor(stop)
}

#[derive(Default)]
struct Stats {
    cpu: String,
    gpu: String,
    ram: String,
    vram: String,
}
fn percent(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite())
        .map(|v| format!("{:.1}%", v.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "—".into())
}
fn memory(value: Option<u64>) -> String {
    match value {
        Some(bytes) if bytes >= 1 << 30 => format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64),
        Some(bytes) => format!("{:.0} MiB", bytes as f64 / (1u64 << 20) as f64),
        None => "—".into(),
    }
}
fn owned_processes(root: u32, parents: &[(u32, u32)]) -> HashSet<u32> {
    let mut owned = HashSet::from([root]);
    loop {
        let before = owned.len();
        for &(pid, parent) in parents {
            if owned.contains(&parent) {
                owned.insert(pid);
            }
        }
        if owned.len() == before {
            return owned;
        }
    }
}
fn instance(name: &str) -> Option<(u32, &str)> {
    let (pid, engine) = name.strip_prefix("pid_")?.split_once('_')?;
    Some((pid.parse().ok()?, engine))
}
fn busiest_engine(rows: &[(String, f64)], owned: &HashSet<u32>) -> f64 {
    let mut engines = HashMap::<&str, f64>::new();
    for (name, value) in rows {
        if let Some((pid, engine)) = instance(name) {
            if owned.contains(&pid) {
                *engines.entry(engine).or_default() += value;
            }
        }
    }
    engines.values().copied().fold(0.0, f64::max)
}
fn cpu_delta(
    previous: &HashMap<(u32, u64), u64>,
    current: &HashMap<(u32, u64), u64>,
    elapsed: f64,
    cores: usize,
) -> f64 {
    let ticks: u64 = current
        .iter()
        .filter_map(|(id, value)| previous.get(id).map(|old| value.saturating_sub(*old)))
        .sum();
    ticks as f64 / 10_000_000.0 / elapsed / cores.max(1) as f64 * 100.0
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::{CloseHandle, FILETIME, HANDLE},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
                Performance::*,
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2},
                Threading::{
                    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                    PROCESS_VM_READ,
                },
            },
        },
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    pub struct Sampler {
        query: isize,
        gpu: isize,
        vram: isize,
        previous: HashMap<(u32, u64), u64>,
        last: Instant,
    }
    impl Drop for Sampler {
        fn drop(&mut self) {
            if self.query != 0 {
                unsafe {
                    PdhCloseQuery(self.query);
                }
            }
        }
    }
    impl Sampler {
        pub fn new() -> Self {
            let mut s = Self {
                query: 0,
                gpu: 0,
                vram: 0,
                previous: HashMap::new(),
                last: Instant::now(),
            };
            unsafe {
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut s.query) == 0 {
                    if PdhAddEnglishCounterW(
                        s.query,
                        w!("\\GPU Engine(*)\\Utilization Percentage"),
                        0,
                        &mut s.gpu,
                    ) != 0
                    {
                        s.gpu = 0;
                    }
                    if PdhAddEnglishCounterW(
                        s.query,
                        w!("\\GPU Process Memory(*)\\Dedicated Usage"),
                        0,
                        &mut s.vram,
                    ) != 0
                    {
                        s.vram = 0;
                    }
                    PdhCollectQueryData(s.query);
                }
            }
            s
        }
        pub fn sample(&mut self) -> Stats {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last).as_secs_f64();
            let Some(parents) = process_tree() else {
                return Stats {
                    cpu: percent(None),
                    gpu: percent(None),
                    ram: memory(None),
                    vram: memory(None),
                };
            };
            let owned = owned_processes(std::process::id(), &parents);
            let mut current = HashMap::new();
            let mut ram = Some(0u64);
            for &pid in &owned {
                match process_stats(pid) {
                    Some((creation, ticks, bytes)) => {
                        current.insert((pid, creation), ticks);
                        ram = ram
                            .zip(bytes)
                            .map(|(total, bytes)| total.saturating_add(bytes));
                    }
                    None => {
                        ram = None;
                    }
                }
            }
            let cpu = if self.previous.is_empty() || current.len() != owned.len() {
                None
            } else {
                Some(cpu_delta(
                    &self.previous,
                    &current,
                    elapsed,
                    num_cpus::get(),
                ))
            };
            self.previous = current;
            self.last = now;
            let collected = self.query != 0 && unsafe { PdhCollectQueryData(self.query) } == 0;
            let gpu = collected
                .then(|| counter(self.gpu, &owned))
                .flatten()
                .map(|rows| busiest_engine(&rows, &owned));
            let vram = collected
                .then(|| counter(self.vram, &owned))
                .flatten()
                .map(|rows| {
                    rows.iter()
                        .filter(|(name, _)| {
                            instance(name).is_some_and(|(pid, _)| owned.contains(&pid))
                        })
                        .map(|(_, value)| *value as u64)
                        .sum()
                });
            Stats {
                cpu: percent(cpu),
                gpu: percent(gpu),
                ram: memory(ram),
                vram: memory(vram),
            }
        }
    }
    fn process_tree() -> Option<Vec<(u32, u32)>> {
        unsafe {
            let snapshot = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            Process32FirstW(snapshot.0, &mut entry).ok()?;
            let mut parents = vec![(entry.th32ProcessID, entry.th32ParentProcessID)];
            while Process32NextW(snapshot.0, &mut entry).is_ok() {
                parents.push((entry.th32ProcessID, entry.th32ParentProcessID));
            }
            Some(parents)
        }
    }
    fn ticks(time: FILETIME) -> u64 {
        ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
    }
    fn process_stats(pid: u32) -> Option<(u64, u64, Option<u64>)> {
        unsafe {
            let handle = Handle(
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                    false,
                    pid,
                )
                .ok()?,
            );
            let (mut creation, mut exit, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
            let mut mem = PROCESS_MEMORY_COUNTERS_EX2 {
                cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
                ..Default::default()
            };
            let bytes = GetProcessMemoryInfo(
                handle.0,
                (&mut mem as *mut PROCESS_MEMORY_COUNTERS_EX2).cast(),
                mem.cb,
            )
            .ok()
            .map(|_| mem.PrivateWorkingSetSize as u64);
            Some((ticks(creation), ticks(kernel) + ticks(user), bytes))
        }
    }
    fn counter(handle: isize, owned: &HashSet<u32>) -> Option<Vec<(String, f64)>> {
        if handle == 0 {
            return None;
        }
        unsafe {
            let (mut bytes, mut count) = (0, 0);
            if PdhGetFormattedCounterArrayW(handle, PDH_FMT_DOUBLE, &mut bytes, &mut count, None)
                != PDH_MORE_DATA
            {
                return None;
            }
            for _ in 0..3 {
                if bytes == 0 || bytes > 16 * 1024 * 1024 {
                    return None;
                }
                let size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
                let mut buffer =
                    vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (bytes as usize).div_ceil(size)];
                let result = PdhGetFormattedCounterArrayW(
                    handle,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    Some(buffer.as_mut_ptr()),
                );
                if result == PDH_MORE_DATA {
                    continue;
                }
                if result != 0 || count as usize > buffer.len() {
                    return None;
                }
                let mut rows = Vec::new();
                for item in &buffer[..count as usize] {
                    let name = item.szName.to_string().ok()?;
                    if !instance(&name).is_some_and(|(pid, _)| owned.contains(&pid)) {
                        continue;
                    }
                    if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                        && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
                    {
                        return None;
                    }
                    let value = item.FmtValue.Anonymous.doubleValue;
                    if !value.is_finite() || value < 0.0 {
                        return None;
                    }
                    rows.push((name, value));
                }
                return Some(rows);
            }
            None
        }
    }
}
#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct Sampler;
    impl Sampler {
        pub fn new() -> Self {
            Self
        }
        pub fn sample(&mut self) -> Stats {
            Stats {
                cpu: percent(None),
                gpu: percent(None),
                ram: memory(None),
                vram: memory(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_samples_own_cpu_and_memory_without_elevation() {
        let mut sampler = platform::Sampler::new();
        let first = sampler.sample();
        assert_ne!(first.ram, "—", "private resident RAM must be available");
        std::thread::sleep(Duration::from_millis(100));
        let second = sampler.sample();
        assert_ne!(second.cpu, "—", "CPU delta must be available");
        eprintln!(
            "CPU {} GPU {} RAM {} VRAM {}",
            second.cpu, second.gpu, second.ram, second.vram
        );
    }
    #[test]
    fn includes_nested_helpers_but_excludes_external_servers() {
        assert_eq!(
            owned_processes(10, &[(30, 20), (20, 10), (40, 1)]),
            HashSet::from([10, 20, 30])
        );
    }
    #[test]
    fn gpu_sums_same_engine_across_helpers_without_summing_parallel_engines() {
        let rows = vec![
            ("pid_10_luid_A_eng_0".into(), 30.0),
            ("pid_20_luid_A_eng_0".into(), 20.0),
            ("pid_10_luid_A_eng_1".into(), 40.0),
            ("pid_40_luid_A_eng_0".into(), 90.0),
        ];
        assert_eq!(busiest_engine(&rows, &HashSet::from([10, 20])), 50.0);
    }
    #[test]
    fn cpu_normalizes_capacity_and_ignores_reused_pids() {
        let old = HashMap::from([((10, 1), 10_000_000), ((20, 2), 50_000_000)]);
        let new = HashMap::from([((10, 1), 30_000_000), ((20, 3), 80_000_000)]);
        assert_eq!(cpu_delta(&old, &new, 1.0, 4), 50.0);
        assert_eq!(percent(None), "—");
        assert_eq!(memory(Some(2 << 30)), "2.00 GiB");
    }
}

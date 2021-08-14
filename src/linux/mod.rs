mod sysinfo_ffi;
mod x11_ffi;

use crate::extra;
use crate::extra::list_dir_entries;
use crate::traits::*;
use aparato::{Fetch, PCIDevice};
use itertools::Itertools;
use std::fs;
use std::fs::read_dir;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use sysctl::{Ctl, Sysctl};
use sysinfo_ffi::sysinfo;

impl From<sqlite::Error> for ReadoutError {
    fn from(e: sqlite::Error) -> Self {
        ReadoutError::Other(e.to_string())
    }
}

pub struct LinuxBatteryReadout;

pub struct LinuxKernelReadout {
    os_release_ctl: Option<Ctl>,
    os_type_ctl: Option<Ctl>,
}

pub struct LinuxGeneralReadout {
    hostname_ctl: Option<Ctl>,
    sysinfo: sysinfo,
}

pub struct LinuxMemoryReadout {
    sysinfo: sysinfo,
}
pub struct LinuxProductReadout;

pub struct LinuxPackageReadout;

impl BatteryReadout for LinuxBatteryReadout {
    fn new() -> Self {
        LinuxBatteryReadout
    }

    fn percentage(&self) -> Result<u8, ReadoutError> {
        let mut dirs = list_dir_entries(&PathBuf::from("/sys/class/power_supply"));
        let index = dirs
            .iter()
            .position(|f| f.to_string_lossy().contains("ADP"));
        if let Some(i) = index {
            dirs.remove(i);
        }

        let bat = dirs.first();
        if let Some(b) = bat {
            let path_to_capacity = b.join("capacity");
            let percentage_text = extra::pop_newline(fs::read_to_string(path_to_capacity)?);
            let percentage_parsed = percentage_text.parse::<u8>();

            match percentage_parsed {
                Ok(p) => return Ok(p),
                Err(e) => {
                    return Err(ReadoutError::Other(format!(
                        "Could not parse the value '{}' into a \
            digit: {:?}",
                        percentage_text, e
                    )))
                }
            };
        }

        Err(ReadoutError::Other(format!("No batteries detected.")))
    }

    fn status(&self) -> Result<BatteryState, ReadoutError> {
        let mut dirs = list_dir_entries(&PathBuf::from("/sys/class/power_supply"));
        let index = dirs
            .iter()
            .position(|f| f.to_string_lossy().contains("ADP"));
        if let Some(i) = index {
            dirs.remove(i);
        }

        let bat = dirs.first();
        if let Some(b) = bat {
            let path_to_status = b.join("status");
            let status_text =
                extra::pop_newline(fs::read_to_string(path_to_status)?).to_lowercase();

            match &status_text[..] {
                "charging" => return Ok(BatteryState::Charging),
                "discharging" | "full" => return Ok(BatteryState::Discharging),
                s => {
                    return Err(ReadoutError::Other(format!(
                        "Got an unexpected value \"{}\" reading battery status",
                        s,
                    )))
                }
            }
        }

        Err(ReadoutError::Other(format!("No batteries detected.")))
    }

    fn health(&self) -> Result<u64, ReadoutError> {
        let mut dirs = list_dir_entries(&PathBuf::from("/sys/class/power_supply"));
        let index = dirs
            .iter()
            .position(|f| f.to_string_lossy().contains("ADP"));
        if let Some(i) = index {
            dirs.remove(i);
        }

        let bat = dirs.first();
        if let Some(b) = bat {
            let energy_full =
                extra::pop_newline(fs::read_to_string(b.join("energy_full"))?).parse::<u64>();

            let energy_full_design =
                extra::pop_newline(fs::read_to_string(b.join("energy_full_design"))?)
                    .parse::<u64>();

            match (energy_full, energy_full_design) {
                (Ok(mut ef), Ok(efd)) => {
                    if ef > efd {
                        ef = efd;
                        return Ok(((ef as f64 / efd as f64) * 100 as f64) as u64);
                    }
                    return Ok(((ef as f64 / efd as f64) * 100 as f64) as u64);
                }
                _ => {
                    return Err(ReadoutError::Other(format!(
                        "Error calculating battery health.",
                    )))
                }
            }
        }

        Err(ReadoutError::Other(format!("No batteries detected.")))
    }
}

impl KernelReadout for LinuxKernelReadout {
    fn new() -> Self {
        LinuxKernelReadout {
            os_release_ctl: Ctl::new("kernel.osrelease").ok(),
            os_type_ctl: Ctl::new("kernel.ostype").ok(),
        }
    }

    fn os_release(&self) -> Result<String, ReadoutError> {
        Ok(self
            .os_release_ctl
            .as_ref()
            .ok_or(ReadoutError::MetricNotAvailable)?
            .value_string()?)
    }

    fn os_type(&self) -> Result<String, ReadoutError> {
        Ok(self
            .os_type_ctl
            .as_ref()
            .ok_or(ReadoutError::MetricNotAvailable)?
            .value_string()?)
    }
}

impl GeneralReadout for LinuxGeneralReadout {
    fn new() -> Self {
        LinuxGeneralReadout {
            hostname_ctl: Ctl::new("kernel.hostname").ok(),
            sysinfo: sysinfo::new(),
        }
    }

    fn resolution(&self) -> Result<String, ReadoutError> {
        fn get_resolution_without_x() -> Result<String, ReadoutError> {
            let drm = std::path::Path::new("/sys/class/drm");
            if drm.is_dir() {
                let dirs = extra::list_dir_entries(drm);
                let mut resolution = String::new();
                for entry in dirs {
                    if entry.read_link().is_ok() {
                        let modes = std::path::PathBuf::from(entry).join("modes");
                        if modes.is_file() {
                            if let Ok(mut this_res) = std::fs::read_to_string(modes) {
                                if !this_res.is_empty() {
                                    if this_res.ends_with("\n") {
                                        this_res.pop();
                                    }
                                    resolution.push_str(&this_res);
                                    resolution.push_str(", ");
                                }
                            }
                        }
                    }
                }
                if resolution.trim_end().ends_with(",") {
                    resolution.pop();
                }

                Ok(resolution)
            } else {
                Err(ReadoutError::Other(String::from(
                    "Could not obtain screen resolution from /sys/class/drm.",
                )))
            }
        }

        if cfg!(feature = "xserver") {
            use std::os::raw::c_char;
            use x11_ffi::*;

            let display_name: *const c_char = std::ptr::null_mut();
            let display = unsafe { XOpenDisplay(display_name) };

            if !display.is_null() {
                let screen = unsafe { XDefaultScreen(display) };
                let width = unsafe { XDisplayWidth(display, screen) };
                let height = unsafe { XDisplayHeight(display, screen) };

                unsafe {
                    XCloseDisplay(display);
                    libc::free(display_name as *mut libc::c_void);
                }

                return Ok(format!("{}x{}", width, height));
            } else {
                return get_resolution_without_x();
            }
        } else {
            return get_resolution_without_x();
        }
    }

    fn machine(&self) -> Result<String, ReadoutError> {
        let product_readout = LinuxProductReadout::new();

        let name = product_readout.name()?;
        let family = product_readout.family()?;
        let version = product_readout.version()?;
        let vendor = product_readout.vendor()?;

        let product = format!("{} {} {} {}", vendor, family, name, version)
            .replace("To be filled by O.E.M.", "");

        let new_product: Vec<_> = product.split_whitespace().into_iter().unique().collect();

        if family == name && family == version {
            return Ok(family);
        } else if version.is_empty() || version.len() <= 22 {
            return Ok(new_product.into_iter().join(" "));
        }

        Ok(version)
    }

    fn local_ip(&self) -> Result<String, ReadoutError> {
        crate::shared::local_ip()
    }

    fn username(&self) -> Result<String, ReadoutError> {
        crate::shared::username()
    }

    fn hostname(&self) -> Result<String, ReadoutError> {
        Ok(self
            .hostname_ctl
            .as_ref()
            .ok_or(ReadoutError::MetricNotAvailable)?
            .value_string()?)
    }

    fn distribution(&self) -> Result<String, ReadoutError> {
        use os_release::OsRelease;
        let content = OsRelease::new()?;

        if !content.version.is_empty() {
            return Ok(format!("{} {}", content.name, content.version));
        } else if !content.version_id.is_empty() {
            return Ok(format!("{} {}", content.name, content.version_id));
        }

        Ok(content.name)
    }

    fn desktop_environment(&self) -> Result<String, ReadoutError> {
        crate::shared::desktop_environment()
    }

    fn window_manager(&self) -> Result<String, ReadoutError> {
        crate::shared::window_manager()
    }

    fn terminal(&self) -> Result<String, ReadoutError> {
        // This function returns the PPID of a given PID.
        fn get_parent(pid: i32) -> i32 {
            let process_path = PathBuf::from("/proc").join(pid.to_string()).join("status");
            let file = fs::File::open(process_path);
            match file {
                Ok(content) => {
                    let reader = BufReader::new(content);
                    for line in reader.lines().flatten() {
                        if line.to_uppercase().starts_with("PPID") {
                            let s_mem_kb: String =
                                line.chars().filter(|c| c.is_digit(10)).collect();
                            return s_mem_kb.parse::<i32>().unwrap_or(-1);
                        }
                    }
                    -1
                }
                Err(_e) => -1,
            }
        }

        // This function returns the name associated with the PPID. It can traverse
        // `/proc` to find out the actual terminal in case of a nested shell situation
        fn terminal_name() -> String {
            let mut terminal_pid = get_parent(unsafe { libc::getppid() });

            let shells = ["sh", "su", "bash", "fish", "dash", "zsh", "ksh", "csh"];
            let path = PathBuf::from("/proc")
                .join(terminal_pid.to_string())
                .join("comm");

            if let Ok(mut terminal_name) = fs::read_to_string(path) {
                while shells.contains(&terminal_name.replace("\n", "").as_str()) {
                    let id = get_parent(terminal_pid);
                    terminal_pid = id;

                    let path = PathBuf::from("/proc").join(id.to_string()).join("comm");

                    if let Ok(comm) = fs::read_to_string(path) {
                        terminal_name = comm;
                    }
                }

                return terminal_name;
            }

            String::new()
        }

        let terminal = terminal_name();

        if terminal.is_empty() {
            return Err(ReadoutError::Other(
                "Querying terminal information failed".to_string(),
            ));
        }

        Ok(extra::pop_newline(terminal))
    }

    fn shell(&self, format: ShellFormat, kind: ShellKind) -> Result<String, ReadoutError> {
        crate::shared::shell(format, kind)
    }

    fn gpus(&self) -> Result<Vec<String>, ReadoutError> {
        Ok(PCIDevice::fetch_gpus(None))
    }

    fn cpu_model_name(&self) -> Result<String, ReadoutError> {
        Ok(crate::shared::cpu_model_name())
    }

    fn cpu_physical_cores(&self) -> Result<usize, ReadoutError> {
        crate::shared::cpu_physical_cores()
    }

    fn cpu_cores(&self) -> Result<usize, ReadoutError> {
        crate::shared::cpu_cores()
    }

    fn cpu_usage(&self) -> Result<usize, ReadoutError> {
        let mut info = self.sysinfo;
        let info_ptr: *mut sysinfo = &mut info;
        let ret = unsafe { sysinfo(info_ptr) };
        if ret != -1 {
            let f_load = 1f64 / (1 << libc::SI_LOAD_SHIFT) as f64;
            let cpu_usage = info.loads[0] as f64 * f_load;
            let cpu_usage_u = (cpu_usage / num_cpus::get() as f64 * 100.0).round() as usize;
            Ok(cpu_usage_u as usize)
        } else {
            Err(ReadoutError::Other(
                "Failed to get system statistics".to_string(),
            ))
        }
    }

    fn uptime(&self) -> Result<usize, ReadoutError> {
        let mut info = self.sysinfo;
        let info_ptr: *mut sysinfo = &mut info;
        let ret = unsafe { sysinfo(info_ptr) };
        if ret != -1 {
            Ok(info.uptime as usize)
        } else {
            Err(ReadoutError::Other(
                "Failed to get system statistics".to_string(),
            ))
        }
    }
}

impl MemoryReadout for LinuxMemoryReadout {
    fn new() -> Self {
        LinuxMemoryReadout {
            sysinfo: sysinfo::new(),
        }
    }

    fn total(&self) -> Result<u64, ReadoutError> {
        let mut info = self.sysinfo;
        let info_ptr: *mut sysinfo = &mut info;
        let ret = unsafe { sysinfo(info_ptr) };
        if ret != -1 {
            Ok(info.totalram as u64 * info.mem_unit as u64 / 1024)
        } else {
            Err(ReadoutError::Other(
                "Failed to get system statistics".to_string(),
            ))
        }
    }

    fn free(&self) -> Result<u64, ReadoutError> {
        let mut info = self.sysinfo;
        let info_ptr: *mut sysinfo = &mut info;
        let ret = unsafe { sysinfo(info_ptr) };
        if ret != -1 {
            Ok(info.freeram as u64 * info.mem_unit as u64 / 1024)
        } else {
            Err(ReadoutError::Other(
                "Failed to get system statistics".to_string(),
            ))
        }
    }

    fn buffers(&self) -> Result<u64, ReadoutError> {
        let mut info = self.sysinfo;
        let info_ptr: *mut sysinfo = &mut info;
        let ret = unsafe { sysinfo(info_ptr) };
        if ret != -1 {
            Ok(info.bufferram as u64 * info.mem_unit as u64 / 1024)
        } else {
            Err(ReadoutError::Other(
                "Failed to get system statistics".to_string(),
            ))
        }
    }

    fn cached(&self) -> Result<u64, ReadoutError> {
        Ok(crate::shared::get_meminfo_value("Cached"))
    }

    fn reclaimable(&self) -> Result<u64, ReadoutError> {
        Ok(crate::shared::get_meminfo_value("SReclaimable"))
    }

    fn used(&self) -> Result<u64, ReadoutError> {
        let total = self.total().unwrap();
        let free = self.free().unwrap();
        let cached = self.cached().unwrap();
        let reclaimable = self.reclaimable().unwrap();
        let buffers = self.buffers().unwrap();
        Ok(total - free - cached - reclaimable - buffers)
    }
}

impl ProductReadout for LinuxProductReadout {
    fn new() -> Self {
        LinuxProductReadout
    }

    fn version(&self) -> Result<String, ReadoutError> {
        Ok(extra::pop_newline(fs::read_to_string(
            "/sys/class/dmi/id/product_version",
        )?))
    }

    fn vendor(&self) -> Result<String, ReadoutError> {
        Ok(extra::pop_newline(fs::read_to_string(
            "/sys/class/dmi/id/sys_vendor",
        )?))
    }

    fn family(&self) -> Result<String, ReadoutError> {
        Ok(extra::pop_newline(fs::read_to_string(
            "/sys/class/dmi/id/product_family",
        )?))
    }
    fn name(&self) -> Result<String, ReadoutError> {
        Ok(extra::pop_newline(fs::read_to_string(
            "/sys/class/dmi/id/product_name",
        )?))
    }
}

impl PackageReadout for LinuxPackageReadout {
    fn new() -> Self {
        LinuxPackageReadout
    }

    fn count_pkgs(&self) -> Vec<(PackageManager, usize)> {
        let mut packages = Vec::new();
        // Instead of having a condition for each distribution.
        // we will try and extract package count by checking
        // if a certain package manager is installed
        if extra::which("pacman") {
            if let Some(c) = LinuxPackageReadout::count_pacman() {
                packages.push((PackageManager::Pacman, c));
            }
        } else if extra::which("dpkg") {
            if let Some(c) = LinuxPackageReadout::count_dpkg() {
                packages.push((PackageManager::Dpkg, c));
            }
        } else if extra::which("qlist") {
            if let Some(c) = LinuxPackageReadout::count_portage() {
                packages.push((PackageManager::Portage, c));
            }
        } else if extra::which("xbps-query") {
            if let Some(c) = LinuxPackageReadout::count_xbps() {
                packages.push((PackageManager::Xbps, c));
            }
        } else if extra::which("rpm") {
            if let Some(c) = LinuxPackageReadout::count_rpm() {
                packages.push((PackageManager::Rpm, c));
            }
        } else if extra::which("eopkg") {
            if let Some(c) = LinuxPackageReadout::count_eopkg() {
                packages.push((PackageManager::Eopkg, c));
            }
        } else if extra::which("apk") {
            if let Some(c) = LinuxPackageReadout::count_apk() {
                packages.push((PackageManager::Apk, c));
            }
        }

        if extra::which("cargo") {
            if let Some(c) = LinuxPackageReadout::count_cargo() {
                packages.push((PackageManager::Cargo, c));
            }
        }
        if extra::which("flatpak") {
            if let Some(c) = LinuxPackageReadout::count_flatpak() {
                packages.push((PackageManager::Flatpak, c));
            }
        }
        if extra::which("snap") {
            if let Some(c) = LinuxPackageReadout::count_snap() {
                packages.push((PackageManager::Snap, c));
            }
        }

        packages
    }
}

impl LinuxPackageReadout {
    /// Returns the number of installed packages for systems
    /// that utilize `rpm` as their package manager.
    fn count_rpm() -> Option<usize> {
        // Return the number of installed packages using sqlite (~1ms)
        // as directly calling rpm or dnf is too expensive (~500ms)
        let path = "/var/lib/rpm/rpmdb.sqlite";
        let connection = sqlite::open(path);
        if let Ok(con) = connection {
            let statement = con.prepare("SELECT COUNT(*) FROM Installtid");
            if let Ok(mut s) = statement {
                if s.next().is_ok() {
                    return match s.read::<Option<i64>>(0) {
                        Ok(Some(count)) => Some(count as usize),
                        _ => None,
                    };
                }
            }
        }

        None
    }

    /// Returns the number of installed packages for systems
    /// that utilize `pacman` as their package manager.
    fn count_pacman() -> Option<usize> {
        let pacman_dir = Path::new("/var/lib/pacman/local");
        if pacman_dir.exists() {
            if let Ok(read_dir) = read_dir(pacman_dir) {
                return Some(read_dir.count() - 1);
            };
        }

        None
    }

    /// Returns the number of installed packages for systems
    /// that utilize `eopkg` as their package manager.
    fn count_eopkg() -> Option<usize> {
        let eopkg_dir = Path::new("/var/lib/eopkg/package");
        if eopkg_dir.exists() {
            if let Ok(read_dir) = read_dir(eopkg_dir) {
                return Some(read_dir.count() - 1);
            };
        }

        None
    }

    /// Returns the number of installed packages for systems
    /// that utilize `dpkg` as their package manager.
    fn count_dpkg() -> Option<usize> {
        let dpkg_dir = Path::new("/var/lib/dpkg/info");
        let dir_entries = extra::list_dir_entries(dpkg_dir);
        if !dir_entries.is_empty() {
            return Some(
                dir_entries
                    .iter()
                    .filter(|x| {
                        if let Some(ext) = extra::path_extension(x) {
                            ext == "list"
                        } else {
                            false
                        }
                    })
                    .into_iter()
                    .count(),
            );
        }

        None
    }

    /// Returns the number of installed packages for systems
    /// that utilize `portage` as their package manager.
    fn count_portage() -> Option<usize> {
        let qlist_output = Command::new("qlist")
            .arg("-I")
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        extra::count_lines(
            String::from_utf8(qlist_output.stdout)
                .expect("ERROR: \"qlist -I\" output was not valid UTF-8"),
        )
    }

    /// Returns the number of installed packages for systems
    /// that utilize `xbps` as their package manager.
    fn count_xbps() -> Option<usize> {
        let xbps_output = Command::new("xbps-query")
            .arg("-l")
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        extra::count_lines(
            String::from_utf8(xbps_output.stdout)
                .expect("ERROR: \"xbps-query -l\" output was not valid UTF-8"),
        )
    }

    /// Returns the number of installed packages for systems
    /// that utilize `apk` as their package manager.
    fn count_apk() -> Option<usize> {
        let apk_output = Command::new("apk")
            .arg("info")
            .stdout(Stdio::piped())
            .output()
            .unwrap();

        extra::count_lines(
            String::from_utf8(apk_output.stdout)
                .expect("ERROR: \"apk info\" output was not valid UTF-8"),
        )
    }

    /// Returns the number of installed packages for systems
    /// that have `cargo` installed.
    fn count_cargo() -> Option<usize> {
        crate::shared::count_cargo()
    }

    /// Returns the number of installed packages for systems
    /// that have `flatpak` installed.
    fn count_flatpak() -> Option<usize> {
        // Return the number of system-wide installed flatpaks
        let global_flatpak_dir = Path::new("/var/lib/flatpak/app");
        let mut global_packages = 0;
        if let Ok(dir) = read_dir(global_flatpak_dir) {
            global_packages = dir.count();
        }

        // Return the number of per-user installed flatpaks
        let mut user_packages: usize = 0;
        if let Some(home_dir) = dirs::home_dir() {
            let user_flatpak_dir = home_dir.join(".local/share/flatpak/app");
            if let Ok(dir) = read_dir(user_flatpak_dir) {
                user_packages = dir.count();
            }
        }

        let total = global_packages + user_packages;
        if total > 0 {
            return Some(total);
        }

        None
    }

    /// Returns the number of installed packages for systems
    /// that have `snap` installed.
    fn count_snap() -> Option<usize> {
        let snap_dir = Path::new("/var/lib/snapd/snaps");
        if snap_dir.is_dir() {
            let dir_entries = extra::list_dir_entries(snap_dir);
            if !dir_entries.is_empty() {
                return Some(
                    dir_entries
                        .iter()
                        .filter(|x| {
                            if let Some(ext) = extra::path_extension(x) {
                                ext == "snap"
                            } else {
                                false
                            }
                        })
                        .into_iter()
                        .count(),
                );
            }
        }

        None
    }
}

//! Mounted volume discovery: kind (SSD/HDD/network), physical device identity
//! (so two partitions of one drive are not counted as two copies), free space.

use crate::darwin_io::{self, FsStat};
use crate::scan::detect_card_vendor;
use crate::util::{output_with_timeout, run_with_timeout};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Ssd,
    Hdd,
    Network,
    Unknown,
}

impl DeviceKind {
    /// Rotational and network destinations verify after copying.
    pub fn defer_verify(self) -> bool {
        matches!(self, DeviceKind::Hdd | DeviceKind::Network)
    }

    /// How many transfers may write to one physical device at once.
    pub fn concurrent_writers(self) -> usize {
        match self {
            DeviceKind::Ssd => 3,
            DeviceKind::Unknown => 2,
            DeviceKind::Hdd | DeviceKind::Network => 1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Ssd => "SSD",
            DeviceKind::Hdd => "HDD",
            DeviceKind::Network => "NAS",
            DeviceKind::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub mount_point: String,
    pub name: String,
    pub fs_type: String,
    pub device_node: String,
    /// Physical device identity, e.g. "disk4" or "net:nas.local".
    pub device_id: String,
    pub kind: DeviceKind,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub block_size: u64,
    pub internal: bool,
    pub ejectable: bool,
    /// Removable media (SD/CF readers usually report this).
    pub removable: bool,
    pub network: bool,
    pub read_only: bool,
    pub protocol: Option<String>,
    pub media_name: Option<String>,
    pub volume_uuid: Option<String>,
    pub card_vendor: Option<String>,
    pub is_camera_card: bool,
    /// The volume did not answer a directory listing in time. Usually a
    /// pending macOS privacy prompt or a stalled drive.
    pub unresponsive: bool,
}

#[derive(Debug, Clone, Default)]
struct DiskInfo {
    physical_whole: Option<String>,
    solid_state: Option<bool>,
    internal: Option<bool>,
    ejectable: Option<bool>,
    removable: Option<bool>,
    protocol: Option<String>,
    media_name: Option<String>,
    volume_uuid: Option<String>,
    volume_name: Option<String>,
}

fn whole_disk(id: &str) -> String {
    // "disk4s2" -> "disk4", "disk7s1s1" -> "disk7"
    let id = id.trim_start_matches("/dev/");
    let digits_end = id
        .char_indices()
        .skip(4)
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(id.len());
    id[..digits_end].to_string()
}

fn diskutil_info(arg: &str) -> Option<DiskInfo> {
    let out =
        output_with_timeout(Command::new("/usr/sbin/diskutil").args(["info", "-plist", arg]), DISKUTIL_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let dict: plist::Dictionary = plist::from_bytes(&out.stdout).ok()?;
    let s = |k: &str| dict.get(k).and_then(|v| v.as_string()).map(str::to_string).filter(|v| !v.is_empty());
    let b = |k: &str| dict.get(k).and_then(|v| v.as_boolean());
    let physical = dict
        .get("APFSPhysicalStores")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_dictionary())
        .and_then(|d| d.get("APFSPhysicalStore"))
        .and_then(|v| v.as_string())
        .map(whole_disk)
        .or_else(|| s("ParentWholeDisk").map(|p| whole_disk(&p)));
    Some(DiskInfo {
        physical_whole: physical,
        solid_state: b("SolidState"),
        internal: b("Internal"),
        ejectable: b("Ejectable"),
        removable: b("RemovableMedia").or(b("Removable")),
        protocol: s("BusProtocol").or_else(|| s("Protocol")),
        media_name: s("MediaName").or_else(|| s("IORegistryEntryName")),
        volume_uuid: s("VolumeUUID"),
        volume_name: s("VolumeName"),
    })
}

const NETWORK_FS: &[&str] = &["smbfs", "afpfs", "nfs", "webdav", "cifs"];

fn is_listed(st: &FsStat) -> bool {
    let mp = st.mount_point.as_str();
    if mp == "/System/Volumes/Data" {
        return true;
    }
    if !mp.starts_with("/Volumes/") {
        return false;
    }
    let dont_browse = st.flags & (libc::MNT_DONTBROWSE as u32) != 0;
    !(dont_browse && !NETWORK_FS.contains(&st.fs_type.as_str())) && !mp.contains("com.apple.TimeMachine")
}

/// Caches `diskutil` answers per device node (each call takes ~50 ms and can
/// spin up a sleeping HDD).
#[derive(Clone, Default)]
struct ProbeState {
    pending: bool,
    vendor: Option<Option<String>>,
}

#[derive(Default)]
pub struct VolumeCache {
    disks: Mutex<HashMap<String, Option<DiskInfo>>>,
    probes: Arc<Mutex<HashMap<String, ProbeState>>>,
}

impl VolumeCache {
    /// Card-vendor detection with a deadline. Returns (vendor, responsive).
    /// A probe that hangs is never started twice for the same mount.
    fn probe_card(&self, mount: &str, refresh: bool) -> (Option<String>, bool) {
        {
            let mut probes = match self.probes.lock() {
                Ok(p) => p,
                Err(_) => return (None, false),
            };
            let st = probes.entry(mount.to_string()).or_default();
            if st.pending {
                return (None, false);
            }
            if !refresh && let Some(v) = &st.vendor {
                return (v.clone(), true);
            }
            st.pending = true;
        }
        let probes = self.probes.clone();
        let key = mount.to_string();
        let path = PathBuf::from(mount);
        let result = run_with_timeout(PROBE_TIMEOUT, move || {
            let v = detect_card_vendor(&path).map(str::to_string);
            if let Ok(mut p) = probes.lock() {
                let st = p.entry(key).or_default();
                st.pending = false;
                st.vendor = Some(v.clone());
            }
            v
        });
        match result {
            Some(v) => (v, true),
            None => (None, false),
        }
    }

    /// Whether `path` answers a metadata + directory read within the probe
    /// deadline.
    pub fn is_responsive(path: &Path) -> bool {
        let p = path.to_path_buf();
        run_with_timeout(PROBE_TIMEOUT, move || std::fs::read_dir(&p).map(|mut rd| rd.next()).is_ok())
            .unwrap_or(false)
    }

    fn disk_info(&self, node: &str) -> Option<DiskInfo> {
        if let Some(hit) = self.disks.lock().ok().and_then(|m| m.get(node).cloned()) {
            return hit;
        }
        let info = diskutil_info(node);
        if let Ok(mut m) = self.disks.lock() {
            m.insert(node.to_string(), info.clone());
        }
        info
    }

    pub fn forget(&self) {
        if let Ok(mut m) = self.disks.lock() {
            m.clear();
        }
    }

    /// Volumes a user can pick: everything under /Volumes plus the internal
    /// data volume.
    pub fn list(&self) -> Vec<VolumeInfo> {
        let mounts: Vec<FsStat> = darwin_io::mounted_filesystems().into_iter().filter(is_listed).collect();
        let infos: Vec<Option<DiskInfo>> = std::thread::scope(|s| {
            let handles: Vec<_> = mounts
                .iter()
                .map(|m| {
                    s.spawn(move || {
                        if NETWORK_FS.contains(&m.fs_type.as_str()) || !m.device.starts_with("/dev/") {
                            None
                        } else {
                            self.disk_info(&m.device)
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        let mut out: Vec<VolumeInfo> = mounts
            .iter()
            .zip(infos)
            .map(|(m, info)| {
                let mut v = build_info(m, info.as_ref());
                if card_candidate(&v) {
                    let (vendor, ok) = self.probe_card(&m.mount_point, false);
                    v.unresponsive = !ok;
                    apply_vendor(&mut v, vendor);
                }
                v
            })
            .collect();
        out.sort_by(|a, b| {
            (b.is_camera_card, a.internal, a.name.to_lowercase()).cmp(&(a.is_camera_card, b.internal, b.name.to_lowercase()))
        });
        out
    }

    /// Volume that contains `path` (the deepest mount point).
    pub fn volume_for_path(&self, path: &Path) -> Option<VolumeInfo> {
        let st = darwin_io::statfs(path).ok()?;
        let info = if NETWORK_FS.contains(&st.fs_type.as_str()) || !st.device.starts_with("/dev/") {
            None
        } else {
            self.disk_info(&st.device)
        };
        Some(build_info(&st, info.as_ref()))
    }
}

fn card_candidate(v: &VolumeInfo) -> bool {
    let fs = v.fs_type.to_lowercase();
    matches!(fs.as_str(), "exfat" | "msdos" | "vfat") && !v.network && !v.internal
}

/// Camera layout + removable media, or an ejectable device of card size
/// (<= 1 TiB). A big SSD holding a copied DCIM folder is not a card.
fn apply_vendor(v: &mut VolumeInfo, vendor: Option<String>) {
    let card_sized = v.total_bytes > 0 && v.total_bytes <= 1024 * 1024 * 1024 * 1024;
    v.is_camera_card = vendor.is_some() && (v.removable || (v.ejectable && card_sized));
    v.card_vendor = vendor;
}

fn network_device_id(from: &str) -> String {
    // "//user@nas.local/share" -> "net:nas.local"
    let s = from.trim_start_matches('/');
    let s = s.rsplit_once('@').map(|(_, h)| h).unwrap_or(s);
    let host = s.split('/').next().unwrap_or(s);
    format!("net:{}", host.to_lowercase())
}

fn build_info(m: &FsStat, info: Option<&DiskInfo>) -> VolumeInfo {
    let network = NETWORK_FS.contains(&m.fs_type.as_str());
    let name = if m.mount_point == "/System/Volumes/Data" {
        l!("Macintosh HD (internal)", "Macintosh HD (ổ trong)")
    } else {
        info.and_then(|i| i.volume_name.clone()).unwrap_or_else(|| {
            Path::new(&m.mount_point).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        })
    };
    let device_id = if network {
        network_device_id(&m.device)
    } else {
        info.and_then(|i| i.physical_whole.clone()).unwrap_or_else(|| whole_disk(&m.device))
    };
    let kind = if network {
        DeviceKind::Network
    } else {
        match info.and_then(|i| i.solid_state) {
            Some(true) => DeviceKind::Ssd,
            Some(false) => DeviceKind::Hdd,
            None => DeviceKind::Unknown,
        }
    };
    let internal = info.and_then(|i| i.internal).unwrap_or(false);
    let removable = info.and_then(|i| i.removable).unwrap_or(false);
    let ejectable = info.and_then(|i| i.ejectable).unwrap_or(!internal && !network) || removable;
    VolumeInfo {
        mount_point: m.mount_point.clone(),
        name,
        fs_type: m.fs_type.clone(),
        device_node: m.device.clone(),
        device_id,
        kind,
        total_bytes: m.total_bytes,
        free_bytes: m.free_bytes,
        block_size: m.block_size,
        internal,
        ejectable,
        removable,
        network,
        read_only: m.flags & (libc::MNT_RDONLY as u32) != 0,
        protocol: info.and_then(|i| i.protocol.clone()),
        media_name: info.and_then(|i| i.media_name.clone()),
        volume_uuid: info.and_then(|i| i.volume_uuid.clone()),
        card_vendor: None,
        is_camera_card: false,
        unresponsive: false,
    }
}

/// Unmount and eject a volume (and its whole disk, e.g. an SD card).
pub fn eject(mount_point: &str) -> Result<(), String> {
    let out = output_with_timeout(
        Command::new("/usr/sbin/diskutil").args(["eject", mount_point]),
        Duration::from_secs(60),
    )
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if msg.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { msg };
        Err(l!("Cannot eject {}: {}", "Không eject được {}: {}", mount_point, msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disk_ids() {
        assert_eq!(whole_disk("disk4s2"), "disk4");
        assert_eq!(whole_disk("/dev/disk7s1"), "disk7");
        assert_eq!(whole_disk("disk2s1s1"), "disk2");
        assert_eq!(whole_disk("disk12"), "disk12");
    }

    #[test]
    fn network_ids() {
        assert_eq!(network_device_id("//kim@NAS.local/Footage"), "net:nas.local");
        assert_eq!(network_device_id("//nas.local/Footage"), "net:nas.local");
    }

    #[test]
    fn lists_internal_volume() {
        let cache = VolumeCache::default();
        let vols = cache.list();
        let data = vols.iter().find(|v| v.mount_point == "/System/Volumes/Data").expect("data volume");
        assert!(data.internal);
        assert_eq!(data.kind, DeviceKind::Ssd);
        assert!(data.device_id.starts_with("disk"));
        let tmp = cache.volume_for_path(Path::new("/tmp")).unwrap();
        assert_eq!(tmp.device_id, data.device_id);
    }
}

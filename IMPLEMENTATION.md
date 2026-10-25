# OFFSHOOT PRO PERSONAL (macOS Edition) — MASTER IMPLEMENTATION BLUEPRINT

Tài liệu này chứa toàn bộ hướng dẫn thiết lập môi trường, kiến trúc thư mục và mã nguồn hoàn chỉnh để triển khai phần mềm **OffShoot Pro Personal** (Sao chép Đa đích + Thác đổ Cascading + Kiểm tra mã băm Bypass RAM Cache + Xuất chuẩn `.mhl` + Xuất báo cáo FoolCat HTML/PDF) ngay trong **Visual Studio Code**.

---

## 1. YÊU CẦU HỆ THỐNG & KHỞI TẠO DỰ ÁN TRONG VS CODE

Mở **Terminal** tích hợp trong Visual Studio Code (`Ctrl + ~` hoặc `Cmd + J`) và chạy lần lượt các lệnh dưới đây để cài đặt công cụ nền tảng trên macOS:

### Bước 1.1: Cài đặt Xcode Command Line Tools, Rust & FFmpeg
```bash
# 1. Cài đặt trình biên dịch C/Clang và IOKit headers của macOS
xcode-select --install

# 2. Cài đặt Rust Toolchain (nếu chưa có)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Cài đặt FFmpeg (dùng cho module tạo báo cáo FoolCat & trích xuất Thumbnail)
brew install ffmpeg node
```

### Bước 1.2: Khởi tạo khung dự án Tauri v2 + React + TypeScript
```bash
# Tạo thư mục dự án và khởi tạo bằng Vite + Tauri v2
npm create tauri-app@latest offshoot-pro -- --template react-ts --manager npm
cd offshoot-pro

# Cài đặt TailwindCSS và bộ icon Lucide cho giao diện DIT Dark Mode
npm install -D tailwindcss postcss autoprefixer
npm install lucide-react @tauri-apps/api @tauri-apps/plugin-dialog @tauri-apps/plugin-shell
npx tailwindcss init -p
```

---

## 2. CẤU TRÚC THƯ MỤC DỰ ÁN CHUẨN

Sau khi tạo các file theo tài liệu này, cây thư mục trong VS Code Explorer của bạn sẽ có cấu trúc như sau:

```text
offshoot-pro/
├── IMPLEMENTATION_PLAN.md              # File tài liệu hướng dẫn này
├── package.json
├── src/
│   ├── App.tsx                         # Giao diện DIT 3 cột + Cascading Queue + Report UI
│   ├── main.tsx
│   └── styles.css                      # Cấu hình TailwindCSS Dark Mode
└── src-tauri/
    ├── Cargo.toml                      # Khai báo thư viện Rust hệ thống
    ├── tauri.conf.json                 # Cấu hình quyền truy cập ổ đĩa macOS
    └── src/
        ├── main.rs                     # Entrypoint của ứng dụng
        ├── lib.rs                      # Tauri Commands & Event Emitter
        ├── engine/
        │   ├── mod.rs
        │   ├── darwin_io.rs            # Syscall F_NOCACHE, F_FULLFSYNC, lọc rác macOS
        │   ├── hasher.rs               # Bộ băm siêu tốc XXH3-64, xxHash64, MD5, SHA-256
        │   ├── copier.rs               # Engine đọc 1 lần - ghi đa đích + Cold Verify
        │   └── cascading.rs            # Điều phối Thác đổ (Thẻ -> SSD -> HDD)
        └── postprocess/
            ├── mod.rs
            ├── preset_path.rs          # Tạo cây thư mục chuẩn DIT & Auto-increment Roll
            ├── mhl_writer.rs           # Xuất file chuẩn .mhl (Media Hash List XML)
            └── report_gen.rs           # FoolCat Engine: FFprobe Metadata + 3 Thumbnails + HTML/PDF
```

---

## 3. CẤU HÌNH BACKEND RUST (`src-tauri/Cargo.toml`)

Thay thế nội dung file `src-tauri/Cargo.toml` bằng cấu hình tối ưu hiệu năng (`opt-level = 3`, `lto = true`) dưới đây:

```toml
[package]
name = "offshoot-pro"
version = "1.0.0"
description = "Professional DIT Offload, Verification & Camera Report Engine for macOS"
authors = ["DIT Engineer"]
edition = "2021"

[lib]
name = "offshoot_pro_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-dialog = "2"
tauri-plugin-shell = "2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.38", features = ["full"] }
xxhash-rust = { version = "0.8", features = ["xxh64", "xxh3"] }
md-5 = "0.10"
sha2 = "0.10"
hex = "0.4"
libc = "0.2"
filetime = "0.2"
walkdir = "2.5"
chrono = { version = "0.4", features = ["serde"] }
sysinfo = "0.30"
anyhow = "1.0"
uuid = { version = "1.8", features = ["v4"] }

[profile.release]
panic = "abort"
codegen-units = 1
lto = true
opt-level = 3
strip = true
```

---

## 4. MÃ NGUỒN LÕI RUST (HIGH-PERFORMANCE CORE)

### 4.1. Khai báo Module (`src-tauri/src/engine/mod.rs` & `src-tauri/src/postprocess/mod.rs`)

Tạo file `src-tauri/src/engine/mod.rs`:
```rust
pub mod darwin_io;
pub mod hasher;
pub mod copier;
pub mod cascading;
```

Tạo file `src-tauri/src/postprocess/mod.rs`:
```rust
pub mod preset_path;
pub mod mhl_writer;
pub mod report_gen;
```

---

### 4.2. Xử lý I/O Vật lý trên macOS (`src-tauri/src/engine/darwin_io.rs`)

Module này trực tiếp gọi nhân Darwin để tắt bộ đệm RAM (`F_NOCACHE`), ép xả bộ nhớ đệm của controller ổ cứng (`F_FULLFSYNC`), giữ nguyên ngày giờ quay gốc (`mtime`) và lọc sạch file rác của macOS.

```rust
use anyhow::{Context, Result};
use filetime::{set_file_mtime, FileTime};
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Danh sách các file và thư mục rác của macOS/Windows cần loại bỏ khi Offload thẻ nhớ
const OS_JUNK_NAMES: &[&str] = &[
    ".DS_Store",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
    ".TemporaryItems",
    "Thumbs.db",
    "desktop.ini",
    "System Volume Information",
];

/// Kiểm tra xem một đường dẫn có phải là file ẩn/rác hệ điều hành hay không
pub fn is_os_junk_file(path: &Path) -> bool {
    for component in path.components() {
        if let Some(name) = component.as_os_str().to_str() {
            // Loại bỏ file AppleDouble (._filename) sinh ra trên thẻ exFAT/FAT32
            if name.starts_with("._") {
                return true;
            }
            if OS_JUNK_NAMES.contains(&name) {
                return true;
            }
        }
    }
    false
}

/// Bật cờ F_NOCACHE trên File Descriptor của macOS để đọc/ghi xuyên qua Unified RAM Cache
pub fn enable_darwin_nocache(file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    unsafe {
        let fd = file.as_raw_fd();
        let ret = libc::fcntl(fd, libc::F_NOCACHE, 1);
        if ret == -1 {
            return Err(anyhow::anyhow!(
                "Không thể bật F_NOCACHE trên fd {}: errno {}",
                fd,
                *libc::__error()
            ));
        }
    }
    Ok(())
}

/// Ép bộ điều khiển vật lý của ổ SSD/HDD xả toàn bộ cache xuống chip NAND / đĩa từ
pub fn darwin_full_fsync(file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    unsafe {
        let fd = file.as_raw_fd();
        let ret = libc::fcntl(fd, libc::F_FULLFSYNC);
        if ret == -1 {
            // Fallback sang sync_all nếu ổ mạng SMB không hỗ trợ F_FULLFSYNC
            file.sync_all()?;
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()?;
    }
    Ok(())
}

/// Mở file nguồn ở chế độ Direct Uncached Read
pub fn open_uncached_reader(path: &Path) -> Result<File> {
    let file = File::open(path)
        .with_context(|| format!("Không thể mở file nguồn: {}", path.display()))?;
    enable_darwin_nocache(&file)?;
    Ok(file)
}

/// Tạo file đích ở chế độ Direct Uncached Write
pub fn create_uncached_writer(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("Không thể tạo file đích: {}", path.display()))?;
    enable_darwin_nocache(&file)?;
    Ok(file)
}

/// Sao chép nguyên vẹn Timestamp khởi tạo/chỉnh sửa của file máy quay gốc sang ổ đích
pub fn preserve_camera_timestamps(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::metadata(src)?;
    let mtime = FileTime::from_last_modification_time(&meta);
    set_file_mtime(dst, mtime)?;
    Ok(())
}
```

---

### 4.3. Bộ Băm Siêu Tốc Đa Thuật Toán (`src-tauri/src/engine/hasher.rs`)

Hỗ trợ `XXH3-64`, `xxHash64` (chuẩn mặc định của OffShoot/Silverstack), `MD5` và `SHA-256`.

```rust
use md5::{Digest as Md5Digest, Md5};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh64::Xxh64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashAlgorithm {
    #[serde(rename = "xxh64")]
    Xxh64,
    #[serde(rename = "xxh3")]
    Xxh3,
    #[serde(rename = "md5")]
    Md5,
    #[serde(rename = "sha256")]
    Sha256,
}

impl HashAlgorithm {
    /// Tên thẻ XML chuẩn theo đặc tả ASC-MHL / MHL v1.1
    pub fn mhl_tag_name(&self) -> &'static str {
        match self {
            HashAlgorithm::Xxh64 => "xxhash64be",
            HashAlgorithm::Xxh3 => "xxh3",
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha256 => "sha256",
        }
    }
}

pub enum StreamHasher {
    Xxh64(Box<Xxh64>),
    Xxh3(Box<Xxh3>),
    Md5(Md5),
    Sha256(Sha256),
}

impl StreamHasher {
    pub fn new(algo: HashAlgorithm) -> Self {
        match algo {
            HashAlgorithm::Xxh64 => StreamHasher::Xxh64(Box::new(Xxh64::new(0))),
            HashAlgorithm::Xxh3 => StreamHasher::Xxh3(Box::new(Xxh3::new())),
            HashAlgorithm::Md5 => StreamHasher::Md5(Md5::new()),
            HashAlgorithm::Sha256 => StreamHasher::Sha256(Sha256::new()),
        }
    }

    #[inline]
    pub fn update(&mut self, chunk: &[u8]) {
        match self {
            StreamHasher::Xxh64(h) => h.update(chunk),
            StreamHasher::Xxh3(h) => h.update(chunk),
            StreamHasher::Md5(h) => h.update(chunk),
            StreamHasher::Sha256(h) => h.update(chunk),
        }
    }

    pub fn finalize_hex(self) -> String {
        match self {
            StreamHasher::Xxh64(h) => format!("{:016x}", h.digest()),
            StreamHasher::Xxh3(h) => format!("{:016x}", h.digest()),
            StreamHasher::Md5(h) => hex::encode(h.finalize()),
            StreamHasher::Sha256(h) => hex::encode(h.finalize()),
        }
    }
}
```

---

### 4.4. Engine Sao Chép Đọc 1 Lần – Ghi Đa Đích & Verify (`src-tauri/src/engine/copier.rs`)

Đọc từng khối $4\text{ MB}$ từ thẻ nhớ vào RAM đúng 1 lần duy nhất, vừa băm tại chỗ vừa ghi ra nhiều ổ SSD cùng lúc, sau đó đọc ngược lại từ đĩa vật lý để đối chiếu mã băm và hỗ trợ **Resume** (bỏ qua file đã tồn tại và khớp mã băm).

```rust
use crate::engine::darwin_io::{
    create_uncached_writer, darwin_full_fsync, is_os_junk_file, open_uncached_reader,
    preserve_camera_timestamps,
};
use crate::engine::hasher::{HashAlgorithm, StreamHasher};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

/// Kích thước Chunk 4MB tối ưu cho băng thông thẻ CFexpress Type A/B & NVMe
const CHUNK_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedFileRecord {
    pub relative_path: String,
    pub size_bytes: u64,
    pub hash_hex: String,
    pub algorithm: HashAlgorithm,
    pub file_mtime_utc: DateTime<Utc>,
    pub verified_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgressEvent {
    pub job_id: String,
    pub stage: String, // "Scanning" | "Copying" | "Verifying" | "Cascading" | "SafeToEject" | "Completed" | "Error"
    pub current_file: String,
    pub bytes_processed: u64,
    pub total_bytes: u64,
    pub speed_mbps: f64,
    pub eta_seconds: u64,
    pub message: String,
}

/// Quét toàn bộ file hợp lệ trong thẻ nhớ (loại bỏ rác macOS)
pub fn scan_source_directory(source_root: &Path) -> Result<(Vec<(PathBuf, PathBuf, u64)>, u64)> {
    let mut files = Vec::new();
    let mut total_bytes = 0u64;

    for entry in WalkDir::new(source_root).into_iter().filter_map(|e| e.ok()) {
        let full_path = entry.path();
        if full_path.is_file() && !is_os_junk_file(full_path) {
            let rel_path = full_path
                .strip_prefix(source_root)
                .context("Lỗi tách đường dẫn tương đối")?
                .to_path_buf();
            let size = entry.metadata()?.len();
            total_bytes += size;
            files.push((full_path.to_path_buf(), rel_path, size));
        }
    }
    Ok((files, total_bytes))
}

/// Tính mã băm trực tiếp từ đĩa vật lý (Bypass OS RAM Cache)
pub fn compute_cold_file_hash(path: &Path, algo: HashAlgorithm) -> Result<String> {
    let mut file = open_uncached_reader(path)?;
    let mut hasher = StreamHasher::new(algo);
    let mut buffer = vec![0u8; CHUNK_SIZE];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize_hex())
}

/// Thực thi sao chép Single-Read Multi-Write tới nhiều ổ đích song song + Cold Verify
pub fn copy_and_verify_multi_destinations<F>(
    job_id: &str,
    source_root: &Path,
    destination_roots: &[PathBuf],
    algo: HashAlgorithm,
    mut on_progress: F,
) -> Result<Vec<VerifiedFileRecord>>
where
    F: FnMut(TransferProgressEvent),
{
    let (file_list, total_bytes) = scan_source_directory(source_root)?;
    let mut verified_records = Vec::with_capacity(file_list.len());
    let mut bytes_copied_so_far = 0u64;
    let start_time = Instant::now();
    let mut buffer = vec![0u8; CHUNK_SIZE];

    // BƯỚC 1: ĐỌC 1 LẦN TỪ THẺ -> BĂM INLINE -> GHI ĐA ĐÍCH
    for (src_full, rel_path, file_size) in &file_list {
        let rel_str = rel_path.to_string_lossy().to_string();

        // Kiểm tra Stop & Resume: Nếu tất cả các ổ đích đã có sẵn file này đúng dung lượng & khớp mã băm thì bỏ qua
        let mut all_dests_already_valid = true;
        for dst_root in destination_roots {
            let dst_file = dst_root.join(rel_path);
            if !dst_file.exists() || dst_file.metadata().map(|m| m.len()).unwrap_or(0) != *file_size
            {
                all_dests_already_valid = false;
                break;
            }
        }

        let source_hash = if all_dests_already_valid {
            let src_h = compute_cold_file_hash(src_full, algo)?;
            let mut hashes_match = true;
            for dst_root in destination_roots {
                let dst_h = compute_cold_file_hash(&dst_root.join(rel_path), algo)?;
                if dst_h != src_h {
                    hashes_match = false;
                    break;
                }
            }
            if hashes_match {
                bytes_copied_so_far += *file_size;
                src_h
            } else {
                copy_single_file_multi_writer(
                    job_id,
                    src_full,
                    rel_path,
                    destination_roots,
                    algo,
                    &mut buffer,
                    &mut bytes_copied_so_far,
                    total_bytes,
                    start_time,
                    &mut on_progress,
                )?
            }
        } else {
            copy_single_file_multi_writer(
                job_id,
                src_full,
                rel_path,
                destination_roots,
                algo,
                &mut buffer,
                &mut bytes_copied_so_far,
                total_bytes,
                start_time,
                &mut on_progress,
            )?
        };

        // BƯỚC 2: COLD VERIFICATION (ĐỌC NGƯỢC TỪ BỀ MẶT VẬT LÝ CỦA TỪNG Ổ ĐÍCH)
        for dst_root in destination_roots {
            let dst_file = dst_root.join(rel_path);
            on_progress(TransferProgressEvent {
                job_id: job_id.to_string(),
                stage: "Verifying".to_string(),
                current_file: rel_str.clone(),
                bytes_processed: bytes_copied_so_far,
                total_bytes,
                speed_mbps: calculate_speed_mbps(bytes_copied_so_far, start_time),
                eta_seconds: calculate_eta(bytes_copied_so_far, total_bytes, start_time),
                message: format!("Đang kiểm tra vật lý (F_NOCACHE): {}", dst_file.display()),
            });

            let dst_hash = compute_cold_file_hash(&dst_file, algo)?;
            if dst_hash != source_hash {
                bail!(
                    "LỖI MÃ BĂM NGHIÊM TRỌNG tại file [{}]! Nguồn: {} != Đích: {}",
                    dst_file.display(),
                    source_hash,
                    dst_hash
                );
            }
            preserve_camera_timestamps(src_full, &dst_file)?;
        }

        let src_meta = std::fs::metadata(src_full)?;
        let mtime_utc: DateTime<Utc> = src_meta.modified()?.into();

        verified_records.push(VerifiedFileRecord {
            relative_path: rel_str,
            size_bytes: *file_size,
            hash_hex: source_hash,
            algorithm: algo,
            file_mtime_utc: mtime_utc,
            verified_at_utc: Utc::now(),
        });
    }

    Ok(verified_records)
}

#[allow(clippy::too_many_arguments)]
fn copy_single_file_multi_writer<F>(
    job_id: &str,
    src_full: &Path,
    rel_path: &Path,
    destination_roots: &[PathBuf],
    algo: HashAlgorithm,
    buffer: &mut [u8],
    bytes_copied_so_far: &mut u64,
    total_bytes: u64,
    start_time: Instant,
    on_progress: &mut F,
) -> Result<String>
where
    F: FnMut(TransferProgressEvent),
{
    let mut reader = open_uncached_reader(src_full)?;
    let mut writers: Vec< std::fs::File > = destination_roots
        .iter()
        .map(|root| create_uncached_writer(&root.join(rel_path)))
        .collect::<Result<Vec<_>>>()?;

    let mut hasher = StreamHasher::new(algo);
    let rel_str = rel_path.to_string_lossy().to_string();

    loop {
        let n = reader.read(buffer)?;
        if n == 0 {
            break;
        }
        let chunk = &buffer[..n];

        // 1. Tính mã băm ngay trên RAM
        hasher.update(chunk);

        // 2. Ghi đồng thời xuống tất cả các ổ đích trong tầng hiện tại
        for writer in &mut writers {
            writer.write_all(chunk)?;
        }

        *bytes_copied_so_far += n as u64;
        let speed = calculate_speed_mbps(*bytes_copied_so_far, start_time);
        let eta = calculate_eta(*bytes_copied_so_far, total_bytes, start_time);

        on_progress(TransferProgressEvent {
            job_id: job_id.to_string(),
            stage: "Copying".to_string(),
            current_file: rel_str.clone(),
            bytes_processed: *bytes_copied_so_far,
            total_bytes,
            speed_mbps: speed,
            eta_seconds: eta,
            message: format!("Đang chép: {}", rel_str),
        });
    }

    // Ép toàn bộ các ổ đích xả sạch cache phần cứng xuống NAND/Platter trước khi Verify
    for writer in &writers {
        darwin_full_fsync(writer)?;
    }

    Ok(hasher.finalize_hex())
}

fn calculate_speed_mbps(bytes: u64, start: Instant) -> f64 {
    let elapsed = start.elapsed().as_secs_f64();
    if elapsed <= 0.001 {
        0.0
    } else {
        (bytes as f64 / (1024.0 * 1024.0)) / elapsed
    }
}

fn calculate_eta(done: u64, total: u64, start: Instant) -> u64 {
    if done == 0 || done >= total {
        return 0;
    }
    let elapsed = start.elapsed().as_secs_f64();
    let rate = done as f64 / elapsed;
    if rate <= 1.0 {
        0
    } else {
        ((total - done) as f64 / rate) as u64
    }
}
```

---

### 4.5. Bộ Điều Phối Thác Đổ (`src-tauri/src/engine/cascading.rs`)

Thực thi chuẩn **2A + 2B**:
* **Chặng 1 (Tier 1 - Primary Fast SSDs):** Chép trực tiếp từ Thẻ Nhớ ra 1 hoặc nhiều ổ SSD tốc độ cao, Verify và ghi file `.mhl`. Ngay khi xong Chặng 1, phát tín hiệu **`SafeToEject`** để bạn rút thẻ nhớ trả cho đoàn quay ngay lập tức.
* **Chặng 2 (Tier 2 - Cascading Backup HDDs):** Tự động lấy ổ SSD đầu tiên của Chặng 1 làm nguồn mới để chép tuần tự sang các ổ HDD lưu trữ dung lượng lớn mà không làm nghẽn đầu đọc từ tính của HDD.

```rust
use crate::engine::copier::{
    copy_and_verify_multi_destinations, TransferProgressEvent, VerifiedFileRecord,
};
use crate::engine::hasher::HashAlgorithm;
use crate::postprocess::mhl_writer::write_mhl_file;
use crate::postprocess::preset_path::{build_roll_relative_path, RollPresetConfig};
use anyhow::{bail, Result};
use std::path::PathBuf;

pub struct CascadingJobSpec {
    pub job_id: String,
    pub source_card_path: PathBuf,
    pub primary_ssd_roots: Vec<PathBuf>,   // Mục 2A: Các ổ SSD chép song song tốc độ cao
    pub cascade_hdd_roots: Vec<PathBuf>,   // Mục 2B: Các ổ HDD chép thác đổ tuần tự từ SSD
    pub preset: RollPresetConfig,
    pub algorithm: HashAlgorithm,
}

pub fn execute_tiered_cascading_offload<F>(
    spec: CascadingJobSpec,
    mut on_progress: F,
) -> Result<(Vec<VerifiedFileRecord>, Vec<PathBuf>)>
where
    F: FnMut(TransferProgressEvent),
{
    if spec.primary_ssd_roots.is_empty() {
        bail!("Cần chọn ít nhất 1 ổ đích chính (Primary SSD) để bắt đầu Offload!");
    }

    let roll_subpath = build_roll_relative_path(&spec.preset);

    // Tạo danh sách đường dẫn đích đầy đủ cho Tier 1 (Primary SSDs)
    let tier1_targets: Vec<PathBuf> = spec
        .primary_ssd_roots
        .iter()
        .map(|root| root.join(&roll_subpath))
        .collect();

    // --- CHẶNG 1: THẺ NHỚ -> CÁC Ổ SSD CHÍNH (SONG SONG) ---
    let verified_records = copy_and_verify_multi_destinations(
        &spec.job_id,
        &spec.source_card_path,
        &tier1_targets,
        spec.algorithm,
        &mut on_progress,
    )?;

    // Xuất file chuẩn .mhl ngay trong thư mục Roll của từng ổ SSD
    for ssd_roll_dir in &tier1_targets {
        write_mhl_file(ssd_roll_dir, &spec.preset.roll_id, &verified_records)?;
    }

    // PHÁT TÍN HIỆU GIẢI PHÓNG THẺ NHỚ NGAY LẬP TỨC!
    on_progress(TransferProgressEvent {
        job_id: spec.job_id.clone(),
        stage: "SafeToEject".to_string(),
        current_file: spec.preset.roll_id.clone(),
        bytes_processed: verified_records.iter().map(|r| r.size_bytes).sum(),
        total_bytes: verified_records.iter().map(|r| r.size_bytes).sum(),
        speed_mbps: 0.0,
        eta_seconds: 0,
        message: format!(
            "ĐÃ VERIFY XONG TRÊN SSD! Bạn có thể rút thẻ [{}] an toàn ngay bây giờ.",
            spec.preset.roll_id
        ),
    });

    // --- CHẶNG 2: THÁC ĐỔ TỪ SSD CHÍNH -> TỪNG Ổ HDD BACKUP (TUẦN TỰ CHỐNG THRASHING) ---
    let primary_verified_source = &tier1_targets[0];
    let mut all_completed_roll_dirs = tier1_targets.clone();

    for hdd_root in &spec.cascade_hdd_roots {
        let hdd_roll_dir = hdd_root.join(&roll_subpath);

        on_progress(TransferProgressEvent {
            job_id: spec.job_id.clone(),
            stage: "Cascading".to_string(),
            current_file: hdd_roll_dir.to_string_lossy().to_string(),
            bytes_processed: 0,
            total_bytes: verified_records.iter().map(|r| r.size_bytes).sum(),
            speed_mbps: 0.0,
            eta_seconds: 0,
            message: format!(
                "Đang chạy Cascading từ SSD sang ổ lưu trữ HDD: {}",
                hdd_root.display()
            ),
        });

        let hdd_records = copy_and_verify_multi_destinations(
            &spec.job_id,
            primary_verified_source,
            std::slice::from_ref(&hdd_roll_dir),
            spec.algorithm,
            &mut on_progress,
        )?;

        write_mhl_file(&hdd_roll_dir, &spec.preset.roll_id, &hdd_records)?;
        all_completed_roll_dirs.push(hdd_roll_dir);
    }

    Ok((verified_records, all_completed_roll_dirs))
}
```

---

## 5. HẬU KỲ: CÂY THƯ MỤC DIT, CHUẨN `.MHL` & BÁO CÁO FOOLCAT

### 5.1. Tự động Tạo Cây Thư Mục & Tăng Số Thẻ (`src-tauri/src/postprocess/preset_path.rs`)

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollPresetConfig {
    pub shoot_date_yyyymmdd: String, // VD: "20261025"
    pub project_name: String,        // VD: "TVC_BrandX"
    pub media_category: String,      // "01_FOOTAGE" hoặc "02_AUDIO"
    pub shoot_day: String,           // VD: "Day01"
    pub camera_id: String,           // VD: "CAM_A_FX6"
    pub roll_id: String,             // VD: "A001"
    pub dit_name: String,            // Tên kỹ thuật viên DIT hiển thị trên MHL & Report
}

/// Tạo đường dẫn tương đối chuẩn DIT:
/// {YYYYMMDD}_{ProjectName}/01_FOOTAGE/{ShootDay}/{CameraID}/{RollID}
pub fn build_roll_relative_path(cfg: &RollPresetConfig) -> PathBuf {
    let root_project = format!(
        "{}_{}",
        sanitize_token(&cfg.shoot_date_yyyymmdd),
        sanitize_token(&cfg.project_name)
    );
    PathBuf::from(root_project)
        .join(sanitize_token(&cfg.media_category))
        .join(sanitize_token(&cfg.shoot_day))
        .join(sanitize_token(&cfg.camera_id))
        .join(sanitize_token(&cfg.roll_id))
}

/// Tự động tăng mã thẻ: "A001" -> "A002", "B019" -> "B020"
pub fn increment_roll_id(current_roll: &str) -> String {
    let prefix: String = current_roll
        .chars()
        .take_while(|c| !c.is_ascii_digit())
        .collect();
    let digits: String = current_roll
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .collect();

    if let Ok(num) = digits.parse::<u32>() {
        let width = digits.len().max(3);
        format!("{}{:0width$}", prefix, num + 1, width = width)
    } else {
        format!("{}_002", current_roll)
    }
}

fn sanitize_token(input: &str) -> String {
    input
        .trim()
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "_")
}
```

---

### 5.2. Xuất File Chuẩn Quốc Tế `.mhl` (`src-tauri/src/postprocess/mhl_writer.rs`)

Tương thích 100% với **Pomfort SealVerify**, **Silverstack**, **DaVinci Resolve** và **OffShoot**.

```rust
use crate::engine::copier::VerifiedFileRecord;
use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_mhl_file(
    roll_dir: &Path,
    roll_id: &str,
    records: &[VerifiedFileRecord],
) -> Result<PathBuf> {
    let now = Utc::now();
    let timestamp_slug = now.format("%Y-%m-%d_%H%M%S").to_string();
    let mhl_filename = format!("{}_{}.mhl", roll_id, timestamp_slug);
    let mhl_path = roll_dir.join(mhl_filename);

    let host_name = sysinfo::System::host_name().unwrap_or_else(|| "MacBook-Pro-DIT".to_string());
    let start_iso = now.to_rfc3339_opts(SecondsFormat::Secs, true);

    let mut xml = String::with_capacity(4096 + records.len() * 350);
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<hashlist version=\"1.1\">\n");
    xml.push_str("  <creatorinfo>\n");
    xml.push_str("    <name>OffShoot Pro Personal (macOS Darwin Engine)</name>\n");
    xml.push_str("    <version>1.0.0</version>\n");
    xml.push_str(&format!("    <hostname>{}</hostname>\n", escape_xml(&host_name)));
    xml.push_str("    <tool>OffShoot Pro Personal F_NOCACHE Engine</tool>\n");
    xml.push_str(&format!("    <startdate>{}</startdate>\n", start_iso));
    xml.push_str(&format!("    <finishdate>{}</finishdate>\n", start_iso));
    xml.push_str("  </creatorinfo>\n");

    for rec in records {
        // Không đưa chính file .mhl vào trong danh sách hash
        if rec.relative_path.ends_with(".mhl") {
            continue;
        }
        let tag = rec.algorithm.mhl_tag_name();
        let mtime_iso = rec.file_mtime_utc.to_rfc3339_opts(SecondsFormat::Secs, true);
        let hashdate_iso = rec.verified_at_utc.to_rfc3339_opts(SecondsFormat::Secs, true);

        xml.push_str("  <hash>\n");
        xml.push_str(&format!("    <file>{}</file>\n", escape_xml(&rec.relative_path)));
        xml.push_str(&format!("    <size>{}</size>\n", rec.size_bytes));
        xml.push_str(&format!("    <lastmodificationdate>{}</lastmodificationdate>\n", mtime_iso));
        xml.push_str(&format!("    <{}>{}</{}>\n", tag, rec.hash_hex, tag));
        xml.push_str(&format!("    <hashdate>{}</hashdate>\n", hashdate_iso));
        xml.push_str("  </hash>\n");
    }

    xml.push_str("</hashlist>\n");

    let mut file = File::create(&mhl_path)?;
    file.write_all(xml.as_bytes())?;
    file.sync_all()?;

    Ok(mhl_path)
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
```

---

### 5.3. Xuất Báo Cáo Bàn Giao Kiểu FoolCat (`src-tauri/src/postprocess/report_gen.rs`)

Tự động quét các clip video trong thẻ nhớ (`.MP4`, `.MOV`, `.MXF`, `.BRAW`, `.R3D`), gọi `ffprobe` lấy thông số kỹ thuật (Codec, Độ phân giải, FPS, Thời lượng, Timecode) và gọi `ffmpeg` trích xuất **3 ảnh Thumbnail (Đầu 10% - Giữa 50% - Cuối 90%)** nhúng dạng Base64 trực tiếp vào 1 file HTML/PDF độc lập (gửi qua email/Zalo cho Producer mở xem trên mọi thiết bị mà không sợ mất ảnh).

```rust
use crate::engine::copier::VerifiedFileRecord;
use crate::postprocess::preset_path::RollPresetConfig;
use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mxf", "braw", "r3d", "ari", "mts", "mkv"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoClipMetadata {
    pub file_name: String,
    pub relative_path: String,
    pub size_mb: f64,
    pub hash_hex: String,
    pub codec: String,
    pub resolution: String,
    pub fps: String,
    pub duration_sec: f64,
    pub timecode: String,
    pub thumbnails_base64: Vec<String>,
}

pub fn generate_foolcat_html_report(
    roll_dir: &Path,
    report_output_dir: &Path,
    preset: &RollPresetConfig,
    records: &[VerifiedFileRecord],
) -> Result<PathBuf> {
    fs::create_dir_all(report_output_dir)?;

    let mut clips = Vec::new();
    let mut total_bytes = 0u64;
    let mut total_duration_sec = 0.0f64;

    for rec in records {
        total_bytes += rec.size_bytes;
        let full_path = roll_dir.join(&rec.relative_path);
        let ext = full_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            let meta = probe_clip_with_ffmpeg(&full_path, rec);
            total_duration_sec += meta.duration_sec;
            clips.push(meta);
        }
    }

    let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let report_filename = format!(
        "{}_{}_{}_{}_CameraReport.html",
        preset.shoot_date_yyyymmdd, preset.project_name, preset.shoot_day, preset.roll_id
    );
    let report_path = report_output_dir.join(report_filename);

    let html = render_report_template(preset, records.len(), total_gb, total_duration_sec, &clips);
    fs::write(&report_path, html)?;

    Ok(report_path)
}

fn probe_clip_with_ffmpeg(video_path: &Path, rec: &VerifiedFileRecord) -> VideoClipMetadata {
    let file_name = video_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string();

    // Gọi ffprobe lấy JSON thông tin stream
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(video_path)
        .output();

    let mut codec = "RAW/ProRes".to_string();
    let mut resolution = "N/A".to_string();
    let mut fps = "N/A".to_string();
    let mut duration_sec = 0.0f64;
    let mut timecode = "00:00:00:00".to_string();

    if let Ok(out) = output {
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(dur_str) = json["format"]["duration"].as_str() {
                duration_sec = dur_str.parse::<f64>().unwrap_or(0.0);
            }
            if let Some(tc) = json["format"]["tags"]["timecode"].as_str() {
                timecode = tc.to_string();
            }
            if let Some(streams) = json["streams"].as_array() {
                for stream in streams {
                    if stream["codec_type"].as_str() == Some("video") {
                        codec = stream["codec_name"]
                            .as_str()
                            .unwrap_or("video")
                            .to_uppercase();
                        let w = stream["width"].as_i64().unwrap_or(0);
                        let h = stream["height"].as_i64().unwrap_or(0);
                        if w > 0 && h > 0 {
                            resolution = format!("{}x{}", w, h);
                        }
                        if let Some(fr) = stream["r_frame_rate"].as_str() {
                            fps = parse_frame_rate(fr);
                        }
                        if let Some(tc) = stream["tags"]["timecode"].as_str() {
                            timecode = tc.to_string();
                        }
                        break;
                    }
                }
            }
        }
    }

    // Trích xuất 3 ảnh Thumbnail ở mốc 10%, 50%, 90% thời lượng clip
    let mut thumbnails_base64 = Vec::new();
    if duration_sec > 0.2 {
        for ratio in [0.10, 0.50, 0.90] {
            let timestamp = duration_sec * ratio;
            if let Some(b64) = extract_single_thumbnail_base64(video_path, timestamp) {
                thumbnails_base64.push(b64);
            }
        }
    }

    VideoClipMetadata {
        file_name,
        relative_path: rec.relative_path.clone(),
        size_mb: rec.size_bytes as f64 / (1024.0 * 1024.0),
        hash_hex: rec.hash_hex.clone(),
        codec,
        resolution,
        fps,
        duration_sec,
        timecode,
        thumbnails_base64,
    }
}

fn extract_single_thumbnail_base64(video_path: &Path, timestamp_sec: f64) -> Option<String> {
    let out = Command::new("ffmpeg")
        .args([
            "-ss",
            &format!("{:.2}", timestamp_sec),
            "-i",
        ])
        .arg(video_path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=320:-1",
            "-q:v",
            "5",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .output()
        .ok()?;

    if out.stdout.is_empty() {
        return None;
    }
    Some(base64_encode(&out.stdout))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn parse_frame_rate(raw: &str) -> String {
    if let Some((num, den)) = raw.split_once('/') {
        let n = num.parse::<f64>().unwrap_or(0.0);
        let d = den.parse::<f64>().unwrap_or(1.0);
        if d > 0.0 {
            return format!("{:.2} fps", n / d);
        }
    }
    format!("{} fps", raw)
}

fn format_duration_hms(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

fn render_report_template(
    preset: &RollPresetConfig,
    total_files: usize,
    total_gb: f64,
    total_duration_sec: f64,
    clips: &[VideoClipMetadata],
) -> String {
    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let total_dur_str = format_duration_hms(total_duration_sec);

    let mut clip_rows = String::new();
    for clip in clips {
        let mut thumbs_html = String::new();
        if clip.thumbnails_base64.is_empty() {
            thumbs_html.push_str("<div class='thumb-placeholder'>No Preview Available</div>");
        } else {
            for b64 in &clip.thumbnails_base64 {
                thumbs_html.push_str(&format!(
                    "<img src='data:image/jpeg;base64,{}' class='thumb-img' alt='thumb' />",
                    b64
                ));
            }
        }

        clip_rows.push_str(&format!(
            r#"
            <div class="clip-card">
                <div class="thumbs-strip">{thumbs_html}</div>
                <div class="clip-details">
                    <div class="clip-header">
                        <span class="clip-title">{file_name}</span>
                        <span class="badge-verified">VERIFIED: {hash_hex}</span>
                    </div>
                    <div class="clip-meta-grid">
                        <div><strong>Codec:</strong> {codec}</div>
                        <div><strong>Resolution:</strong> {resolution}</div>
                        <div><strong>Frame Rate:</strong> {fps}</div>
                        <div><strong>Duration:</strong> {dur}</div>
                        <div><strong>Start TC:</strong> {tc}</div>
                        <div><strong>Size:</strong> {size:.1} MB</div>
                    </div>
                    <div class="clip-path">{rel_path}</div>
                </div>
            </div>
            "#,
            thumbs_html = thumbs_html,
            file_name = clip.file_name,
            hash_hex = clip.hash_hex,
            codec = clip.codec,
            resolution = clip.resolution,
            fps = clip.fps,
            dur = format_duration_hms(clip.duration_sec),
            tc = clip.timecode,
            size = clip.size_mb,
            rel_path = clip.relative_path
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="vi">
<head>
<meta charset="UTF-8" />
<title>Camera Offload Report - {project} - {roll}</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", sans-serif; background: #0f1115; color: #e5e7eb; margin: 0; padding: 32px; }}
  .container {{ max-width: 1120px; margin: 0 auto; }}
  .header {{ display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #272a34; padding-bottom: 20px; margin-bottom: 24px; }}
  .brand {{ font-size: 24px; font-weight: 800; color: #f9fafb; letter-spacing: 0.5px; }}
  .status-pill {{ background: #064e3b; color: #34d399; border: 1px solid #059669; padding: 8px 16px; border-radius: 999px; font-weight: 700; font-size: 13px; }}
  .summary-grid {{ display: grid; grid-template-columns: repeat(6, 1fr); gap: 12px; margin-bottom: 28px; }}
  .summary-box {{ background: #181b22; border: 1px solid #272a34; padding: 14px; border-radius: 10px; }}
  .summary-label {{ font-size: 11px; text-transform: uppercase; color: #9ca3af; margin-bottom: 6px; }}
  .summary-val {{ font-size: 16px; font-weight: 700; color: #ffffff; }}
  .clip-card {{ display: grid; grid-template-columns: 390px 1fr; gap: 20px; background: #161920; border: 1px solid #262936; border-radius: 12px; padding: 14px; margin-bottom: 14px; page-break-inside: avoid; }}
  .thumbs-strip {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 6px; align-items: center; }}
  .thumb-img {{ width: 100%; height: 74px; object-fit: cover; border-radius: 6px; background: #000; }}
  .thumb-placeholder {{ grid-column: span 3; height: 74px; display: flex; align-items: center; justify-content: center; background: #1f2430; color: #6b7280; font-size: 12px; border-radius: 6px; }}
  .clip-header {{ display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px; }}
  .clip-title {{ font-size: 16px; font-weight: 700; color: #fff; }}
  .badge-verified {{ font-family: ui-monospace, SFMono-Regular, monospace; font-size: 12px; background: #111827; color: #60a5fa; padding: 4px 10px; border-radius: 6px; border: 1px solid #1e3a8a; }}
  .clip-meta-grid {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; font-size: 13px; color: #d1d5db; margin-bottom: 8px; }}
  .clip-path {{ font-family: monospace; font-size: 11px; color: #6b7280; }}
  @media print {{
    body {{ background: #fff; color: #111; padding: 0; }}
    .summary-box, .clip-card {{ background: #f9fafb; border-color: #d1d5db; color: #111; }}
    .brand, .summary-val, .clip-title {{ color: #000; }}
    .clip-meta-grid {{ color: #374151; }}
  }}
</style>
</head>
<body>
  <div class="container">
    <div class="header">
      <div>
        <div class="brand">CAMERA OFFLOAD & VERIFICATION REPORT</div>
        <div style="color:#9ca3af; font-size:13px; margin-top:4px;">Generated at {now_str} • DIT: {dit} • macOS F_NOCACHE Verified</div>
      </div>
      <div class="status-pill">100% VERIFIED • ZERO ERRORS</div>
    </div>

    <div class="summary-grid">
      <div class="summary-box"><div class="summary-label">Project</div><div class="summary-val">{project}</div></div>
      <div class="summary-box"><div class="summary-label">Shoot Day</div><div class="summary-val">{day}</div></div>
      <div class="summary-box"><div class="summary-label">Camera / Roll</div><div class="summary-val">{cam} / {roll}</div></div>
      <div class="summary-box"><div class="summary-label">Total Files / Clips</div><div class="summary-val">{total_files} files ({clip_count} clips)</div></div>
      <div class="summary-box"><div class="summary-label">Total Size</div><div class="summary-val">{total_gb:.2} GB</div></div>
      <div class="summary-box"><div class="summary-label">Total Duration</div><div class="summary-val">{total_dur_str}</div></div>
    </div>

    {clip_rows}
  </div>
</body>
</html>"#,
        project = preset.project_name,
        roll = preset.roll_id,
        now_str = now_str,
        dit = preset.dit_name,
        day = preset.shoot_day,
        cam = preset.camera_id,
        total_files = total_files,
        clip_count = clips.len(),
        total_gb = total_gb,
        total_dur_str = total_dur_str,
        clip_rows = clip_rows
    )
}
```

---

### 5.4. Kết nối Tauri Commands (`src-tauri/src/lib.rs` & `src-tauri/src/main.rs`)

Tạo file `src-tauri/src/lib.rs`:
```rust
pub mod engine;
pub mod postprocess;

use crate::engine::cascading::{execute_tiered_cascading_offload, CascadingJobSpec};
use crate::engine::hasher::HashAlgorithm;
use crate::postprocess::preset_path::{increment_roll_id, RollPresetConfig};
use crate::postprocess::report_gen::generate_foolcat_html_report;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use sysinfo::Disks;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountedVolumeInfo {
    pub name: String,
    pub mount_point: String,
    pub available_gb: f64,
    pub total_gb: f64,
    pub is_removable: bool,
    pub kind_label: String, // "SSD / NVMe" hoặc "HDD / Rotational"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartOffloadRequest {
    pub source_card_path: String,
    pub primary_ssd_paths: Vec<String>,
    pub cascade_hdd_paths: Vec<String>,
    pub preset: RollPresetConfig,
    pub algorithm: HashAlgorithm,
    pub generate_report: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffloadSummaryResponse {
    pub job_id: String,
    pub verified_files_count: usize,
    pub next_suggested_roll: String,
    pub report_html_path: Option<String>,
}

/// Quét toàn bộ thẻ nhớ và ổ cứng gắn ngoài trên macOS (/Volumes)
#[tauri::command]
pub fn list_macos_volumes() -> Vec<MountedVolumeInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut result = Vec::new();

    for disk in &disks {
        let mount_str = disk.mount_point().to_string_lossy().to_string();
        // Chỉ hiển thị các ổ gắn ngoài trong /Volumes hoặc ổ Data chính
        if mount_str == "/" || mount_str.starts_with("/Volumes/") {
            let total_gb = disk.total_space() as f64 / (1024.0 * 1024.0 * 1024.0);
            let avail_gb = disk.available_space() as f64 / (1024.0 * 1024.0 * 1024.0);
            let kind_label = match disk.kind() {
                sysinfo::DiskKind::SSD => "SSD / NVMe".to_string(),
                sysinfo::DiskKind::HDD => "HDD (Rotational)".to_string(),
                _ => "External Media".to_string(),
            };

            result.push(MountedVolumeInfo {
                name: disk.name().to_string_lossy().to_string(),
                mount_point: mount_str,
                available_gb: (avail_gb * 10.0).round() / 10.0,
                total_gb: (total_gb * 10.0).round() / 10.0,
                is_removable: disk.is_removable(),
                kind_label,
            });
        }
    }
    result
}

/// Khởi chạy tiến trình Offload (2A Song song + 2B Thác đổ + Xuất .mhl + Xuất FoolCat Report)
#[tauri::command]
pub async fn start_offload_job(
    app: AppHandle,
    req: StartOffloadRequest,
) -> Result<OffloadSummaryResponse, String> {
    let job_id = Uuid::new_v4().to_string();
    let job_id_clone = job_id.clone();

    let spec = CascadingJobSpec {
        job_id: job_id.clone(),
        source_card_path: PathBuf::from(&req.source_card_path),
        primary_ssd_roots: req.primary_ssd_paths.iter().map(PathBuf::from).collect(),
        cascade_hdd_roots: req.cascade_hdd_paths.iter().map(PathBuf::from).collect(),
        preset: req.preset.clone(),
        algorithm: req.algorithm,
    };

    let preset_for_report = req.preset.clone();
    let should_report = req.generate_report;
    let first_ssd_root = PathBuf::from(&req.primary_ssd_paths[0]);

    let outcome = tokio::task::spawn_blocking(move || {
        let (records, completed_dirs) = execute_tiered_cascading_offload(spec, |progress| {
            let _ = app.emit("offload-progress", progress);
        })?;

        let mut report_path_str = None;
        if should_report && !completed_dirs.is_empty() {
            let project_folder = format!(
                "{}_{}",
                preset_for_report.shoot_date_yyyymmdd, preset_for_report.project_name
            );
            let report_dir = first_ssd_root.join(project_folder).join("03_DIT_REPORTS");
            let report_file = generate_foolcat_html_report(
                &completed_dirs[0],
                &report_dir,
                &preset_for_report,
                &records,
            )?;
            report_path_str = Some(report_file.to_string_lossy().to_string());
        }

        Ok::<(usize, Option<String>), anyhow::Error>((records.len(), report_path_str))
    })
    .await
    .map_err(|e| format!("Lỗi luồng thực thi: {}", e))?
    .map_err(|e| e.to_string())?;

    let next_roll = increment_roll_id(&req.preset.roll_id);

    Ok(OffloadSummaryResponse {
        job_id: job_id_clone,
        verified_files_count: outcome.0,
        next_suggested_roll: next_roll,
        report_html_path: outcome.1,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            list_macos_volumes,
            start_offload_job
        ])
        .run(tauri::generate_context!())
        .expect("Lỗi khi khởi chạy ứng dụng Tauri macOS");
}
```

Cập nhật file `src-tauri/src/main.rs`:
```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    offshoot_pro_lib::run();
}
```

---

## 6. GIAO DIỆN NGƯỜI DÙNG DIT DARK-MODE (`src/App.tsx`)

Thay thế nội dung file `src/App.tsx` bằng giao diện điều khiển 3 cột trực quan dưới đây:

```tsx
import React, { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  HardDrive,
  FolderOpen,
  ShieldCheck,
  Play,
  RefreshCw,
  CheckCircle2,
  FileText,
  Layers,
  AlertTriangle,
} from "lucide-react";

interface MountedVolumeInfo {
  name: string;
  mount_point: string;
  available_gb: number;
  total_gb: number;
  is_removable: boolean;
  kind_label: string;
}

interface TransferProgressEvent {
  job_id: string;
  stage: string;
  current_file: string;
  bytes_processed: number;
  total_bytes: number;
  speed_mbps: number;
  eta_seconds: number;
  message: string;
}

interface OffloadSummaryResponse {
  job_id: string;
  verified_files_count: number;
  next_suggested_roll: string;
  report_html_path: string | null;
}

export default function App() {
  const [volumes, setVolumes] = useState<MountedVolumeInfo[]>([]);
  const [sourcePath, setSourcePath] = useState<string>("");
  const [primarySsdPaths, setPrimarySsdPaths] = useState<string[]>([]);
  const [cascadeHddPaths, setCascadeHddPaths] = useState<string[]>([]);

  // Cấu hình Preset chuẩn DIT (Mục 3A)
  const [shootDate, setShootDate] = useState<string>("20261025");
  const [projectName, setProjectName] = useState<string>("TVC_BrandX");
  const [mediaCategory, setMediaCategory] = useState<string>("01_FOOTAGE");
  const [shootDay, setShootDay] = useState<string>("Day01");
  const [cameraId, setCameraId] = useState<string>("CAM_A_FX6");
  const [rollId, setRollId] = useState<string>("A001");
  const [ditName, setDitName] = useState<string>("DIT Master");

  // Cấu hình Hash & Report (Mục 4A, 4B, 5A)
  const [algorithm, setAlgorithm] = useState<"xxh64" | "xxh3" | "md5" | "sha256">("xxh64");
  const [generateReport, setGenerateReport] = useState<boolean>(true);

  // Trạng thái truyền tải thời gian thực
  const [isRunning, setIsRunning] = useState<boolean>(false);
  const [safeToEjectBanner, setSafeToEjectBanner] = useState<string | null>(null);
  const [progress, setProgress] = useState<TransferProgressEvent | null>(null);
  const [lastSummary, setLastSummary] = useState<OffloadSummaryResponse | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const refreshVolumes = async () => {
    try {
      const list = await invoke<MountedVolumeInfo[]>("list_macos_volumes");
      setVolumes(list);
    } catch (err) {
      console.error("Failed to list volumes:", err);
    }
  };

  useEffect(() => {
    refreshVolumes();
    const unlistenPromise = listen<TransferProgressEvent>("offload-progress", (event) => {
      setProgress(event.payload);
      if (event.payload.stage === "SafeToEject") {
        setSafeToEjectBanner(event.payload.message);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  const togglePrimarySsd = (mount: string) => {
    setPrimarySsdPaths((prev) =>
      prev.includes(mount) ? prev.filter((p) => p !== mount) : [...prev, mount]
    );
    setCascadeHddPaths((prev) => prev.filter((p) => p !== mount));
  };

  const toggleCascadeHdd = (mount: string) => {
    setCascadeHddPaths((prev) =>
      prev.includes(mount) ? prev.filter((p) => p !== mount) : [...prev, mount]
    );
    setPrimarySsdPaths((prev) => prev.filter((p) => p !== mount));
  };

  const handleStartOffload = async () => {
    if (!sourcePath || primarySsdPaths.length === 0) {
      setErrorMsg("Vui lòng chọn Thẻ Nhớ Nguồn và ít nhất 1 ổ SSD Đích Chính (Tier 1)!");
      return;
    }
    setErrorMsg(null);
    setSafeToEjectBanner(null);
    setLastSummary(null);
    setIsRunning(true);

    try {
      const summary = await invoke<OffloadSummaryResponse>("start_offload_job", {
        req: {
          source_card_path: sourcePath,
          primary_ssd_paths: primarySsdPaths,
          cascade_hdd_paths: cascadeHddPaths,
          preset: {
            shoot_date_yyyymmdd: shootDate,
            project_name: projectName,
            media_category: mediaCategory,
            shoot_day: shootDay,
            camera_id: cameraId,
            roll_id: rollId,
            dit_name: ditName,
          },
          algorithm,
          generate_report: generateReport,
        },
      });
      setLastSummary(summary);
      setRollId(summary.next_suggested_roll); // Tự động nhảy A001 -> A002
      refreshVolumes();
    } catch (err: any) {
      setErrorMsg(String(err));
    } finally {
      setIsRunning(false);
    }
  };

  const percent =
    progress && progress.total_bytes > 0
      ? Math.min(100, Math.round((progress.bytes_processed / progress.total_bytes) * 100))
      : 0;

  const previewRelativePath = `${shootDate}_${projectName}/${mediaCategory}/${shootDay}/${cameraId}/${rollId}/`;

  return (
    <div className="min-h-screen bg-[#0b0d12] text-gray-100 p-6 font-sans select-none">
      {/* HEADER */}
      <header className="flex items-center justify-between border-b border-gray-800 pb-4 mb-6">
        <div className="flex items-center gap-3">
          <div className="h-10 w-10 rounded-xl bg-blue-600/20 border border-blue-500/40 flex items-center justify-center">
            <ShieldCheck className="w-6 h-6 text-blue-400" />
          </div>
          <div>
            <h1 className="text-xl font-bold tracking-tight text-white">
              OFFSHOOT PRO <span className="text-xs font-mono text-blue-400 ml-2">macOS F_NOCACHE ENGINE</span>
            </h1>
            <p className="text-xs text-gray-400">
              Single-Read Multi-Write • Cascading SSD {"->"} HDD • Standard .MHL • FoolCat Camera Report
            </p>
          </div>
        </div>

        <button
          onClick={refreshVolumes}
          className="flex items-center gap-2 px-3 py-2 rounded-lg bg-gray-800/80 hover:bg-gray-700 text-xs font-medium border border-gray-700 transition"
        >
          <RefreshCw className="w-4 h-4" /> Quét lại ổ đĩa (/Volumes)
        </button>
      </header>

      {/* THÔNG BÁO RÚT THẺ NHỚ AN TOÀN (KHI XONG TIER 1 SSD) */}
      {safeToEjectBanner && (
        <div className="mb-5 p-4 rounded-xl bg-emerald-950/80 border border-emerald-500/60 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <CheckCircle2 className="w-6 h-6 text-emerald-400 shrink-0" />
            <div>
              <div className="text-sm font-bold text-emerald-300">
                AN TOÀN RÚT THẺ NHỚ! (SAFE TO EJECT CAMERA CARD)
              </div>
              <div className="text-xs text-emerald-200/90">{safeToEjectBanner}</div>
            </div>
          </div>
          <span className="text-xs font-mono bg-emerald-900/80 text-emerald-300 px-3 py-1 rounded-full">
            Tier 1 SSD Verified
          </span>
        </div>
      )}

      {/* BỐ CỤC 3 CỘT CHUẨN DIT */}
      <div className="grid grid-cols-12 gap-6">
        {/* CỘT 1: CHỌN THẺ NHỚ NGUỒN (SOURCES) */}
        <div className="col-span-4 bg-[#12161f] border border-gray-800 rounded-2xl p-4 flex flex-col justify-between">
          <div>
            <h2 className="text-xs font-bold uppercase tracking-wider text-gray-400 mb-3 flex items-center gap-2">
              <FolderOpen className="w-4 h-4 text-blue-400" /> 1. Thẻ Nhớ Nguồn (Source Card)
            </h2>

            <div className="space-y-2 max-h-64 overflow-y-auto pr-1">
              {volumes.map((vol) => {
                const selected = sourcePath === vol.mount_point;
                return (
                  <div
                    key={vol.mount_point}
                    onClick={() => setSourcePath(vol.mount_point)}
                    className={`p-3 rounded-xl border cursor-pointer transition ${
                      selected
                        ? "bg-blue-950/50 border-blue-500"
                        : "bg-[#181d29] border-gray-800 hover:border-gray-700"
                    }`}
                  >
                    <div className="flex justify-between items-center">
                      <span className="font-semibold text-sm text-white">{vol.name || vol.mount_point}</span>
                      <span className="text-[11px] font-mono px-2 py-0.5 rounded bg-gray-800 text-gray-300">
                        {vol.kind_label}
                      </span>
                    </div>
                    <div className="text-xs text-gray-400 font-mono mt-1">{vol.mount_point}</div>
                    <div className="text-[11px] text-gray-400 mt-1">
                      Trống: {vol.available_gb} GB / {vol.total_gb} GB
                    </div>
                  </div>
                );
              })}
            </div>

            <div className="mt-4">
              <label className="block text-xs text-gray-400 mb-1">
                Hoặc nhập đường dẫn thư mục thẻ bất kỳ:
              </label>
              <input
                type="text"
                value={sourcePath}
                onChange={(e) => setSourcePath(e.target.value)}
                placeholder="/Volumes/Untitled/PRIVATE"
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-xs font-mono text-white"
              />
            </div>
          </div>
        </div>

        {/* CỘT 2: PRESET ĐẶT TÊN THƯ MỤC & MÃ BĂM */}
        <div className="col-span-4 bg-[#12161f] border border-gray-800 rounded-2xl p-4">
          <h2 className="text-xs font-bold uppercase tracking-wider text-gray-400 mb-3 flex items-center gap-2">
            <Layers className="w-4 h-4 text-purple-400" /> 2. Cấu Trúc Thư Mục DIT & Hash
          </h2>

          <div className="grid grid-cols-2 gap-3 text-xs">
            <div>
              <label className="text-gray-400 block mb-1">Ngày quay (YYYYMMDD)</label>
              <input
                value={shootDate}
                onChange={(e) => setShootDate(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white font-mono"
              />
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Tên Dự Án (Project)</label>
              <input
                value={projectName}
                onChange={(e) => setProjectName(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white font-semibold"
              />
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Phân loại Media</label>
              <select
                value={mediaCategory}
                onChange={(e) => setMediaCategory(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white"
              >
                <option value="01_FOOTAGE">01_FOOTAGE</option>
                <option value="02_AUDIO">02_AUDIO</option>
              </select>
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Ngày sản xuất (Shoot Day)</label>
              <input
                value={shootDay}
                onChange={(e) => setShootDay(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white font-mono"
              />
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Máy Quay (Camera ID)</label>
              <input
                value={cameraId}
                onChange={(e) => setCameraId(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white font-mono"
              />
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Mã Thẻ (Roll ID - Tự tăng)</label>
              <input
                value={rollId}
                onChange={(e) => setRollId(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-blue-950/60 border border-blue-500 text-blue-300 font-mono font-bold"
              />
            </div>
          </div>

          {/* XEM TRƯỚC ĐƯỜNG DẪN ĐÍCH */}
          <div className="mt-3 p-2.5 rounded-lg bg-[#0b0d12] border border-gray-800">
            <div className="text-[10px] uppercase text-gray-500 mb-1">Đường dẫn tự động tạo trên các ổ đích:</div>
            <div className="text-xs font-mono text-emerald-400 break-all">{previewRelativePath}</div>
          </div>

          <div className="grid grid-cols-2 gap-3 mt-3 text-xs">
            <div>
              <label className="text-gray-400 block mb-1">Thuật toán Checksum</label>
              <select
                value={algorithm}
                onChange={(e) => setAlgorithm(e.target.value as any)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white font-mono"
              >
                <option value="xxh64">xxHash64 (Chuẩn OffShoot)</option>
                <option value="xxh3">XXH3-64 (Siêu tốc Apple Silicon)</option>
                <option value="md5">MD5 (Truyền thống)</option>
                <option value="sha256">SHA-256</option>
              </select>
            </div>
            <div>
              <label className="text-gray-400 block mb-1">Tên DIT (Ghi vào MHL & Report)</label>
              <input
                value={ditName}
                onChange={(e) => setDitName(e.target.value)}
                className="w-full px-3 py-2 rounded-lg bg-[#0b0d12] border border-gray-700 text-white"
              />
            </div>
          </div>

          <label className="flex items-center gap-2 mt-4 text-xs text-gray-300 cursor-pointer">
            <input
              type="checkbox"
              checked={generateReport}
              onChange={(e) => setGenerateReport(e.target.checked)}
              className="rounded border-gray-600"
            />
            <FileText className="w-4 h-4 text-amber-400" />
            Tự động xuất file .MHL + Báo cáo FoolCat (HTML/PDF kèm 3 Thumbnails/clip)
          </label>
        </div>

        {/* CỘT 3: CHỌN Ổ ĐÍCH SONG SONG (2A) & THÁC ĐỔ CASCADING (2B) */}
        <div className="col-span-4 bg-[#12161f] border border-gray-800 rounded-2xl p-4 flex flex-col justify-between">
          <div>
            <h2 className="text-xs font-bold uppercase tracking-wider text-gray-400 mb-3 flex items-center gap-2">
              <HardDrive className="w-4 h-4 text-emerald-400" /> 3. Ổ Đích: Song Song (2A) & Thác Đổ (2B)
            </h2>

            <div className="space-y-2 max-h-72 overflow-y-auto pr-1">
              {volumes
                .filter((v) => v.mount_point !== sourcePath)
                .map((vol) => {
                  const isTier1 = primarySsdPaths.includes(vol.mount_point);
                  const isTier2 = cascadeHddPaths.includes(vol.mount_point);
                  return (
                    <div
                      key={vol.mount_point}
                      className="p-3 rounded-xl bg-[#181d29] border border-gray-800 flex flex-col gap-2"
                    >
                      <div className="flex justify-between items-center">
                        <span className="font-semibold text-sm text-white">{vol.name || vol.mount_point}</span>
                        <span className="text-[11px] font-mono text-gray-400">
                          Trống {vol.available_gb} GB ({vol.kind_label})
                        </span>
                      </div>
                      <div className="flex gap-2">
                        <button
                          onClick={() => togglePrimarySsd(vol.mount_point)}
                          className={`flex-1 py-1.5 rounded-lg text-xs font-semibold border transition ${
                            isTier1
                              ? "bg-emerald-600 text-white border-emerald-400"
                              : "bg-gray-800/70 text-gray-400 border-gray-700 hover:text-white"
                          }`}
                        >
                          {isTier1 ? "✓ Tier 1: SSD Chính (2A)" : "+ Chọn làm SSD Chính (2A)"}
                        </button>
                        <button
                          onClick={() => toggleCascadeHdd(vol.mount_point)}
                          className={`flex-1 py-1.5 rounded-lg text-xs font-semibold border transition ${
                            isTier2
                              ? "bg-amber-600 text-white border-amber-400"
                              : "bg-gray-800/70 text-gray-400 border-gray-700 hover:text-white"
                          }`}
                        >
                          {isTier2 ? "✓ Tier 2: Cascade HDD (2B)" : "+ Chọn Cascade HDD (2B)"}
                        </button>
                      </div>
                    </div>
                  );
                })}
            </div>
          </div>

          <button
            disabled={isRunning}
            onClick={handleStartOffload}
            className={`mt-4 w-full py-3.5 rounded-xl font-bold text-sm flex items-center justify-center gap-2 shadow-lg transition ${
              isRunning
                ? "bg-gray-700 text-gray-400 cursor-not-allowed"
                : "bg-blue-600 hover:bg-blue-500 text-white"
            }`}
          >
            <Play className="w-4 h-4 fill-current" />
            {isRunning ? "ĐANG CHÉP & VERIFY VẬT LÝ..." : `BẮT ĐẦU OFFLOAD THẺ [${rollId}]`}
          </button>
        </div>
      </div>

      {/* THANH TIẾN TRÌNH & BIỂU ĐỒ TRẠNG THÁI REALTIME */}
      <div className="mt-6 bg-[#12161f] border border-gray-800 rounded-2xl p-5">
        <div className="flex justify-between items-center mb-2">
          <div className="text-xs font-mono text-gray-300">
            {progress ? (
              <>
                <span className="px-2 py-0.5 rounded bg-blue-950 text-blue-300 border border-blue-700 mr-2 font-bold">
                  {progress.stage.toUpperCase()}
                </span>
                {progress.message}
              </>
            ) : (
              "Sẵn sàng. Mọi luồng đọc kiểm tra đều kích hoạt cờ nhân Darwin F_NOCACHE & F_FULLFSYNC."
            )}
          </div>
          {progress && (
            <div className="text-xs font-mono text-emerald-400 font-bold">
              {progress.speed_mbps.toFixed(1)} MB/s • ETA: {progress.eta_seconds}s ({percent}%)
            </div>
          )}
        </div>

        <div className="w-full h-3 bg-gray-900 rounded-full overflow-hidden border border-gray-800">
          <div
            className="h-full bg-gradient-to-r from-blue-600 to-emerald-500 transition-all duration-200"
            style={{ width: `${percent}%` }}
          />
        </div>

        {errorMsg && (
          <div className="mt-4 p-3 rounded-lg bg-red-950/80 border border-red-600 text-xs text-red-200 flex items-center gap-2">
            <AlertTriangle className="w-4 h-4 text-red-400 shrink-0" />
            {errorMsg}
          </div>
        )}

        {lastSummary && (
          <div className="mt-4 p-3 rounded-lg bg-blue-950/40 border border-blue-800/80 text-xs text-blue-200 flex items-center justify-between">
            <div>
              Đã kiểm tra thành công <strong>{lastSummary.verified_files_count} files</strong> (100% Khớp mã băm{" "}
              {algorithm.toUpperCase()}). Đã tự động chuyển mã thẻ kế tiếp sang{" "}
              <strong>{lastSummary.next_suggested_roll}</strong>.
            </div>
            {lastSummary.report_html_path && (
              <span className="font-mono text-amber-300">
                Report: {lastSummary.report_html_path}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
```

---

## 7. CHẠY THỬ NGHIỆM & ĐÓNG GÓI FILE `.DMG` / `.APP` TRÊN macOS

Trong Terminal của Visual Studio Code:

```bash
# 1. Chạy chế độ Development (Hot-reload cả giao diện React và lõi Rust)
npm run tauri dev

# 2. Đóng gói thành ứng dụng macOS độc lập (.app và .dmg) tối ưu cho Apple Silicon / Intel
npm run tauri build
```

Sau khi lệnh `npm run tauri build` hoàn tất, file cài đặt ứng dụng hoàn chỉnh của bạn sẽ nằm tại:
`src-tauri/target/release/bundle/dmg/offshoot-pro_1.0.0_aarch64.dmg` (hoặc `x64.dmg`).
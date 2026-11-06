//! 不可变文件底座。
//!
//! 打开时优先整文件只读 mmap（单个大文件 mmap 比流式读快约四分之一，
//! 与 ripgrep 的选择一致），映射失败（页面文件受限/锁盘/权限）回退到共享句柄
//! 的 seek+read。两条路径对上层呈现同一 `read_span` 语义。
//!
//! 轮转防御（计划风险 R1）：日志文件常被外部截断/重写，Windows 上
//! 访问已缩短文件的映射页会触发 EXCEPTION_IN_PAGE_ERROR。上层在
//! 关键路径调用 [`FileBase::file_changed`]（O(1) metadata 长度比对），
//! 变化即整体失效缓存并提示重载。

use parking_lot::{Mutex, RwLock};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 顺序扫描的单次读块大小（klogg 用 5 MiB，同一量级）。
pub const READ_CHUNK: usize = 5 << 20;

pub struct FileBase {
    path: PathBuf,
    /// 打开时的字节长度（follow-tail 采纳增长后更新）。原子：len() 在
    /// UI 线程读，adopt_growth 在轮询路径写。
    size_at_open: AtomicU64,
    /// 打开时的字节代：文件被外部重写（轮转/截断）时由上层递增，
    /// 各缓存按 (块号, 代) 整体失效。
    generation: AtomicU64,
    /// Windows 的映射是**定长**的：文件追加后旧映射不见新字节，
    /// follow-tail 采纳增长时必须重映射（ RwLock：读路径共享，重映
    /// 射独占，旧映射在读者走完后才释放）。
    mmap: RwLock<Option<memmap2::Mmap>>,
    file: Mutex<File>,
}

impl FileBase {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let mmap = unsafe { memmap2::Mmap::map(&file).ok() };
        Ok(Self {
            path: path.to_path_buf(),
            size_at_open: AtomicU64::new(size),
            generation: AtomicU64::new(0),
            mmap: RwLock::new(mmap),
            file: Mutex::new(file),
        })
    }

    /// 跳过 mmap 的打开（pread 回退路径）。Windows 不允许截断一个
    /// 被映射的文件，轮转检测的测试用它模拟外部重写。
    pub fn open_unmapped(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        Ok(Self {
            path: path.to_path_buf(),
            size_at_open: AtomicU64::new(size),
            generation: AtomicU64::new(0),
            mmap: RwLock::new(None),
            file: Mutex::new(file),
        })
    }

    pub fn len(&self) -> u64 {
        self.size_at_open.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 当前磁盘长度（尾随轮询用；与 [`FileBase::len`] 的差即增量）。
    pub fn current_len(&self) -> u64 {
        self.file
            .lock()
            .metadata()
            .map(|m| m.len())
            .unwrap_or_else(|_| self.len())
    }

    /// follow-tail：把外部追加当作新基准（此后 [`FileBase::file_changed`]
    /// 对该增长不再报警，续扫把增量并入行表），并按新长度重映射。
    pub fn adopt_growth(&self, new_len: u64) {
        self.size_at_open.store(new_len, Ordering::Release);
        // Windows 映射定长：追加的字节只有重映射后才可见。
        let f = self.file.lock();
        let remapped = unsafe { memmap2::Mmap::map(&*f).ok() };
        drop(f);
        if let Some(m) = remapped {
            *self.mmap.write() = Some(m);
        }
    }

    /// 文件是否已被外部改动（长度变化即视为轮转/重写）。O(1)。
    /// follow-tail 采纳的增长（adopt_growth）不算改动。
    pub fn file_changed(&self) -> bool {
        self.file
            .lock()
            .metadata()
            .map(|m| m.len() != self.len())
            .unwrap_or(true)
    }

    /// mmap 后备的字节视图快照（guard 存活期间有效）。`None` = pread
    /// 回退路径。（follow-tail 重映射会替换映射，长期持有 guard 会钉住
    /// 旧映射 —— 只作短期读取用。）
    pub fn mapped(&self) -> Option<parking_lot::RwLockReadGuard<'_, Option<memmap2::Mmap>>> {
        let g = self.mmap.read();
        if g.is_some() {
            Some(g)
        } else {
            None
        }
    }

    /// 读取 `[off, off+buf.len())`，越界部分静默截断，返回实际读到
    /// 的字节数。mmap 命中时无系统调用。
    pub fn read_span(&self, off: u64, buf: &mut [u8]) -> usize {
        {
            let m = self.mmap.read();
            if let Some(m) = m.as_ref() {
                let len = m.len() as u64;
                if off >= len {
                    return 0;
                }
                let end = (off + buf.len() as u64).min(len);
                let n = (end - off) as usize;
                buf[..n].copy_from_slice(&m[off as usize..end as usize]);
                return n;
            }
        }
        let mut f = self.file.lock();
        if f.seek(SeekFrom::Start(off)).is_err() {
            return 0;
        }
        f.read(buf).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evlarge-{name}-{}", std::process::id()));
        let mut f = File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    #[test]
    fn read_span_mmap_and_bounds() {
        let p = temp_file("span", b"hello world");
        let base = FileBase::open(&p).unwrap();
        let mut buf = [0u8; 5];
        assert_eq!(base.read_span(6, &mut buf), 5);
        assert_eq!(&buf, b"world");
        // 截断到 EOF
        let mut tail = [0u8; 10];
        assert_eq!(base.read_span(8, &mut tail), 3);
        assert_eq!(&tail[..3], b"rld");
        // 越界
        assert_eq!(base.read_span(100, &mut tail), 0);
        assert!(!base.file_changed());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn rotation_detected_by_length_change() {
        let p = temp_file("rot", b"0123456789");
        // mmap 中的文件 Windows 不允许就地截断，用 pread 路径验证
        // 轮转检测本身（mmap 路径的失效由 check_rotated 的调用方
        // 经同一 metadata 比对覆盖）。
        let base = FileBase::open_unmapped(&p).unwrap();
        assert!(!base.file_changed());
        std::fs::write(&p, b"short").unwrap();
        assert!(base.file_changed());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn pread_fallback_reads() {
        let p = temp_file("pread", b"hello");
        let base = FileBase::open_unmapped(&p).unwrap();
        let mut buf = [0u8; 3];
        assert_eq!(base.read_span(2, &mut buf), 3);
        assert_eq!(&buf, b"llo");
        assert!(base.mapped().is_none());
        std::fs::remove_file(&p).ok();
    }
}

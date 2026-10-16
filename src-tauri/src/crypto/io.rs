//! 原子写盘 + 滚动备份。
//!
//! 库文件（`.mdl` / `.mdlb` / `vault.json`）是整棵树序列化后加密的结果，
//! 每次保存都是全量重写。裸 `fs::write` 在崩溃、断电、磁盘满、iCloud/Dropbox
//! 半写等场景下，可能把整个库写成不可解的字节——丢的不是一个笔记，是所有笔记。
//!
//! 所有落盘统一走 [`atomic_write`]：
//! 1. 先把新内容写进同目录的临时文件并 `fsync`，确保数据真正落盘；
//! 2. 覆盖前把上一版目标滚动备份（`.bak` / `.bak.1` …），顺带提供「恢复上一版」能力；
//! 3. 用 `rename` 原子替换目标（同分区，POSIX/Windows 均为原子操作）；
//! 4. `rename` 后 `fsync` 父目录（POSIX），让目录项变更也持久化。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};

use super::error::{CryptoError, Result};

/// 保留的备份份数（含最新的 `.bak`）。
const BACKUP_KEEP: usize = 2;

/// 「保存时保留上一版备份」开关：由前端经 `set_backup_enabled` 同步，默认开启。
/// 关闭后原子写盘仍然生效（tmp → fsync → rename），只是不再产生 `.mlk-bak` 旁路文件。
static BACKUP_ENABLED: AtomicBool = AtomicBool::new(true);

/// 设置是否在覆盖写盘时保留上一版备份。
pub fn set_backups_enabled(enabled: bool) {
    BACKUP_ENABLED.store(enabled, Ordering::SeqCst);
}

/// 临时文件与备份的标记后缀，带点前缀 + MarkLock 专属 token，避免与用户文件重名。
const TMP_MARK: &str = ".mlk-tmp";
const BAK_MARK: &str = ".mlk-bak";

/// 判断某个文件名是否为落盘过程产生的旁路文件（临时 / 备份）。
///
/// 库目录内所有非 `vault.json` 文件都被视为加密笔记，因此这些旁路文件
/// 必须在目录列举与改密重包裹时被排除，否则会显示为幽灵笔记或被误处理。
pub fn is_sidecar(name: &str) -> bool {
    name.starts_with('.') && (name.contains(TMP_MARK) || name.contains(BAK_MARK))
}

/// 原子写入 `path`：写临时文件 → fsync → 滚动备份上一版 → rename → fsync 父目录。
///
/// 目标不存在时（新建）跳过备份，等价于「安全创建」。任一步失败都会尽量清理临时文件，
/// 原目标保持完好，绝不会留下半写的字节。
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let tmp = tmp_path(&dir, path);

    // 1) 写临时文件并 fsync。
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| CryptoError::Io(format!("创建临时文件失败: {e}")))?;
    let write_res = (|| -> std::io::Result<()> {
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    drop(f);
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(CryptoError::Io(format!("写入临时文件失败: {e}")));
    }

    // 2) 覆盖前把上一版滚动备份（仅当开启备份且目标已存在）。
    if BACKUP_ENABLED.load(Ordering::SeqCst) && path.exists() {
        if let Err(e) = rotate_backups(path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    }

    // 3) 原子 rename 覆盖目标。
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(CryptoError::Io(format!("替换目标失败: {e}")));
    }

    // 4) fsync 父目录（POSIX；Windows 不支持打开目录，忽略）。
    sync_dir(&dir);

    Ok(())
}

/// 生成同目录下的唯一临时文件名（`.名称.tmp-<pid>-<纳秒>`，点前缀便于隐藏/忽略）。
fn tmp_path(dir: &Path, path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "marklock".into());
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!(".{name}{TMP_MARK}-{pid}-{nanos}"))
}

/// 第 `idx` 份备份的路径：`idx=0` 为 `.bak`（最新），越大越旧。
fn backup_path(path: &Path, idx: usize) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "marklock".into());
    if idx == 0 {
        path.with_file_name(format!(".{name}{BAK_MARK}"))
    } else {
        path.with_file_name(format!(".{name}{BAK_MARK}.{idx}"))
    }
}

/// 覆盖目标前的备份滚动：删最旧、依次后移、把当前目标复制为最新 `.bak`。
fn rotate_backups(path: &Path) -> Result<()> {
    // 丢弃超出保留份数的最旧备份。
    let _ = fs::remove_file(backup_path(path, BACKUP_KEEP - 1));
    // 依次后移：`.bak.{i}` → `.bak.{i+1}`。
    for i in (1..BACKUP_KEEP - 1).rev() {
        let from = backup_path(path, i);
        if from.exists() {
            fs::rename(&from, backup_path(path, i + 1))
                .map_err(|e| CryptoError::Io(format!("备份滚动失败: {e}")))?;
        }
    }
    // 当前 `.bak` 后移为 `.bak.1`。
    let cur = backup_path(path, 0);
    if cur.exists() {
        fs::rename(&cur, backup_path(path, 1))
            .map_err(|e| CryptoError::Io(format!("备份滚动失败: {e}")))?;
    }
    // 把上一版目标复制为新的 `.bak` 并 fsync，确保旧数据先安全落盘再覆盖目标。
    copy_with_sync(path, &cur)
        .map_err(|e| CryptoError::Io(format!("备份上一版失败: {e}")))?;
    Ok(())
}

/// 读全量再写并 fsync 的复制（库文件较小，整体读入内存可接受）。
fn copy_with_sync(from: &Path, to: &Path) -> std::io::Result<()> {
    let data = fs::read(from)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(to)?;
    f.write_all(&data)?;
    f.flush()?;
    f.sync_all()
}

/// fsync 父目录，让 rename / 新建备份的目录项持久化（仅 POSIX）。
fn sync_dir(dir: &Path) {
    if cfg!(unix) {
        if let Ok(f) = File::open(dir) {
            let _ = f.sync_all();
        }
    }
}

/// 用最近一份备份 `.mlk-bak` 恢复目标；恢复前经原子写盘把当前版本滚动为新备份，故可再次撤销。
/// 返回恢复后文件的字节数。
pub fn restore_latest_backup(path: &Path) -> Result<usize> {
    let bak = backup_path(path, 0);
    if !bak.exists() {
        return Err(CryptoError::Io("没有可恢复的上一版备份".into()));
    }
    let data = fs::read(&bak).map_err(|e| CryptoError::Io(format!("读取备份失败: {e}")))?;
    // 当前目标 → 新 .bak，旧 .bak → .bak.1，备份内容 → 目标（仅当备份开关开启时才滚动）。
    atomic_write(path, &data)?;
    Ok(data.len())
}

/// 是否存在可恢复的最近一份备份（`.mlk-bak`）。供前端在弹确认框前做廉价预检。
pub fn latest_backup_exists(path: &Path) -> bool {
    backup_path(path, 0).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("marklock_io_{tag}_{}_{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_overwrite_keeps_backup() {
        let dir = unique_tmp_dir("bak");
        let path = dir.join("vault.mdlb");

        atomic_write(&path, b"v1").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v1");
        // 新建不应产生备份。
        assert!(!backup_path(&path, 0).exists());

        atomic_write(&path, b"v2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v2");
        // 上一版应被备份。
        assert_eq!(fs::read(backup_path(&path, 0)).unwrap(), b"v1");

        atomic_write(&path, b"v3").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v3");
        assert_eq!(fs::read(backup_path(&path, 0)).unwrap(), b"v2");
        assert_eq!(fs::read(backup_path(&path, 1)).unwrap(), b"v1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_temp_file_left_behind() {
        let dir = unique_tmp_dir("clean");
        let path = dir.join("note.mdl");
        atomic_write(&path, b"hello").unwrap();
        atomic_write(&path, b"world").unwrap();
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(TMP_MARK))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_latest_backup_rolls_current() {
        let dir = unique_tmp_dir("restore");
        let path = dir.join("note.mdl");

        atomic_write(&path, b"v1").unwrap();
        atomic_write(&path, b"v2").unwrap(); // .bak = v1, target = v2
        assert!(restore_latest_backup(&path).is_ok());
        // 目标回到上一版 v1，恢复前的 v2 被滚动为新备份（可再次撤销）。
        assert_eq!(fs::read(&path).unwrap(), b"v1");
        assert_eq!(fs::read(backup_path(&path, 0)).unwrap(), b"v2");

        // 无任何备份时恢复应报错（先删掉旁路备份）。
        let dir2 = unique_tmp_dir("restore_none");
        let fresh = dir2.join("x.mdl");
        atomic_write(&fresh, b"only").unwrap(); // 新建不产生备份
        assert!(restore_latest_backup(&fresh).is_err());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }
}

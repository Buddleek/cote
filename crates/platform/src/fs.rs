//! 原子写入：先写临时文件，再 rename 到目标（FR-1.5）。
//!
//! 保证磁盘上永远只存在完整的旧文件或完整的新文件，
//! 崩溃/断电不会产生半写损坏的文档。

use std::fs;
use std::io;
use std::path::Path;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = path.with_file_name(format!(".{file_name}.cote-tmp"));
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // 清理临时文件，避免残留
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_roundtrip() {
        let dir = std::env::temp_dir().join("cote-platform-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("atomic-test.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        write_atomic(&path, b"world!").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"world!");
        // 无临时残留
        let tmp = path.with_file_name(".atomic-test.txt.cote-tmp");
        assert!(!tmp.exists());
        let _ = fs::remove_file(&path);
    }
}

use crate::error::{Error, Result};
use libsqlite3_sys as sqlite;
use std::{
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Database(*mut sqlite::sqlite3);
impl Database {
    fn open(path: &Path, flags: i32) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::invalid("备份路径无效"))?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: path is NUL-terminated and lives for the call; handle is a
        // writable out-pointer. Database owns and closes even a failed-open
        // handle, when SQLite returns one. No handle crosses worker threads.
        let code =
            unsafe { sqlite::sqlite3_open_v2(path.as_ptr(), &mut handle, flags, std::ptr::null()) };
        let db = Self(handle);
        if code != sqlite::SQLITE_OK || handle.is_null() {
            return Err(Error::new(503, "无法打开备份账本"));
        }
        Ok(db)
    }
    fn valid(&self) -> Result<()> {
        for (sql, expected) in [
            (c"PRAGMA integrity_check", Some("ok")),
            (c"PRAGMA foreign_key_check", None),
        ] {
            let statement = Statement::prepare(self, sql)?;
            // SAFETY: prepare returned a non-null live statement borrowing self.
            let code = unsafe { sqlite::sqlite3_step(statement.raw) };
            let passed = match expected {
                Some(expected) if code == sqlite::SQLITE_ROW => {
                    // SAFETY: SQLITE_ROW makes column 0 available. SQLite owns
                    // this NUL-terminated text until the next step/finalize.
                    let value = unsafe { sqlite::sqlite3_column_text(statement.raw, 0) };
                    let matches = !value.is_null()
                        // SAFETY: non-null text is read before the next step;
                        // neither the pointer nor a borrowed slice escapes.
                        && unsafe { CStr::from_ptr(value.cast()) }.to_bytes() == expected.as_bytes();
                    // SAFETY: statement is still live; the text borrow is over.
                    matches && unsafe { sqlite::sqlite3_step(statement.raw) } == sqlite::SQLITE_DONE
                }
                None => code == sqlite::SQLITE_DONE,
                _ => false,
            };
            if !passed {
                return Err(Error::new(503, "账本完整性或外键检查未通过"));
            }
        }
        Ok(())
    }
}
struct Statement<'db> {
    raw: *mut sqlite::sqlite3_stmt,
    _database: std::marker::PhantomData<&'db Database>,
}
impl<'db> Statement<'db> {
    fn prepare(database: &'db Database, sql: &CStr) -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the live database outlives this guard; sql is NUL-terminated,
        // and raw is a valid out-pointer. The statement is finalized once.
        let code = unsafe {
            sqlite::sqlite3_prepare_v2(database.0, sql.as_ptr(), -1, &mut raw, std::ptr::null_mut())
        };
        let statement = Self {
            raw,
            _database: std::marker::PhantomData,
        };
        if code != sqlite::SQLITE_OK || raw.is_null() {
            return Err(Error::new(503, "备份完整性检查失败"));
        }
        Ok(statement)
    }
}
impl Drop for Statement<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: this guard uniquely owns raw and cannot outlive Database.
            unsafe {
                sqlite::sqlite3_finalize(self.raw);
            }
        }
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned by this guard. Borrowed statements
            // and backup transfers have been released before the database.
            unsafe {
                sqlite::sqlite3_close(self.0);
            }
        }
    }
}
struct Transfer(*mut sqlite::sqlite3_backup);
impl Drop for Transfer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard uniquely owns the transfer; both databases
            // remain live until it is dropped, including error/timeout paths.
            unsafe {
                sqlite::sqlite3_backup_finish(self.0);
            }
        }
    }
}

pub async fn create(source: &Path, output: &Path) -> Result<()> {
    let source = source.to_owned();
    let output = output.to_owned();
    tokio::task::spawn_blocking(move || create_sync(&source, &output))
        .await
        .map_err(|_| Error::unavailable())?
}
fn create_sync(source: &Path, output: &Path) -> Result<()> {
    if !source.is_file() {
        return Err(Error::invalid("账本不存在"));
    }
    if output.exists() {
        return Err(Error::conflict("备份目标已存在，不会覆盖"));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(Error::invalid("备份目录不存在"));
    }
    let temporary: PathBuf = parent.join(format!(".backup-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temporary)?;
        drop(file);
        {
            let input = Database::open(
                source,
                sqlite::SQLITE_OPEN_READONLY | sqlite::SQLITE_OPEN_NOMUTEX,
            )?;
            let output = Database::open(
                &temporary,
                sqlite::SQLITE_OPEN_READWRITE | sqlite::SQLITE_OPEN_NOMUTEX,
            )?;
            // SAFETY: both database handles are live and distinct; static main
            // names are NUL-terminated. Transfer owns the returned backup.
            let pointer = unsafe {
                sqlite::sqlite3_backup_init(output.0, c"main".as_ptr(), input.0, c"main".as_ptr())
            };
            if pointer.is_null() {
                return Err(Error::new(503, "备份未能开始"));
            }
            let mut transfer = Transfer(pointer);
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                // SAFETY: transfer is initialized and neither database has
                // been closed or accessed concurrently by this worker.
                let code = unsafe { sqlite::sqlite3_backup_step(transfer.0, 128) };
                if code == sqlite::SQLITE_DONE {
                    break;
                }
                if ![
                    sqlite::SQLITE_OK,
                    sqlite::SQLITE_BUSY,
                    sqlite::SQLITE_LOCKED,
                ]
                .contains(&code)
                    || Instant::now() >= deadline
                {
                    return Err(Error::new(503, "备份未完成，请稍后重试"));
                }
                if code != sqlite::SQLITE_OK {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            // SAFETY: finish consumes the live transfer. Clearing its pointer
            // prevents Drop from finishing it a second time, even on failure.
            let code = unsafe { sqlite::sqlite3_backup_finish(transfer.0) };
            transfer.0 = std::ptr::null_mut();
            if code != sqlite::SQLITE_OK {
                return Err(Error::new(503, "备份未完成"));
            }
            output.valid()?;
        }
        File::open(&temporary)?.sync_all()?;
        // A same-directory hard link publishes atomically without clobbering a
        // destination created concurrently. The temporary name is ours only.
        std::fs::hard_link(&temporary, output).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::conflict("备份目标已存在，不会覆盖")
            } else {
                error.into()
            }
        })?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&temporary);
    if result.is_ok() {
        cleanup?;
        File::open(parent)?.sync_all()?;
    }
    result
}

pub async fn check(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        Database::open(
            &path,
            sqlite::SQLITE_OPEN_READONLY | sqlite::SQLITE_OPEN_NOMUTEX,
        )?
        .valid()
    })
    .await
    .map_err(|_| Error::unavailable())?
}

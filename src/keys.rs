use crate::{
    error::{Error, Result},
    json::{hash, text},
    store::{self, Store},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const KEYRING: &str = "session.keys.json";
const PENDING: &str = "session.keys.pending.json";

/// Shared by the service and offline maintenance commands. The file handle is
/// retained for their entire lifetime; a crashed process releases the OS lock.
pub struct LedgerLock(File);
impl LedgerLock {
    pub fn acquire(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let file = private_options()
            .create(true)
            .truncate(false)
            .open(directory.join("settlement.lock"))?;
        file.try_lock_exclusive()
            .map_err(|_| Error::conflict("账本正在使用，请先停止结算服务"))?;
        Ok(Self(file))
    }
}
impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    active: String,
    keys: BTreeMap<String, String>,
}

pub struct Keyring {
    pub active: String,
    keys: BTreeMap<String, fernet::Fernet>,
}
impl Keyring {
    pub async fn load(directory: &Path, store: &Store) -> Result<Self> {
        ensure_legacy_key(directory)?;
        let pending = directory.join(PENDING);
        if pending.exists() {
            let document = read_document(&pending)?;
            apply_rotation(directory, store, &document).await?;
        }
        let document = current_document(directory)?;
        // Rollback to an older Rust service can still create legacy ciphertext.
        // Keep the raw active key synchronized, without changing Cookie or CSRF.
        let legacy = std::fs::read_to_string(directory.join("session.key"))?;
        if document.keys.get(&document.active).map(String::as_str) != Some(legacy.trim()) {
            return Err(Error::conflict(
                "会话密钥文件不一致，请恢复同一次备份中的密钥文件",
            ));
        }
        Self::from_document(&document)
    }
    fn from_document(document: &Document) -> Result<Self> {
        if document.version != 1
            || document.keys.is_empty()
            || document.keys.len() > 32
            || !document.keys.contains_key(&document.active)
            || document.keys.keys().any(|id| !valid_id(id))
        {
            return Err(Error::invalid("会话密钥集格式无效"));
        }
        let keys = document
            .keys
            .iter()
            .map(|(id, key)| {
                fernet::Fernet::new(key)
                    .map(|cipher| (id.clone(), cipher))
                    .ok_or_else(|| Error::invalid("会话密钥格式无效"))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            active: document.active.clone(),
            keys,
        })
    }
    pub fn encrypt(&self, value: &[u8]) -> String {
        self.keys[&self.active].encrypt(value)
    }
    pub fn decrypt(&self, value: &str) -> Result<Vec<u8>> {
        self.keys[&self.active]
            .decrypt(value)
            .ok()
            .or_else(|| {
                self.keys
                    .iter()
                    .filter(|(id, _)| *id != &self.active)
                    .find_map(|(_, key)| key.decrypt(value).ok())
            })
            .ok_or_else(|| Error::new(401, "登录已失效"))
    }
}

/// The caller must hold LedgerLock. A durable journal retains both keys before
/// any database change; recovery rolls forward and never deletes old keys.
pub async fn rotate(directory: &Path, store: &Store, id: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(Error::invalid(
            "密钥 ID 仅允许 1–64 位字母、数字、连字符或下划线",
        ));
    }
    let _ = Keyring::load(directory, store).await?;
    let mut document = current_document(directory)?;
    if document.keys.contains_key(id) {
        return Err(Error::conflict("密钥 ID 已存在"));
    }
    if document.keys.len() >= 32 {
        return Err(Error::conflict(
            "密钥集已达到保留上限，请先归档并审查旧密钥",
        ));
    }
    document.active = id.to_owned();
    document
        .keys
        .insert(id.to_owned(), fernet::Fernet::generate_key());
    atomic_private(&directory.join(PENDING), &serde_json::to_vec(&document)?)?;
    apply_rotation(directory, store, &document).await
}

async fn apply_rotation(directory: &Path, store: &Store, document: &Document) -> Result<()> {
    let ring = Keyring::from_document(document)?;
    let mut db = store.write().await?;
    let sessions = store::all(&mut db, "SELECT id,token FROM sessions ORDER BY id", &[]).await?;
    for session in sessions {
        let raw = ring.decrypt(text(&session, "token")).map_err(|_| {
            Error::conflict("存在无法解密的历史会话，轮换未完成；请保留密钥集并检查备份")
        })?;
        store::exec(
            &mut db,
            "UPDATE sessions SET token=? WHERE id=?",
            &[json!(ring.encrypt(&raw)), session["id"].clone()],
        )
        .await?;
        store::exec(&mut db,"INSERT INTO rust_session_keys(session_id,key_id) VALUES(?,?) ON CONFLICT(session_id) DO UPDATE SET key_id=excluded.key_id",
            &[session["id"].clone(),json!(ring.active)]).await?;
    }
    db.commit().await?;
    atomic_private(
        &directory.join("session.key"),
        document.keys[&document.active].as_bytes(),
    )?;
    atomic_private(&directory.join(KEYRING), &serde_json::to_vec(document)?)?;
    std::fs::remove_file(directory.join(PENDING))?;
    sync_directory(directory)?;
    Ok(())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn current_document(directory: &Path) -> Result<Document> {
    let path = directory.join(KEYRING);
    if path.exists() {
        return read_document(&path);
    }
    let key = std::fs::read_to_string(directory.join("session.key"))?
        .trim()
        .to_owned();
    let id = format!("legacy-{}", &hash(key.as_bytes())[..16]);
    Ok(Document {
        version: 1,
        active: id.clone(),
        keys: BTreeMap::from([(id, key)]),
    })
}
fn read_document(path: &Path) -> Result<Document> {
    if std::fs::metadata(path)?.len() > 16 * 1024 {
        return Err(Error::invalid("会话密钥集过大"));
    }
    let document = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| Error::invalid("会话密钥集格式无效"))?;
    let _ = Keyring::from_document(&document)?;
    Ok(document)
}
fn ensure_legacy_key(directory: &Path) -> Result<()> {
    match private_options()
        .create_new(true)
        .open(directory.join("session.key"))
    {
        Ok(mut file) => {
            file.write_all(fernet::Fernet::generate_key().as_bytes())?;
            file.sync_all()?;
            sync_directory(directory)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
pub fn atomic_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(Error::unavailable)?;
    let temporary: PathBuf = parent.join(format!(".secret-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = private_options().create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

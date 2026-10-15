use crate::Result;
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .into()
}

fn command(program: &str, args: &[&str]) -> Result<String> {
    let result = Command::new(program)
        .args(args)
        .current_dir(root())
        .output()?;
    if !result.status.success() {
        return Err(format!(
            "{program} operation failed (exit {:?})",
            result.status.code()
        )
        .into());
    }
    Ok(String::from_utf8(result.stdout)?)
}

fn version() -> Result<String> {
    let manifest: toml::Value = fs::read_to_string(root().join("Cargo.toml"))?.parse()?;
    Ok(manifest["package"]["version"]
        .as_str()
        .ok_or("Missing version")?
        .into())
}

pub fn section(text: &str, tag: &str) -> Result<String> {
    let mut found = false;
    let mut lines = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## v") {
            if found {
                break;
            }
            found = heading.split_whitespace().next() == tag.strip_prefix('v');
            continue;
        }
        if found {
            lines.push(line);
        }
    }
    let body = lines.join("\n").trim().to_owned();
    if body.is_empty() {
        return Err(format!("CHANGELOG.md 缺少非空版本说明：{tag}").into());
    }
    Ok(body + "\n")
}

pub fn notes(tag: &str, output: &Path) -> Result<()> {
    fs::write(
        output,
        section(&fs::read_to_string(root().join("CHANGELOG.md"))?, tag)?,
    )?;
    Ok(())
}

fn safe_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !path.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some("data" | ".git" | "node_modules" | "target" | "session.key")
            )
        })
        && !path.components().any(|part| {
            part.as_os_str().to_str().is_some_and(|name| {
                (name.starts_with(".env") && text != ".env.example")
                    || name.starts_with("session.keys")
                    || name.starts_with(".secret-")
                    || name.starts_with(".backup-")
                    || name == "settlement.lock"
            })
        })
        && ![
            ".sqlite",
            ".sqlite3",
            ".db",
            ".sqlite-wal",
            ".sqlite-shm",
            ".sqlite-journal",
            ".sqlite3-wal",
            ".sqlite3-shm",
            ".sqlite3-journal",
            ".db-wal",
            ".db-shm",
            ".db-journal",
            ".key",
            ".pem",
        ]
        .iter()
        .any(|suffix| text.ends_with(suffix))
}

fn append(builder: &mut tar::Builder<GzEncoder<fs::File>>, relative: &Path) -> Result<()> {
    if !safe_path(relative) {
        return Err("Release contains a forbidden runtime path".into());
    }
    let file = root().join(relative);
    let metadata = fs::symlink_metadata(&file)?;
    if !metadata.is_file() {
        return Err("Release inputs must be regular files, never symlinks".into());
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata.len());
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, relative, fs::File::open(file)?)?;
    Ok(())
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root().join(directory))? {
        let entry = entry?;
        let path = directory.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            walk(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

pub fn package(output: &Path) -> Result<()> {
    let version = version()?;
    if std::env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag")
        && std::env::var("GITHUB_REF_NAME").ok() != Some(format!("v{version}"))
    {
        return Err("发布标签与 Cargo.toml 版本不一致".into());
    }
    let mut files = command("git", &["ls-files", "-z"])?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    walk(Path::new("frontend/dist"), &mut files)?;
    files.sort();
    files.dedup();
    fs::create_dir_all(output)?;
    let name = format!("sub2api-settlement-v{version}.tar.gz");
    let destination = output.join(&name);
    let mut builder = tar::Builder::new(GzEncoder::new(
        fs::File::create(&destination)?,
        Compression::default(),
    ));
    for file in files {
        append(&mut builder, &file)?;
    }
    builder.into_inner()?.finish()?.sync_all()?;
    let mut hash = Sha256::new();
    let mut input = fs::File::open(destination)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    fs::File::create(output.join("SHA256SUMS"))?
        .write_all(format!("{}  {name}\n", hex::encode(hash.finalize())).as_bytes())?;
    println!("{name}");
    Ok(())
}

pub async fn smoke(image: &str, revision: &str) -> Result<()> {
    let name = format!("settlement-smoke-{}", uuid::Uuid::new_v4().simple());
    let inspect: Value = serde_json::from_str(&command("docker", &["image", "inspect", image])?)?;
    let version = version()?;
    if inspect[0]["Config"]["Labels"]["org.opencontainers.image.revision"] != revision
        || inspect[0]["Config"]["Labels"]["org.opencontainers.image.version"] != version
    {
        return Err("Image metadata does not match the tested revision".into());
    }
    let mount = format!(
        "type=bind,source={},target=/fixture.json,readonly",
        root().join("tests/compat/source.json").display()
    );
    command(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &name,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--tmpfs",
            "/data:rw,uid=10002,gid=10002,mode=0700",
            "--tmpfs",
            "/tmp",
            "--mount",
            &mount,
            "--env",
            "SETTLEMENT_ENV=test",
            "--env",
            "SOURCE_FIXTURE=/fixture.json",
            "--env",
            "SETTLEMENT_BACKGROUND=false",
            "--env",
            "SETTLEMENT_READ_ONLY=true",
            "--env",
            "SETTLEMENT_EXTERNAL_WRITES=false",
            image,
        ],
    )?;
    let result: Result<()> = async {
        let start = Instant::now();
        loop {
            if command("docker", &["exec", &name, "/app/settlement", "healthcheck"]).is_ok() { break; }
            let state: Value = serde_json::from_str(&command("docker", &["inspect", &name])?)?;
            if state[0]["State"]["Running"] != true || start.elapsed() > Duration::from_secs(60) {
                return Err("Image failed its startup health check".into());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let raw = command("docker", &["exec", &name, "bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/18082\nprintf 'GET /settlement/healthz HTTP/1.0\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' >&3\ncat <&3"])?;
        let normalized = raw.replace("\r\n", "\n");
        let (_, body) = normalized.split_once("\n\n").ok_or("Missing HTTP health body")?;
        let health: Value = serde_json::from_str(body)?;
        if health != serde_json::json!({"status":"ok","version":version,"revision":revision,"engine":"rust"}) {
            return Err("Image health identity differs from the tested revision".into());
        }
        command("docker", &["exec", &name, "sh", "-c", "test \"$(id -u)\" = 10002 && test ! -e /app/settlement-tools && ! command -v node && ! command -v cargo"])?;
        command("docker", &["stop", "--time", "60", &name])?;
        let state: Value = serde_json::from_str(&command("docker", &["inspect", &name])?)?;
        if state[0]["State"]["ExitCode"] != 0 || state[0]["State"]["OOMKilled"] != false {
            return Err("Image failed graceful shutdown".into());
        }
        Ok(())
    }.await;
    let cleanup = command("docker", &["rm", "--force", &name]);
    result?;
    cleanup?;
    println!("image smoke: passed; revision={revision}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_historical_release_has_nonempty_chinese_notes() {
        let text = include_str!("../../CHANGELOG.md");
        for tag in [
            "v0.1.0", "v0.2.0", "v0.2.1", "v0.3.0", "v0.4.0", "v0.5.0", "v0.5.1", "v0.6.0",
            "v0.6.1", "v0.7.0", "v0.8.0", "v0.9.0",
        ] {
            assert!(section(text, tag).unwrap().contains("###"));
        }
        assert!(section(text, "v99.0.0").is_err());
        assert_eq!(
            section("## v1.0.0 · 标题\n\n正文\n\n## v0.9.0\n旧文", "v1.0.0").unwrap(),
            "正文\n"
        );
        assert!(section("## v1.0.0\n", "v1.0.0").is_err());
    }
    #[test]
    fn package_rejects_runtime_credentials_and_path_escape() {
        for file in [
            ".env",
            ".env.local",
            "nested/.env",
            "data/source.json",
            "session.key",
            "nested/session.keys.json",
            "session.keys.pending.json",
            "nested/.secret-fixture",
            ".backup-fixture",
            "settlement.lock",
            "ledger.db",
            "ledger.sqlite3",
            "ledger.sqlite3-wal",
            "ledger.sqlite-shm",
            "ledger.db-journal",
            "a/../secret",
            "/etc/passwd",
            "a/token.pem",
        ] {
            assert!(!safe_path(Path::new(file)), "{file}");
        }
        for file in [
            ".env.example",
            "tests/fixtures/ledger/seed.sql",
            "src/main.rs",
        ] {
            assert!(safe_path(Path::new(file)), "{file}");
        }
    }
}

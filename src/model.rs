use crate::store::{atomic_write, Paths};
use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::ValueEnum;
use moonshine_rs::ModelArch;
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::PathBuf, process::Command};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    Small,
    #[default]
    Medium,
}
impl Choice {
    pub fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
        }
    }
    pub fn arch(self) -> ModelArch {
        match self {
            Self::Small => ModelArch::SmallStreaming,
            Self::Medium => ModelArch::MediumStreaming,
        }
    }
}
pub fn selected(p: &Paths) -> Choice {
    fs::read(p.root.join("model.json"))
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
pub fn select(p: &Paths, choice: Choice) -> Result<()> {
    atomic_write(&p.root.join("model.json"), &serde_json::to_vec(&choice)?)
}
pub fn path(p: &Paths, choice: Choice) -> PathBuf {
    p.root
        .join("models")
        .join(format!("moonshine-{}-streaming-en", choice.name()))
}
#[derive(Deserialize)]
struct Manifest {
    groups: Vec<Group>,
}
#[derive(Deserialize)]
struct Group {
    files: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    url: String,
    size: u64,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    checksum_type: String,
}
fn manifest(choice: Choice) -> Result<Manifest> {
    Ok(serde_json::from_str(&moonshine_rs::get_stt_dependencies(
        "en",
        Some(choice.arch()),
        false,
    )?)?)
}
fn verify_asset(file: &std::path::Path, asset: &Asset) -> Result<()> {
    if fs::metadata(file)?.len() != asset.size {
        bail!("Wrong model size for {}", asset.name);
    }
    if let Some(expected) = &asset.checksum {
        if asset.checksum_type != "crc32c" {
            bail!("Unsupported checksum type");
        }
        let mut input = fs::File::open(file)?;
        let mut checksum = 0;
        let mut buffer = [0; 65536];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            checksum = crc32c::crc32c_append(checksum, &buffer[..n]);
        }
        if expected != &base64::engine::general_purpose::STANDARD.encode(checksum.to_be_bytes()) {
            bail!("Checksum mismatch for {}", asset.name);
        }
    }
    Ok(())
}
pub fn ready(p: &Paths, choice: Choice) -> bool {
    path(p, choice).join("installed.json").exists()
}
pub fn install(p: &Paths, choice: Choice) -> Result<()> {
    use fs2::FileExt;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p.root.join("models/install.lock"))?;
    lock.try_lock_exclusive()
        .context("A model download is already running")?;
    let directory = path(p, choice);
    fs::create_dir_all(&directory)?;
    let manifest = manifest(choice)?;
    for asset in manifest.groups.iter().flat_map(|g| &g.files) {
        if !asset.url.starts_with("https://download.moonshine.ai/")
            || asset.name.contains('/')
            || asset.name.contains('\\')
            || asset.name.starts_with('.')
        {
            bail!("Unexpected model manifest asset");
        }
        let destination = directory.join(&asset.name);
        if verify_asset(&destination, asset).is_ok() {
            continue;
        }
        let temp = destination.with_extension(format!("{}.download", uuid::Uuid::new_v4()));
        eprintln!(
            "Downloading {} ({} MB); no audio upload",
            asset.name,
            asset.size / 1_000_000
        );
        let result = (|| -> Result<()> {
            let status = Command::new("/usr/bin/curl")
                .args([
                    "--fail",
                    "--location",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--connect-timeout",
                    "20",
                    "--max-time",
                    "600",
                    "--retry",
                    "2",
                    "--output",
                ])
                .arg(&temp)
                .arg(&asset.url)
                .status()
                .context("Start model download")?;
            if !status.success() {
                bail!("Model download failed; retry when online");
            }
            verify_asset(&temp, asset)?;
            fs::rename(&temp, &destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result?;
    }
    atomic_write(
        &directory.join("installed.json"),
        b"{\"language\":\"en\",\"quantization\":\"int8\"}",
    )?;
    select(p, choice)?;
    Ok(())
}
pub fn dependencies(choice: Choice) -> Result<String> {
    Ok(moonshine_rs::get_stt_dependencies(
        "en",
        Some(choice.arch()),
        false,
    )?)
}

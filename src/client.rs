use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::server::{OpenRequest, OpenResponse};
use crate::target::Target;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl ServerInfo {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ABEVIEW_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::cache_dir()
        .context("could not find a cache directory")?
        .join("abeview"))
}

fn state_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("server.json"))
}

pub fn read_state() -> Option<ServerInfo> {
    let bytes = fs::read(state_path().ok()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The file holds the API token, so only the owner may read it.
pub fn write_state(info: &ServerInfo) -> Result<()> {
    let dir = state_dir()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let tmp = dir.join(format!("server.json.{}", info.pid));
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&serde_json::to_vec(info)?)?;
    drop(f);
    fs::rename(&tmp, state_path()?)?;
    Ok(())
}

pub fn remove_state_if_owned(pid: u32) {
    if read_state().is_some_and(|s| s.pid == pid)
        && let Ok(path) = state_path()
    {
        let _ = fs::remove_file(path);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        // Never route the token through an HTTP proxy from the environment.
        .proxy(None)
        .build()
        .into()
}

fn is_alive(info: &ServerInfo) -> bool {
    agent()
        .get(info.url("/api/health"))
        .header("x-abeview-token", &info.token)
        .config()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .call()
        .is_ok()
}

pub fn ensure_server() -> Result<ServerInfo> {
    if let Some(info) = read_state().filter(is_alive) {
        return Ok(info);
    }
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Own process group, so Ctrl-C in the launching terminal leaves it running.
        .process_group(0)
        .spawn()
        .context("failed to start the abeview server")?;

    let started = Instant::now();
    while started.elapsed() < STARTUP_TIMEOUT {
        if let Some(info) = read_state().filter(|s| s.pid == child.id())
            && is_alive(&info)
        {
            return Ok(info);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the abeview server did not start within {STARTUP_TIMEOUT:?}")
}

pub fn open_view(info: &ServerInfo, target: &Target, note: Option<String>) -> Result<String> {
    let mut res = agent()
        .post(info.url("/api/views"))
        .header("x-abeview-token", &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .send_json(OpenRequest {
            target: target.clone(),
            note,
        })?;
    if !res.status().is_success() {
        bail!(
            "server rejected the request: {}",
            res.body_mut().read_to_string().unwrap_or_default()
        );
    }
    Ok(res.body_mut().read_json::<OpenResponse>()?.url)
}

pub fn stop() -> Result<bool> {
    let Some(info) = read_state().filter(is_alive) else {
        return Ok(false);
    };
    agent()
        .post(info.url("/api/shutdown"))
        .header("x-abeview-token", &info.token)
        .send_empty()?;
    let started = Instant::now();
    while is_alive(&info) && started.elapsed() < STARTUP_TIMEOUT {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(true)
}

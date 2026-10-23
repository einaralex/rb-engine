use crate::capture::{decode_stream, request, Analysis};
use crate::plan::TrackPlan;
use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// One track per call. A cache or worker pool can implement this interface later.
pub trait Analyzer: Send + Sync {
    fn analyze(&self, track: &TrackPlan, output: &Path) -> Result<Analysis>;
}

pub struct OfflineAnalyzer {
    pub executable: PathBuf,
    pub min_tempo: u32,
    pub key_detection: bool,
    pub timeout: Duration,
    pub idle_timeout: Duration,
    pub idle_grace: Duration,
}

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn terminal_seen(bytes: &[u8], scan: &mut usize) -> Result<Option<u32>> {
    while bytes.len().saturating_sub(*scan) >= 24 {
        let len = u64::from_le_bytes(bytes[*scan + 8..*scan + 16].try_into()?);
        if len > 64 * 1024 * 1024 {
            bail!("worker packet exceeds capture limit");
        }
        let size = 24 + len as usize;
        if bytes.len() - *scan < size {
            break;
        }
        let command = u32::from_le_bytes(bytes[*scan + 4..*scan + 8].try_into()?);
        *scan += size;
        if matches!(command, 14..=16) {
            return Ok(Some(command));
        }
    }
    Ok(None)
}

fn accept_worker(
    listener: &TcpListener,
    child: &mut Child,
    deadline: Instant,
) -> Result<TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child.try_wait()?.is_some() {
                    bail!("OfflineAnalyzer exited before connecting");
                }
                if Instant::now() >= deadline {
                    bail!("OfflineAnalyzer connection timed out");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

impl Analyzer for OfflineAnalyzer {
    fn analyze(&self, track: &TrackPlan, output: &Path) -> Result<Analysis> {
        if !self.executable.is_file() {
            bail!("OfflineAnalyzer executable is missing");
        }
        fs::create_dir(output)
            .with_context(|| format!("capture directory exists: {}", output.display()))?;
        let log = File::create(output.join("worker.log"))?;
        let listener =
            TcpListener::bind("127.0.0.1:0").context("cannot open local analyzer listener")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let mut worker = Worker(
            Command::new(&self.executable)
                .arg(port.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()
                .context("could not launch OfflineAnalyzer")?,
        );
        let deadline = Instant::now() + self.timeout;
        let mut stream = accept_worker(&listener, &mut worker.0, deadline)?;
        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        let audio = track.audio.to_str().context("audio path is not UTF-8")?;
        let request = request(audio, self.min_tempo, self.key_detection)?;
        fs::write(output.join("request.bin"), &request)?;
        stream.write_all(&request)?;
        let mut raw = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        let mut scan = 0;
        let mut last_packet = Instant::now();
        let mut terminal = None;
        loop {
            if Instant::now() >= deadline {
                bail!("OfflineAnalyzer timed out");
            }
            if Instant::now().duration_since(last_packet) > self.idle_timeout + self.idle_grace {
                bail!("OfflineAnalyzer stopped sending packets");
            }
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => {
                    raw.extend_from_slice(&buf[..count]);
                    last_packet = Instant::now();
                    if raw.len() > 256 * 1024 * 1024 {
                        bail!("Analyzer capture exceeds 256 MiB");
                    }
                    if let Some(command) = terminal_seen(&raw, &mut scan)? {
                        terminal = Some(command);
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if worker.0.try_wait()?.is_some() && raw.is_empty() {
                        bail!("OfflineAnalyzer exited without packets");
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        fs::write(output.join("received.bin"), &raw)?;
        if terminal != Some(15) {
            bail!("OfflineAnalyzer did not finish successfully: {terminal:?}");
        }
        let analysis = decode_stream(&raw)?;
        fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "outcome": "worker_finished", "audio": audio, "min_tempo": self.min_tempo,
                "sample_rate": analysis.sample_rate, "samples": analysis.samples,
                "bpm": analysis.bpm, "key": analysis.key,
                "overview_entries": analysis.overview_entries, "rgb_entries": analysis.rgb_entries,
            }))?,
        )?;
        Ok(analysis)
    }
}

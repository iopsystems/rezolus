const NAME: &str = "gpu_apple";

use crate::agent::*;

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use plist::Value;
use std::io::BufReader;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::stats::*;

// Shared state for tracking energy deltas across the parsing thread
struct EnergyTracker {
    last_energy: Option<i64>,
    last_update: Option<Instant>,
}

lazy_static::lazy_static! {
    static ref ENERGY_TRACKER: StdMutex<EnergyTracker> = StdMutex::new(EnergyTracker {
        last_energy: None,
        last_update: None,
    });
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = GpuAppleInner::new()?;

    Ok(Some(Box::new(GpuApple {
        inner: inner.into(),
    })))
}

pub struct GpuApple {
    inner: Mutex<GpuAppleInner>,
}

#[async_trait]
impl Sampler for GpuApple {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;
        let _ = inner.refresh();
    }
}

struct GpuAppleInner {
    process: Option<Child>,
    sample_interval: Duration,
}

impl GpuAppleInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let sample_interval = Duration::from_millis(1000);

        Ok(Self {
            process: None,
            sample_interval,
        })
    }

    pub fn refresh(&mut self) -> Result<(), std::io::Error> {
        // Start the process if not running
        if self.process.is_none() {
            self.start_powermetrics()?;
        }

        // Check if process is still running and trigger immediate sample
        if let Some(ref mut child) = self.process {
            if let Ok(Some(status)) = child.try_wait() {
                error!("powermetrics process exited with status: {}", status);
                self.process = None;
                return Err(std::io::Error::other("powermetrics process exited"));
            }

            // Send SIGINFO to trigger immediate sample
            let pid = child.id();
            match kill(Pid::from_raw(pid as i32), Signal::SIGINFO) {
                Ok(_) => debug!("Sent SIGINFO to powermetrics (pid {})", pid),
                Err(e) => debug!("Failed to send SIGINFO to powermetrics: {}", e),
            }
        }

        Ok(())
    }

    fn start_powermetrics(&mut self) -> Result<(), std::io::Error> {
        debug!("Starting powermetrics process");

        // Only use gpu_power sampler - it provides freq_hz, idle_ratio, and gpu_energy
        let mut child = Command::new("powermetrics")
            .args([
                "--samplers",
                "gpu_power",
                "--sample-rate",
                &self.sample_interval.as_millis().to_string(),
                "--sample-count",
                "-1",
                "--format",
                "plist",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                std::io::Error::other(format!("Failed to spawn powermetrics: {}. Note: This sampler requires sudo/root privileges.", e))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("Failed to capture powermetrics stdout"))?;

        // Spawn a thread to continuously read and parse output
        std::thread::spawn(move || {
            parse_powermetrics_output(stdout);
        });

        self.process = Some(child);
        debug!("powermetrics process started successfully");

        Ok(())
    }
}

impl Drop for GpuAppleInner {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn parse_powermetrics_output(stdout: std::process::ChildStdout) {
    let reader = BufReader::new(stdout);
    let mut buffer = Vec::new();

    for byte_result in reader.bytes() {
        let byte = match byte_result {
            Ok(b) => b,
            Err(e) => {
                error!("Error reading powermetrics output: {}", e);
                break;
            }
        };

        if byte == 0 {
            // NUL separator - parse the accumulated buffer
            if !buffer.is_empty() {
                if let Err(e) = parse_plist_sample(&buffer) {
                    warn!("Failed to parse plist sample: {}", e);
                }
                buffer.clear();
            }
        } else {
            buffer.push(byte);
        }
    }

    debug!("powermetrics output reader thread exiting");
}

fn parse_plist_sample(data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let value = plist::from_bytes::<Value>(data)?;

    if let Value::Dictionary(root) = value {
        // Parse GPU metrics from "gpu" dict
        if let Some(Value::Dictionary(gpu_dict)) = root.get("gpu") {
            // GPU Clock (graphics) - keep in Hz to match NVIDIA sampler
            if let Some(Value::Real(freq_hz)) = gpu_dict.get("freq_hz") {
                let clock_hz = (*freq_hz) as i64;
                let _ = GPU_CLOCK_GRAPHICS.set(0, clock_hz);
                debug!("GPU clock: {} Hz", clock_hz);
            }

            // GPU Utilization (convert idle_ratio to utilization percentage)
            if let Some(Value::Real(idle_ratio)) = gpu_dict.get("idle_ratio") {
                let utilization = ((1.0 - *idle_ratio) * 100.0) as i64;
                let _ = GPU_UTILIZATION.set(0, utilization);
                debug!("GPU utilization: {}%", utilization);
            }

            // GPU Energy and Power
            // gpu_energy is in millijoules (mJ), we calculate power in milliwatts (mW)
            if let Some(Value::Integer(energy)) = gpu_dict.get("gpu_energy") {
                let current_energy = energy.as_signed().unwrap_or(0);
                let now = Instant::now();

                // Update the cumulative energy counter
                let _ = GPU_ENERGY_CONSUMPTION.set(0, current_energy as u64);

                // Calculate instantaneous power from energy delta
                if let Ok(mut tracker) = ENERGY_TRACKER.lock() {
                    if let (Some(last_energy), Some(last_update)) = (tracker.last_energy, tracker.last_update) {
                        let energy_delta_mj = current_energy - last_energy;
                        let time_delta_secs = now.duration_since(last_update).as_secs_f64();

                        if time_delta_secs > 0.0 && energy_delta_mj >= 0 {
                            // Power (mW) = Energy (mJ) / Time (s)
                            let power_mw = (energy_delta_mj as f64 / time_delta_secs) as i64;
                            let _ = GPU_POWER_USAGE.set(0, power_mw);
                            debug!("GPU power: {} mW, energy: {} mJ", power_mw, current_energy);
                        }
                    }

                    tracker.last_energy = Some(current_energy);
                    tracker.last_update = Some(now);
                }
            }
        }
    }

    Ok(())
}

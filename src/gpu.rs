use anyhow::{Context, Result, anyhow};
use nvml_wrapper::Nvml;
use std::ffi::OsStr;
use std::path::Path;
use tracing::warn;

const NVML_TRUSTED_LIB_PATHS: &[&str] = &[
    "/lib/x86_64-linux-gnu/libnvidia-ml.so.1",
    "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1",
    "/lib/x86_64-linux-gnu/libnvidia-ml.so",
    "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so",
    "/usr/lib/libnvidia-ml.so.1",
    "/usr/lib/libnvidia-ml.so",
    "/usr/local/lib/libnvidia-ml.so.1",
    "/usr/local/lib/libnvidia-ml.so",
    "/usr/lib/x86_64-linux-gnu/nvidia/current/libnvidia-ml.so.1",
    "/usr/lib/x86_64-linux-gnu/nvidia/current/libnvidia-ml.so",
    "/lib64/libnvidia-ml.so.1",
    "/usr/lib64/libnvidia-ml.so.1",
    "/lib64/libnvidia-ml.so",
    "/usr/lib64/libnvidia-ml.so",
    "/lib/aarch64-linux-gnu/libnvidia-ml.so.1",
    "/usr/lib/aarch64-linux-gnu/libnvidia-ml.so.1",
    "/lib/aarch64-linux-gnu/libnvidia-ml.so",
    "/usr/lib/aarch64-linux-gnu/libnvidia-ml.so",
    "/usr/lib/nvidia/current/libnvidia-ml.so.1",
    "/usr/lib/nvidia/current/libnvidia-ml.so",
    "/usr/lib/wsl/lib/libnvidia-ml.so.1",
    "/usr/lib/wsl/lib/libnvidia-ml.so",
];

#[derive(Debug, Clone)]
pub struct GpuSample {
    pub index: u32,
    pub uuid: String,
    pub name: String,
    pub gpu_util_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
}

impl GpuSample {
    pub fn memory_util_percent(&self) -> f64 {
        if self.memory_total_bytes == 0 {
            return 0.0;
        }

        (self.memory_used_bytes as f64 / self.memory_total_bytes as f64) * 100.0
    }
}

pub trait GpuSampler: Send + Sync {
    fn sample_all(&self) -> Result<Vec<GpuSample>>;
}

pub struct NvmlSampler {
    nvml: Nvml,
}

impl NvmlSampler {
    pub fn new() -> Result<Self> {
        let nvml = init_nvml().context("failed to initialize NVML")?;
        Ok(Self { nvml })
    }
}

fn init_nvml() -> Result<Nvml> {
    let mut attempts = Vec::new();

    for &lib_path in NVML_TRUSTED_LIB_PATHS {
        if !Path::new(lib_path).exists() {
            attempts.push(format!("{lib_path} (not found)"));
            continue;
        }

        let mut builder = Nvml::builder();
        builder.lib_path(OsStr::new(lib_path));

        match builder.init() {
            Ok(nvml) => return Ok(nvml),
            Err(error) => attempts.push(format!("{lib_path} ({error})")),
        }
    }

    Err(anyhow!(
        "failed to initialize NVML from trusted absolute paths: {}",
        attempts.join(", ")
    ))
}

impl GpuSampler for NvmlSampler {
    fn sample_all(&self) -> Result<Vec<GpuSample>> {
        let count = self
            .nvml
            .device_count()
            .context("failed to query GPU device count from NVML")?;

        let mut samples = Vec::with_capacity(count as usize);

        for index in 0..count {
            match sample_device(&self.nvml, index) {
                Ok(sample) => samples.push(sample),
                Err(err) => {
                    warn!(
                        gpu_index = index,
                        error = ?err,
                        "failed to sample single GPU, skipping this device for current cycle"
                    );
                }
            }
        }

        Ok(samples)
    }
}

fn sample_device(nvml: &Nvml, index: u32) -> Result<GpuSample> {
    let device = nvml
        .device_by_index(index)
        .with_context(|| format!("failed to get GPU device by index: {}", index))?;

    let utilization = device
        .utilization_rates()
        .with_context(|| format!("failed to query utilization for GPU index {}", index))?;

    let memory = device
        .memory_info()
        .with_context(|| format!("failed to query memory info for GPU index {}", index))?;

    let uuid = device
        .uuid()
        .with_context(|| format!("failed to query uuid for GPU index {}", index))?;

    let name = device
        .name()
        .with_context(|| format!("failed to query name for GPU index {}", index))?;

    Ok(GpuSample {
        index,
        uuid,
        name,
        gpu_util_percent: utilization.gpu as f64,
        memory_used_bytes: memory.used,
        memory_total_bytes: memory.total,
    })
}

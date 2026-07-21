use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "macos")]
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const DEFAULT_MAX_BYTES: u64 = 20 * 1024 * 1024;
pub const DEFAULT_FILE_COUNT: usize = 10;
pub const DEFAULT_TOTAL_BYTES: u64 = DEFAULT_MAX_BYTES * DEFAULT_FILE_COUNT as u64;
pub const LATENCY_BUCKET_UPPER_US: [u64; 9] = [
    250, 500, 1_000, 2_000, 5_000, 10_000, 25_000, 50_000, 100_000,
];

#[derive(Clone)]
pub struct LatencyHistogram(Arc<LatencyHistogramInner>);

struct LatencyHistogramInner {
    buckets: [AtomicU64; 10],
    samples: AtomicU64,
    max_us: AtomicU64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self(Arc::new(LatencyHistogramInner {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            samples: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
        }))
    }
}

impl LatencyHistogram {
    pub fn record(&self, duration: std::time::Duration) {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        let bucket = LATENCY_BUCKET_UPPER_US
            .iter()
            .position(|upper| micros <= *upper)
            .unwrap_or(LATENCY_BUCKET_UPPER_US.len());
        self.0.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.0.samples.fetch_add(1, Ordering::Relaxed);
        self.0.max_us.fetch_max(micros, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LatencyHistogramSnapshot {
        let counts: Vec<u64> = self
            .0
            .buckets
            .iter()
            .map(|bucket| bucket.load(Ordering::Relaxed))
            .collect();
        let samples = self.0.samples.load(Ordering::Relaxed);
        let target = samples.saturating_mul(99).saturating_add(99) / 100;
        let mut cumulative = 0u64;
        let mut p99_us = 0;
        for (index, count) in counts.iter().enumerate() {
            cumulative = cumulative.saturating_add(*count);
            if target > 0 && cumulative >= target {
                p99_us = LATENCY_BUCKET_UPPER_US
                    .get(index)
                    .copied()
                    .unwrap_or_else(|| self.0.max_us.load(Ordering::Relaxed));
                break;
            }
        }
        LatencyHistogramSnapshot {
            samples,
            p99_us,
            max_us: self.0.max_us.load(Ordering::Relaxed),
            bucket_upper_us: LATENCY_BUCKET_UPPER_US.to_vec(),
            bucket_counts: counts,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LatencyHistogramSnapshot {
    pub samples: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub bucket_upper_us: Vec<u64>,
    pub bucket_counts: Vec<u64>,
}

#[derive(Clone)]
pub struct Diagnostics {
    inner: Arc<Mutex<RotatingWriter>>,
    directory: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DiagnosticWriteMetrics {
    pub duration_us: u64,
    pub rotated: bool,
}

impl Diagnostics {
    pub fn open(directory: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_limits(directory, DEFAULT_MAX_BYTES, DEFAULT_FILE_COUNT)
    }

    pub fn with_limits(
        directory: impl Into<PathBuf>,
        max_bytes: u64,
        file_count: usize,
    ) -> io::Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        let writer = RotatingWriter::open(directory.clone(), max_bytes, file_count)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(writer)),
            directory,
        })
    }

    pub fn record(&self, kind: &str, fields: Value) -> io::Result<()> {
        self.record_with_metrics(kind, fields).map(|_| ())
    }

    pub fn record_with_metrics(
        &self,
        kind: &str,
        fields: Value,
    ) -> io::Result<DiagnosticWriteMetrics> {
        let event = json!({
            "timestamp_unix_ms": unix_millis(),
            "kind": kind,
            "fields": redact_value(fields),
        });
        let mut encoded = serde_json::to_vec(&event).map_err(io::Error::other)?;
        encoded.push(b'\n');
        let started = Instant::now();
        let rotated = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("diagnostics lock poisoned"))?
            .write_record(&encoded)?;
        Ok(DiagnosticWriteMetrics {
            duration_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            rotated,
        })
    }

    /// Creates a local JSONL support bundle. No network operation is performed.
    pub fn export(&self, destination: impl AsRef<Path>) -> io::Result<PathBuf> {
        let mut destination = destination.as_ref().to_path_buf();
        if destination.is_dir() {
            destination.push(format!("gnirehtet-vd-support-{}.jsonl", unix_millis()));
        }
        let mut writer = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("diagnostics lock poisoned"))?;
        if let Some(file) = writer.file.as_mut() {
            file.flush()?;
        }
        let file_count = writer.file_count;
        if (0..file_count).any(|index| destination == log_path(&self.directory, index)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "support bundle destination must not overwrite a diagnostics log",
            ));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let output = File::create(&destination)?;
        let mut output = BufWriter::new(output);
        serde_json::to_writer(
            &mut output,
            &json!({
                "kind": "bundle_metadata",
                "created_unix_ms": unix_millis(),
                "format": 1,
                "redacted": true,
                "packet_payloads_recorded": false,
            }),
        )
        .map_err(io::Error::other)?;
        output.write_all(b"\n")?;

        // Export oldest first. Every line is parsed and redacted again so an
        // older producer cannot smuggle a secret into a manually exported file.
        for index in (0..file_count).rev() {
            let path = log_path(&self.directory, index);
            let Ok(file) = File::open(path) else {
                continue;
            };
            for line in BufReader::new(file).lines() {
                let line = line?;
                let value = serde_json::from_str(&line)
                    .map(redact_value)
                    .unwrap_or_else(|_| json!({"kind": "malformed_log_line"}));
                serde_json::to_writer(&mut output, &value).map_err(io::Error::other)?;
                output.write_all(b"\n")?;
            }
        }
        output.flush()?;
        drop(writer);
        Ok(destination)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

struct RotatingWriter {
    directory: PathBuf,
    file: Option<File>,
    bytes: u64,
    max_bytes: u64,
    file_count: usize,
}

impl RotatingWriter {
    fn open(directory: PathBuf, max_bytes: u64, file_count: usize) -> io::Result<Self> {
        if max_bytes == 0 || file_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rotation bounds must be non-zero",
            ));
        }
        prune_excess_logs(&directory, file_count)?;
        let path = log_path(&directory, 0);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            directory,
            file: Some(file),
            bytes,
            max_bytes,
            file_count,
        })
    }

    fn write_record(&mut self, record: &[u8]) -> io::Result<bool> {
        let mut rotated = false;
        if self.bytes > 0 && self.bytes.saturating_add(record.len() as u64) > self.max_bytes {
            self.rotate()?;
            rotated = true;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("diagnostics writer is closed"))?;
        file.write_all(record)?;
        file.flush()?;
        self.bytes = self.bytes.saturating_add(record.len() as u64);
        Ok(rotated)
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            drop(file);
        }
        if self.file_count > 1 {
            let oldest = log_path(&self.directory, self.file_count - 1);
            match fs::remove_file(oldest) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            for index in (0..self.file_count - 1).rev() {
                let source = log_path(&self.directory, index);
                let target = log_path(&self.directory, index + 1);
                match fs::rename(source, target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        } else {
            match fs::remove_file(log_path(&self.directory, 0)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path(&self.directory, 0))?,
        );
        self.bytes = 0;
        Ok(())
    }
}

fn log_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("gnirehtet-vd.{index}.jsonl"))
}

fn prune_excess_logs(directory: &Path, file_count: usize) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(index) = name
            .strip_prefix("gnirehtet-vd.")
            .and_then(|value| value.strip_suffix(".jsonl"))
            .and_then(|value| value.parse::<usize>().ok())
        else {
            continue;
        };
        if index >= file_count {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(redact_object(object)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_value).collect()),
        Value::String(value) => Value::String(redact_text(&value)),
        other => other,
    }
}

fn redact_object(object: Map<String, Value>) -> Map<String, Value> {
    object
        .into_iter()
        .map(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            let normalized: String = lower
                .bytes()
                .filter(|byte| byte.is_ascii_alphanumeric())
                .map(char::from)
                .collect();
            let sensitive = [
                "payload",
                "secret",
                "password",
                "token",
                "sessionid",
                "serial",
                "username",
                "userid",
                "address",
                "endpoint",
                "filepath",
            ]
            .iter()
            .any(|marker| normalized.contains(marker))
                || normalized == "err"
                || (normalized.contains("error")
                    && !matches!(normalized.as_str(), "errorkind" | "errorcategory"));
            if sensitive {
                (key, Value::String("<redacted>".into()))
            } else {
                (key, redact_value(value))
            }
        })
        .collect()
}

fn redact_text(input: &str) -> String {
    let mut output = input.to_owned();
    for variable in ["USERPROFILE", "HOME", "USERNAME", "USER"] {
        if let Some(value) = env::var_os(variable).and_then(|value| value.into_string().ok()) {
            if !value.is_empty() {
                output = output.replace(&value, "<home>");
            }
        }
    }
    output
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProcessSample {
    pub rss_bytes: Option<u64>,
    pub cpu_total_millis: Option<u64>,
}

#[cfg(target_os = "linux")]
pub fn process_sample() -> ProcessSample {
    let rss_bytes = fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages.saturating_mul(4096));
    ProcessSample {
        rss_bytes,
        cpu_total_millis: None,
    }
}

#[cfg(target_os = "windows")]
pub fn process_sample() -> ProcessSample {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };

    let mut memory = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    let memory_size = memory.cb;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle for this process,
    // and every output pointer references an initialized, correctly sized value.
    let (memory_ok, times_ok) = unsafe {
        let process = GetCurrentProcess();
        (
            GetProcessMemoryInfo(process, &mut memory, memory_size) != 0,
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0,
        )
    };
    let cpu_total_millis =
        times_ok.then(|| filetime_ticks(kernel).saturating_add(filetime_ticks(user)) / 10_000);
    ProcessSample {
        rss_bytes: memory_ok.then_some(memory.WorkingSetSize as u64),
        cpu_total_millis,
    }
}

#[cfg(target_os = "windows")]
fn filetime_ticks(value: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(target_os = "macos")]
pub fn process_sample() -> ProcessSample {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok();
    ProcessSample {
        rss_bytes: output
            .as_ref()
            .and_then(|value| String::from_utf8(value.stdout.clone()).ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|kib| kib.saturating_mul(1024)),
        cpu_total_millis: None,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn process_sample() -> ProcessSample {
    ProcessSample::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retention_is_bounded_to_200_mib() {
        assert_eq!(DEFAULT_MAX_BYTES, 20 * 1024 * 1024);
        assert_eq!(DEFAULT_FILE_COUNT, 10);
        assert_eq!(DEFAULT_TOTAL_BYTES, 200 * 1024 * 1024);
    }

    #[test]
    fn write_metrics_report_rotation_and_open_prunes_excess_generations() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(log_path(directory.path(), 4), b"stale").unwrap();
        let diagnostics = Diagnostics::with_limits(directory.path(), 120, 2).unwrap();
        assert!(!log_path(directory.path(), 4).exists());

        let mut observed_rotation = false;
        for index in 0..10 {
            observed_rotation |= diagnostics
                .record_with_metrics("rotation_test", json!({"index": index}))
                .unwrap()
                .rotated;
        }
        assert!(observed_rotation);
        assert!(log_path(directory.path(), 1).exists());
        assert!(!log_path(directory.path(), 2).exists());
    }

    #[test]
    fn rotates_and_redacts_export() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::with_limits(directory.path(), 180, 3).unwrap();
        for index in 0..20 {
            diagnostics
                .record(
                    "test",
                    json!({"index": index, "session_id": "do-not-export", "payload": [1, 2]}),
                )
                .unwrap();
        }
        assert!(log_path(directory.path(), 1).exists());
        let bundle = diagnostics
            .export(directory.path().join("bundle.jsonl"))
            .unwrap();
        let contents = fs::read_to_string(bundle).unwrap();
        assert!(!contents.contains("do-not-export"));
        assert!(!contents.contains("[1,2]"));
        assert!(contents.contains("<redacted>"));
    }

    #[test]
    fn export_uses_configured_rotation_count_and_redacts_key_variants() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::with_limits(directory.path(), 1024, 7).unwrap();
        fs::write(
            log_path(directory.path(), 6),
            "{\"kind\":\"oldest_marker\",\"sessionId\":\"private-session\",\"remoteAddress\":\"192.0.2.1\",\"deviceSerial\":\"private-device\"}\n",
        )
        .unwrap();
        diagnostics
            .record(
                "safe_counters",
                json!({
                    "tx_packets": 42,
                    "reverse_mapping_error": "device private-device at C:\\private\\tool failed"
                }),
            )
            .unwrap();
        let bundle = diagnostics
            .export(directory.path().join("bundle.jsonl"))
            .unwrap();
        let contents = fs::read_to_string(bundle).unwrap();
        assert!(contents.contains("oldest_marker"));
        assert!(!contents.contains("private-session"));
        assert!(!contents.contains("192.0.2.1"));
        assert!(!contents.contains("private-device"));
        assert!(contents.contains("tx_packets"));
        assert!(contents.contains("42"));
        assert!(!contents.contains("C:\\\\private"));
    }

    #[test]
    fn export_cannot_overwrite_an_active_log() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::with_limits(directory.path(), 1024, 2).unwrap();
        let error = diagnostics
            .export(log_path(directory.path(), 0))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn latency_histogram_exposes_p99_max_and_fixed_buckets() {
        let histogram = LatencyHistogram::default();
        for micros in [100, 200, 300, 900, 1_500, 7_000, 40_000, 120_000] {
            histogram.record(std::time::Duration::from_micros(micros));
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.samples, 8);
        assert_eq!(snapshot.max_us, 120_000);
        assert_eq!(snapshot.p99_us, 120_000);
        assert_eq!(snapshot.bucket_counts.len(), 10);
    }
}

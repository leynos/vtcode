//! Standalone process benchmark for Diapason's release startup paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tempfile::{Builder, TempDir};

#[derive(Debug, Clone, Copy)]
struct BenchmarkCase {
    name: &'static str,
    args: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
enum LaunchMode {
    Warm,
    Cold,
}

impl LaunchMode {
    const ALL: &'static [Self] = &[Self::Warm, Self::Cold];

    const fn name(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}

const CASES: &[BenchmarkCase] = &[
    BenchmarkCase { name: "version", args: &["--version"] },
    BenchmarkCase { name: "help", args: &["--help"] },
    BenchmarkCase {
        name: "schema_tools_ndjson",
        args: &["schema", "tools", "--format", "ndjson", "--name", "code_search"],
    },
];

struct IsolatedEnvironment {
    _root: TempDir,
    workspace: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    legacy: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
    bin: PathBuf,
    temp: PathBuf,
    config_file: PathBuf,
    codex_home: PathBuf,
}

impl IsolatedEnvironment {
    fn new() -> Result<Self> {
        let root = Builder::new()
            .prefix("diapason-startup-bench-")
            .tempdir()
            .context("failed to create startup benchmark temporary root")?;

        let workspace = root.path().join("workspace");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        let legacy = root.path().join("legacy");
        let state = root.path().join("state");
        let cache = root.path().join("cache");
        let runtime = root.path().join("runtime");
        let bin = root.path().join("bin");
        let temp = root.path().join("tmp");
        let config_file = root.path().join("explicit-config").join("config.toml");
        let codex_home = root.path().join("codex-home");

        for directory in [
            &workspace,
            &home,
            &config,
            &data,
            &legacy,
            &state,
            &cache,
            &runtime,
            &bin,
            &temp,
            &codex_home,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("failed to create benchmark directory {}", directory.display()))?;
        }
        if let Some(parent) = config_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create explicit config directory {}", parent.display()))?;
        }
        fs::write(&config_file, b"")
            .with_context(|| format!("failed to create explicit benchmark config {}", config_file.display()))?;

        Ok(Self {
            _root: root,
            workspace,
            home,
            config,
            data,
            legacy,
            state,
            cache,
            runtime,
            bin,
            temp,
            config_file,
            codex_home,
        })
    }

    fn command(&self, executable: &Path, case: BenchmarkCase) -> Command {
        let mut command = Command::new(executable);
        let _ = command
            .args(case.args)
            .current_dir(&self.workspace)
            .env_clear()
            .env("HOME", &self.home)
            .env("DIAPASON_CONFIG", &self.config)
            .env("DIAPASON_DATA", &self.data)
            .env("DIAPASON_HOME", &self.legacy)
            .env("DIAPASON_CONFIG_PATH", &self.config_file)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_BIN_HOME", &self.bin)
            .env("CODEX_HOME", &self.codex_home)
            .env("USER", "diapason-benchmark")
            .env("LOGNAME", "diapason-benchmark")
            .env("TMPDIR", &self.temp)
            .env("TMP", &self.temp)
            .env("TEMP", &self.temp)
            .env("TERM", "dumb")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("DIAPASON_STARTUP_TRACE", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(path) = std::env::var_os("PATH") {
            let _ = command.env("PATH", path);
        }

        command
    }
}

fn main() -> Result<()> {
    // `cargo nextest run --all-targets` probes every test target with the
    // standard harness listing arguments. This standalone process benchmark
    // has no tests to enumerate, so answer that probe without launching VT
    // Code or requiring a release binary.
    if std::env::args().any(|argument| argument == "--list") {
        return Ok(());
    }

    let executable = resolve_executable()?;
    let sample_count = resolve_sample_count()?;

    for case in CASES.iter().copied() {
        for mode in LaunchMode::ALL.iter().copied() {
            benchmark_case(&executable, case, mode, sample_count)
                .with_context(|| format!("failed to benchmark case '{}' in {} mode", case.name, mode.name()))?;
        }
    }

    Ok(())
}

fn resolve_executable() -> Result<PathBuf> {
    let configured = std::env::var_os("DIAPASON_BIN");
    let path = match configured {
        Some(value) if value.is_empty() => bail!("DIAPASON_BIN must not be empty"),
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join(format!("diapason{}", std::env::consts::EXE_SUFFIX)),
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("failed to resolve the benchmark working directory")?
            .join(path)
    };

    let metadata = fs::metadata(&path)
        .with_context(|| format!("DIAPASON_BIN does not point to a readable executable: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("DIAPASON_BIN is not a regular file: {}", path.display());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("DIAPASON_BIN is not executable: {}", path.display());
        }
    }

    Ok(path)
}

fn resolve_sample_count() -> Result<usize> {
    let raw = match std::env::var("DIAPASON_BENCH_RUNS") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "5".to_owned(),
        Err(error) => {
            return Err(anyhow::anyhow!("DIAPASON_BENCH_RUNS could not be read as UTF-8: {error}"));
        }
    };
    let sample_count = raw
        .trim()
        .parse::<usize>()
        .with_context(|| format!("DIAPASON_BENCH_RUNS must be a positive integer, got '{raw}'"))?;
    if sample_count == 0 {
        bail!("DIAPASON_BENCH_RUNS must be greater than zero");
    }
    Ok(sample_count)
}

fn benchmark_case(binary: &Path, case: BenchmarkCase, mode: LaunchMode, sample_count: usize) -> Result<()> {
    if matches!(mode, LaunchMode::Warm) {
        let _ = run_sample(binary, case, mode).context("warm-up launch failed")?;
    }

    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        samples.push(run_sample(binary, case, mode)?);
    }
    samples.sort_by(f64::total_cmp);

    let median = median(&samples)?;
    let p95 = nearest_rank_percentile(&samples, 95, 100)?;
    let raw_samples = samples
        .iter()
        .map(|sample| format!("{sample:.3}"))
        .collect::<Vec<_>>()
        .join(",");

    println!(
        "startup case={} mode={} sample_count={} median_ms={median:.3} p95_ms={p95:.3} raw_ms=[{raw_samples}]",
        case.name,
        mode.name(),
        samples.len(),
    );
    Ok(())
}

fn run_sample(binary: &Path, case: BenchmarkCase, mode: LaunchMode) -> Result<f64> {
    let environment = IsolatedEnvironment::new()?;
    let executable = match mode {
        LaunchMode::Warm => binary.to_path_buf(),
        LaunchMode::Cold => copy_for_cold_launch(binary, &environment)?,
    };

    let started = Instant::now();
    let status = environment
        .command(&executable, case)
        .status()
        .with_context(|| format!("failed to launch {}", case.args.join(" ")))?;
    if !status.success() {
        bail!("{} exited unsuccessfully while running {}: {status}", executable.display(), case.args.join(" "),);
    }

    Ok(started.elapsed().as_secs_f64() * 1_000.0)
}

fn copy_for_cold_launch(binary: &Path, environment: &IsolatedEnvironment) -> Result<PathBuf> {
    let cold_binary = environment.temp.join("diapason-cold-copy");
    let _bytes_copied = fs::copy(binary, &cold_binary).with_context(|| {
        format!("failed to copy release executable {} to {}", binary.display(), cold_binary.display())
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let source_mode = fs::metadata(binary)
            .with_context(|| format!("failed to inspect executable {}", binary.display()))?
            .permissions()
            .mode();
        let mut destination_permissions = fs::metadata(&cold_binary)
            .with_context(|| format!("failed to inspect cold executable {}", cold_binary.display()))?
            .permissions();
        destination_permissions.set_mode(source_mode);
        fs::set_permissions(&cold_binary, destination_permissions)
            .with_context(|| format!("failed to preserve executable permissions on {}", cold_binary.display()))?;
    }

    Ok(cold_binary)
}

fn median(samples: &[f64]) -> Result<f64> {
    let sample_count = samples.len();
    if sample_count == 0 {
        bail!("cannot calculate a median for an empty sample set");
    }

    if sample_count % 2 == 1 {
        samples
            .get(sample_count / 2)
            .copied()
            .context("median index was outside the sample set")
    } else {
        let lower = samples
            .get(sample_count / 2 - 1)
            .copied()
            .context("lower median index was outside the sample set")?;
        let upper = samples
            .get(sample_count / 2)
            .copied()
            .context("upper median index was outside the sample set")?;
        Ok((lower + upper) / 2.0)
    }
}

fn nearest_rank_percentile(samples: &[f64], numerator: usize, denominator: usize) -> Result<f64> {
    if samples.is_empty() {
        bail!("cannot calculate a percentile for an empty sample set");
    }
    if numerator == 0 || denominator == 0 || numerator > denominator {
        bail!("percentile fraction must be between zero and one");
    }

    let rank = samples
        .len()
        .checked_mul(numerator)
        .context("percentile rank overflowed")?
        .div_ceil(denominator);
    let index = rank.checked_sub(1).context("percentile rank was zero")?;
    samples
        .get(index)
        .copied()
        .context("percentile index was outside the sample set")
}

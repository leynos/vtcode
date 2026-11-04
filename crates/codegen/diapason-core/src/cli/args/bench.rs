/// Arguments for `diapason bench-allocator` — see the command docs for meaning.
#[derive(Debug, Clone, clap::Args)]
pub struct BenchAllocatorArgs {
    /// Number of bursts to run
    #[arg(long, default_value_t = 3)]
    pub bursts: usize,
    /// Concurrent tasks per burst (Semaphore cap)
    #[arg(long, default_value_t = 30)]
    pub concurrency: usize,
    /// Tokens allocated per task (each roughly up to 1KB)
    #[arg(long, default_value_t = 200)]
    pub tokens_per_task: usize,
    /// Idle seconds between bursts (lets Tokio workers go idle)
    #[arg(long, default_value_t = 2)]
    pub idle_seconds: u64,
    /// Payload size per task in bytes
    #[arg(long, default_value_t = 4096)]
    pub payload_bytes: usize,
}

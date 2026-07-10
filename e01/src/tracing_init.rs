use tracing_subscriber::{
    EnvFilter, fmt::time::UtcTime, layer::SubscriberExt, util::SubscriberInitExt,
};

const DEFAULT_FILTER: &str = "info,foyer=warn,foyer_memory=warn,foyer_storage=warn";

/// Initialize stderr logging with RFC3339 timestamps and `RUST_LOG` support.
pub fn init() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| DEFAULT_FILTER.into()))
        .with(
            tracing_subscriber::fmt::layer()
                .with_timer(UtcTime::rfc_3339())
                .with_file(false)
                .with_line_number(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_writer(std::io::stderr),
        )
        .init();
}

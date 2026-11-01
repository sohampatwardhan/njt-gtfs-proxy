//! Process entry point for the `njt-gtfs-proxy` binary.
//!
//! Configuration is read from the environment. Failures are printed to stderr
//! without credentials or tokens, and the process exits with status 1.

#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(clippy::missing_errors_doc)]
#![deny(clippy::missing_panics_doc)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]

/// Starts the proxy.
///
/// # Arguments
///
/// This function takes no arguments. Settings come from the environment
/// documented on [`njt_gtfs_proxy::Config`].
///
/// # Returns
///
/// Nothing on the success path. The process stays running until it is signalled.
///
/// # Errors
///
/// This function does not return an error. Configuration and bind failures are
/// printed and the process exits with status 1.
///
/// # Panics
///
/// The Tokio runtime attribute panics if a runtime cannot be created. The proxy
/// itself does not panic on bad configuration.
///
/// # Examples
///
/// ```no_run
/// # async fn demo() -> Result<(), njt_gtfs_proxy::ConfigError> {
/// njt_gtfs_proxy::run_from_env().await
/// # }
/// ```
#[tokio::main]
async fn main() {
    if let Err(err) = njt_gtfs_proxy::run_from_env().await {
        eprintln!("njt-gtfs-proxy: {err}");
        std::process::exit(1);
    }
}

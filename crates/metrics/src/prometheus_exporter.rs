//! Prometheus exporter
//! Adapted from Paradigm's [`reth`](https://github.com/paradigmxyz/reth/blob/c1d7d2bde398bcf410c7e2df13fd7151fc2a58b9/bin/reth/src/prometheus_exporter.rs)
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use metrics::{describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use metrics_util::layers::{PrefixLayer, Stack};

pub(crate) trait Hook: Fn() + Send + Sync {}
impl<T: Fn() + Send + Sync> Hook for T {}

/// Installs Prometheus as the metrics recorder.
pub fn install_recorder() -> anyhow::Result<PrometheusHandle> {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();

    // Build metrics stack
    Stack::new(recorder)
        .push(PrefixLayer::new("katana"))
        .install()
        .map_err(|e| anyhow::anyhow!("Couldn't set metrics recorder: {}", e))?;

    Ok(handle)
}

/// Serves Prometheus metrics over HTTP with hooks.
///
/// The hooks are called every time the metrics are requested at the given endpoint, and can be used
/// to record values for pull-style metrics, i.e. metrics that are not automatically updated.
pub(crate) async fn serve_with_hooks<F: Hook + 'static>(
    listen_addr: SocketAddr,
    handle: PrometheusHandle,
    hooks: impl IntoIterator<Item = F>,
) -> anyhow::Result<()> {
    let hooks: Vec<_> = hooks.into_iter().collect();

    // Start endpoint
    start_endpoint(listen_addr, handle, Arc::new(move || hooks.iter().for_each(|hook| hook())))
        .await
        .map_err(|e| anyhow::anyhow!("Could not start Prometheus endpoint: {}", e))?;

    Ok(())
}

/// Starts an endpoint at the given address to serve Prometheus metrics.
async fn start_endpoint<F: Hook + 'static>(
    listen_addr: SocketAddr,
    handle: PrometheusHandle,
    hook: Arc<F>,
) -> anyhow::Result<()> {
    let make_svc = make_service_fn(move |_| {
        let handle = handle.clone();
        let hook = Arc::clone(&hook);
        async move {
            Ok::<_, Infallible>(service_fn(move |_: Request<Body>| {
                (hook)();
                let metrics = handle.render();
                async move { Ok::<_, Infallible>(Response::new(Body::from(metrics))) }
            }))
        }
    });
    let server = Server::try_bind(&listen_addr)
        .map_err(|e| anyhow::anyhow!("Could not bind to address: {}", e))?
        .serve(make_svc);

    tokio::spawn(async move { server.await.expect("Metrics endpoint crashed") });

    Ok(())
}

/// Serves Prometheus metrics over HTTP with database and process metrics.
pub async fn serve(
    listen_addr: SocketAddr,
    handle: PrometheusHandle,
    process: metrics_process::Collector,
) -> anyhow::Result<()> {
    // Clone `process` to move it into the hook and use the original `process` for describe below.
    let cloned_process = process.clone();
    let hooks: Vec<Box<dyn Hook<Output = ()>>> =
        vec![Box::new(move || cloned_process.collect()), Box::new(collect_memory_stats)];
    serve_with_hooks(listen_addr, handle, hooks).await?;

    process.describe();
    describe_memory_stats();

    Ok(())
}

#[cfg(all(feature = "jemalloc", unix))]
fn collect_memory_stats() {
    use jemalloc_ctl::{epoch, stats};

    if epoch::advance()
        .map_err(|error| tracing::error!(?error, "Failed to advance jemalloc epoch"))
        .is_err()
    {
        return;
    }

    if let Ok(value) = stats::active::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.active"))
    {
        gauge!("jemalloc.active", value as f64);
    }

    if let Ok(value) = stats::allocated::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.allocated"))
    {
        gauge!("jemalloc.allocated", value as f64);
    }

    if let Ok(value) = stats::mapped::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.mapped"))
    {
        gauge!("jemalloc.mapped", value as f64);
    }

    if let Ok(value) = stats::metadata::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.metadata"))
    {
        gauge!("jemalloc.metadata", value as f64);
    }

    if let Ok(value) = stats::resident::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.resident"))
    {
        gauge!("jemalloc.resident", value as f64);
    }

    if let Ok(value) = stats::retained::read()
        .map_err(|error| tracing::error!(?error, "Failed to read jemalloc.stats.retained"))
    {
        gauge!("jemalloc.retained", value as f64);
    }
}

#[cfg(all(feature = "jemalloc", unix))]
fn describe_memory_stats() {
    describe_gauge!(
        "jemalloc.active",
        metrics::Unit::Bytes,
        "Total number of bytes in active pages allocated by the application"
    );
    describe_gauge!(
        "jemalloc.allocated",
        metrics::Unit::Bytes,
        "Total number of bytes allocated by the application"
    );
    describe_gauge!(
        "jemalloc.mapped",
        metrics::Unit::Bytes,
        "Total number of bytes in active extents mapped by the allocator"
    );
    describe_gauge!(
        "jemalloc.metadata",
        metrics::Unit::Bytes,
        "Total number of bytes dedicated to jemalloc metadata"
    );
    describe_gauge!(
        "jemalloc.resident",
        metrics::Unit::Bytes,
        "Total number of bytes in physically resident data pages mapped by the allocator"
    );
    describe_gauge!(
        "jemalloc.retained",
        metrics::Unit::Bytes,
        "Total number of bytes in virtual memory mappings that were retained rather than being \
         returned to the operating system via e.g. munmap(2)"
    );
}

#[cfg(not(all(feature = "jemalloc", unix)))]
fn collect_memory_stats() {}

#[cfg(not(all(feature = "jemalloc", unix)))]
fn describe_memory_stats() {}

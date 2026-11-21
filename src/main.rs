mod app;
mod backups;
mod config;
mod error;
mod files;
mod fs;
mod install;
mod instances;
mod modpacks;
mod properties;
mod routes;
mod server;
mod share;
mod web;

use std::{net::SocketAddr, sync::Arc};

use tokio::signal::unix::{signal, SignalKind};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cfg = config::Config::from_env()?;
    let app = app::App::init(cfg).await?;
    app.boot();

    // Surface joueurs : uniquement les pages de téléchargement des packs.
    let share_listener = tokio::net::TcpListener::bind(app.cfg.share_listen).await?;
    tracing::info!("packs clients sur http://{} ({})", app.cfg.share_listen, app.cfg.share_url);
    let share_app = app.clone();
    tokio::spawn(async move {
        let shutdown = share_app.shutdown.clone();
        let r = axum::serve(share_listener, share::public_router(share_app))
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await;
        if let Err(e) = r {
            tracing::error!("surface joueurs : {e}");
        }
    });

    let listener = tokio::net::TcpListener::bind(app.cfg.listen).await?;
    tracing::info!(
        "omgminecraft sur http://{} — données dans {}, sauvegardes dans {}",
        app.cfg.listen,
        app.cfg.data_dir.display(),
        app.cfg.backup_dir.display()
    );
    axum::serve(
        listener,
        routes::router(app.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown(app))
    .await?;
    Ok(())
}

/// SIGTERM (systemd) : on coupe les flux SSE, puis on arrête proprement le
/// serveur Minecraft sans l'oublier — il repartira au prochain démarrage.
async fn shutdown(app: Arc<app::App>) {
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("arrêt demandé");
    app.shutdown.cancel();
    app.stop_server(false).await;
}

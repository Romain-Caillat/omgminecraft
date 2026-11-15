use std::{convert::Infallible, net::IpAddr, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast::error::RecvError, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    app::App,
    error::{ApiResult, AppError},
    files, fs, install,
    instances::{now, InstallStatus, Instance, Kind},
    properties::Properties,
    web,
};

type AppState = State<Arc<App>>;

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/events", get(events))
        .route("/console", post(console))
        .route("/server/stop", post(stop))
        .route("/server/restart", post(restart))
        .route("/versions/{kind}", get(versions))
        .route("/instances", get(list).post(create))
        .route("/instances/{id}", get(get_one).patch(update).delete(remove))
        .route("/instances/{id}/start", post(start))
        .route("/instances/{id}/fs", get(fs::list).delete(fs::remove))
        .route("/instances/{id}/fs/content", get(fs::read).put(fs::write))
        .route("/instances/{id}/fs/mkdir", post(fs::mkdir))
        .route("/instances/{id}/fs/rename", post(fs::rename))
        .route("/instances/{id}/fs/download", get(fs::download))
        .route(
            "/instances/{id}/fs/upload",
            post(fs::upload).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/instances/{id}/mods",
            get(files::list_mods).post(files::upload_mods).layer(DefaultBodyLimit::disable()),
        )
        .route("/instances/{id}/mods/{file}", delete(files::delete_mod))
        .route("/instances/{id}/mods/{file}/toggle", post(files::toggle_mod))
        .route("/instances/{id}/worlds", get(files::list_worlds).post(files::create_world))
        .route(
            "/instances/{id}/worlds/upload",
            post(files::upload_world).layer(DefaultBodyLimit::disable()),
        )
        .route("/instances/{id}/worlds/{name}", delete(files::delete_world))
        .route("/instances/{id}/worlds/{name}/activate", post(files::activate_world))
        .route("/instances/{id}/worlds/{name}/download", get(files::download_world))
        .fallback(|| async { AppError::not_found("route inconnue") });

    Router::new()
        .nest("/api", api)
        .fallback(web::static_handler)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Le panel n'a pas de comptes : l'accès, c'est NetBird. On refuse donc tout
/// ce qui ne vient pas d'un réseau autorisé, le LAN en particulier.
async fn guard(
    State(app): AppState,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = match addr.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        ip => ip,
    };
    if app.cfg.allowed.iter().any(|n| n.contains(&ip)) {
        next.run(req).await
    } else {
        tracing::warn!("refusé : {ip} {}", req.uri());
        (StatusCode::FORBIDDEN, "accès réservé au VPN").into_response()
    }
}

async fn status(State(app): AppState) -> Json<Value> {
    Json(json!({ "server": app.server.status(), "active": app.persisted().active }))
}

/// Flux SSE : l'état du serveur (`status`) et chaque ligne de console (`log`).
async fn events(State(app): AppState) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(256);
    let (history, mut logs) = app.server.subscribe_logs();
    let mut st = app.server.watch_status();
    let shutdown = app.shutdown.clone();
    tokio::spawn(async move {
        let status_event = |s: crate::server::Status| Event::default().event("status").json_data(s).ok();
        let initial = status_event(st.borrow_and_update().clone());
        if let Some(e) = initial {
            if tx.send(Ok(e)).await.is_err() {
                return;
            }
        }
        let reset = Event::default().event("history").data(history.len().to_string());
        if tx.send(Ok(reset)).await.is_err() {
            return;
        }
        for l in history {
            if tx.send(Ok(Event::default().event("log").data(l))).await.is_err() {
                return;
            }
        }
        loop {
            let ev = tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tx.closed() => return,
                r = logs.recv() => match r {
                    Ok(l) => Event::default().event("log").data(l),
                    Err(RecvError::Lagged(n)) => Event::default().event("log").data(format!("[omg] … {n} lignes sautées")),
                    Err(RecvError::Closed) => return,
                },
                r = st.changed() => {
                    if r.is_err() { return }
                    let s = st.borrow_and_update().clone();
                    match status_event(s) { Some(e) => e, None => continue }
                }
            };
            if tx.send(Ok(ev)).await.is_err() {
                return;
            }
        }
    });
    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct ConsoleReq {
    command: String,
}

async fn console(State(app): AppState, Json(req): Json<ConsoleReq>) -> ApiResult<StatusCode> {
    let cmd = req.command.trim();
    if cmd.is_empty() || cmd.contains(['\n', '\r']) {
        return Err(AppError::bad("commande invalide"));
    }
    let cmd = cmd.strip_prefix('/').unwrap_or(cmd);
    app.server.log(format!("> {cmd}"));
    app.server.send_command(cmd).await.map_err(|e| AppError::conflict(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stop(State(app): AppState) -> StatusCode {
    app.stop_server(true).await;
    StatusCode::NO_CONTENT
}

async fn restart(State(app): AppState) -> ApiResult<StatusCode> {
    let id = app
        .server
        .running_instance()
        .or(app.persisted().active)
        .ok_or_else(|| AppError::conflict("aucune instance à redémarrer"))?;
    app.start_instance(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn versions(State(app): AppState, Path(kind): Path<String>) -> ApiResult<Json<Value>> {
    let kind = match kind.as_str() {
        "vanilla" => Kind::Vanilla,
        "fabric" => Kind::Fabric,
        _ => return Err(AppError::bad("type inconnu")),
    };
    let v = install::versions(&app.http, kind).await?;
    Ok(Json(json!(v)))
}

async fn list(State(app): AppState) -> ApiResult<Json<Vec<Instance>>> {
    Ok(Json(app.list_instances()?))
}

async fn get_one(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Instance>> {
    Ok(Json(app.load_instance(&id)?))
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    kind: Kind,
    mc_version: String,
    memory_max_mb: Option<u32>,
    eula: bool,
}

async fn create(State(app): AppState, Json(req): Json<CreateReq>) -> ApiResult<Json<Instance>> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err(AppError::bad("nom requis (60 caractères max)"));
    }
    if !req.eula {
        return Err(AppError::bad("il faut accepter l'EULA de Minecraft"));
    }
    let mc = req.mc_version.trim();
    if mc.is_empty() || mc.len() > 40 || !mc.chars().all(|c| c.is_ascii_graphic()) {
        return Err(AppError::bad("version invalide"));
    }
    let jar = match req.kind {
        Kind::Vanilla => "server.jar",
        Kind::Fabric => "fabric-server-launch.jar",
        Kind::Custom => return Err(AppError::bad("une instance personnalisée s'importe à la main")),
    };
    let max = req.memory_max_mb.unwrap_or(app.cfg.default_memory_mb);
    check_memory(max / 2, max)?;

    let id = app.unique_id(name);
    let dir = app.instance_dir(&id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("eula.txt"), "eula=true\n")?;
    let mut props = Properties::load(&dir.join("server.properties"))?;
    props.set("server-port", &app.cfg.game_port.to_string());
    props.set("motd", name);
    props.save(&dir.join("server.properties"))?;
    let inst = Instance {
        id: id.clone(),
        name: name.to_string(),
        kind: req.kind,
        mc_version: mc.to_string(),
        loader_version: None,
        jar: jar.into(),
        java_major: 0,
        memory_min_mb: (max / 2).max(512),
        memory_max_mb: max,
        jvm_args: vec![],
        status: InstallStatus::Installing,
        error: None,
        created_at: now(),
    };
    app.save_instance(&inst)?;
    tokio::spawn(install::install_instance(app.clone(), inst.clone()));
    Ok(Json(inst))
}

fn check_memory(min: u32, max: u32) -> ApiResult<()> {
    if !(512..=65536).contains(&max) || min > max {
        return Err(AppError::bad("mémoire : entre 512 et 65536 Mo, minimum ≤ maximum"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct UpdateReq {
    name: Option<String>,
    memory_min_mb: Option<u32>,
    memory_max_mb: Option<u32>,
    jvm_args: Option<Vec<String>>,
    java_major: Option<u32>,
}

async fn update(
    State(app): AppState,
    Path(id): Path<String>,
    Json(req): Json<UpdateReq>,
) -> ApiResult<Json<Instance>> {
    let cur = app.load_instance(&id)?;
    let min = req.memory_min_mb.unwrap_or(cur.memory_min_mb);
    let max = req.memory_max_mb.unwrap_or(cur.memory_max_mb);
    check_memory(min, max)?;
    if let Some(n) = &req.name {
        if n.trim().is_empty() || n.len() > 60 {
            return Err(AppError::bad("nom requis (60 caractères max)"));
        }
    }
    if let Some(j) = req.java_major {
        if !(8..=99).contains(&j) {
            return Err(AppError::bad("version de Java invalide"));
        }
    }
    if let Some(args) = &req.jvm_args {
        if args.iter().any(|a| a.is_empty() || !a.starts_with('-')) {
            return Err(AppError::bad("chaque argument JVM commence par « - »"));
        }
    }
    Ok(Json(app.update_instance(&id, |i| {
        if let Some(n) = req.name {
            i.name = n.trim().to_string();
        }
        i.memory_min_mb = min;
        i.memory_max_mb = max;
        if let Some(a) = req.jvm_args {
            i.jvm_args = a;
        }
        if let Some(j) = req.java_major {
            i.java_major = j;
        }
    })?))
}

async fn remove(State(app): AppState, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let inst = app.load_instance(&id)?;
    if app.server.running_instance().as_deref() == Some(id.as_str()) {
        return Err(AppError::conflict("arrête d'abord le serveur"));
    }
    if inst.status == InstallStatus::Installing {
        return Err(AppError::conflict("installation en cours"));
    }
    let dir = app.instance_dir(&id);
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await??;
    app.forget_instance(&id);
    app.server.log(format!("[omg] instance « {id} » supprimée"));
    Ok(StatusCode::NO_CONTENT)
}

async fn start(State(app): AppState, Path(id): Path<String>) -> ApiResult<StatusCode> {
    app.start_instance(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

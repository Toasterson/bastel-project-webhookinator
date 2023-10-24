use axum::{http::StatusCode, routing::post, Json, Router};
use config::File;
use deno_core::{serde_v8, JsRuntime};
use miette::Diagnostic;
use serde::Deserialize;
use std::net::AddrParseError;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    AddrParse(#[from] AddrParseError),
    #[error(transparent)]
    HyperError(#[from] hyper::Error),
    #[error(transparent)]
    DenoCore(#[from] deno_core::anyhow::Error),
    #[error(transparent)]
    DenoSerdeV8(#[from] deno_core::serde_v8::Error),

    #[error("body could not be passed to js runtime")]
    JSRuntimePassError,
}

pub type Result<T> = miette::Result<T, Error>;

#[derive(Debug, Deserialize)]
pub struct Config {
    listen: String,
}

pub async fn load_config() -> Result<Config> {
    let builder = config::Config::builder()
        .set_default("listen", "0.0.0.0:3000")?
        .add_source(File::with_name("/etc/whinator.yaml").required(false));
    let cfg = builder.build()?;

    tracing::debug!("Loaded Configuration");

    Ok(cfg.try_deserialize()?)
}

pub async fn listen(config: Config) -> Result<()> {
    let app = Router::new().route("/", post(handle_webhook));

    info!("Listening on {0}", &config.listen);
    // run it with hyper on localhost:3000
    axum::Server::bind(&config.listen.parse()?)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

async fn handle_webhook(Json(body): Json<serde_json::Value>) -> (StatusCode, String) {
    match real_handler(body).await {
        Ok(_) => (StatusCode::OK, String::new()),
        Err(err) => {
            tracing::error!("Failed to handle webhook: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Something went wrong: {}", err),
            )
        }
    }
}

async fn real_handler(body: serde_json::Value) -> Result<()> {
    info!("Starting Deno Runtime");
    let mut js_runtime = JsRuntime::new(Default::default());
    {
        let mut scope = js_runtime.handle_scope();
        let variable_context = scope.get_current_context();
        let global = variable_context.global(&mut scope);

        let body_value = serde_v8::to_v8(&mut scope, body)?;

        let body_key_str =
            deno_core::v8::String::new(&mut scope, "body").ok_or(Error::JSRuntimePassError)?;
        let _ = global
            .set(&mut scope, body_key_str.into(), body_value)
            .ok_or(Error::JSRuntimePassError)?;
    }

    let result = js_runtime.execute_script_static("handler", "body.pull_request.url;")?;
    let str = {
        let scope = &mut js_runtime.handle_scope();
        let local = deno_core::v8::Local::new(scope, result);
        // Deserialize a `v8` object into a Rust type using `serde_v8`,
        // in this case deserialize to a JSON `Value`.
        let deserialized_value = serde_v8::from_v8::<serde_json::Value>(scope, local)?;
        deserialized_value
    };
    info!("Result of Javascript evaliation: {}", str);

    Ok(())
}

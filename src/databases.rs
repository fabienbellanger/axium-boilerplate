//! Database module

use crate::config::Config;
use crate::errors::{CliError, CliResult};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::{MySql, Pool};
use std::time::Duration;

/// Initialize database connection pool for MySQL
pub async fn init(settings: &Config) -> CliResult<Pool<MySql>> {
    let url = &settings.database_url;
    let max_connections = settings.database_max_connections;
    let min_connections = settings.database_min_connections;
    let max_lifetime = settings.database_max_lifetime;
    let connect_timeout = settings.database_connect_timeout;
    let idle_timeout = settings.database_idle_timeout;

    let pool = MySqlPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(min_connections)
        .max_lifetime(Some(Duration::from_secs(max_lifetime)))
        .connect_timeout(Duration::from_secs(connect_timeout))
        .idle_timeout(Duration::from_secs(idle_timeout))
        .test_before_acquire(true)
        .connect(url)
        .await
        .map_err(|err| CliError::DatabaseError(err.to_string()))?;

    Ok(pool)
}

/// Initialize database connection pool for Redis
pub async fn init_redis(settings: &Config) -> CliResult<r2d2::Pool<redis::Client>> {
    let url = &settings.redis_url;
    let client = redis::Client::open(url.clone()).map_err(|err| CliError::DatabaseError(err.to_string()))?;

    Ok(r2d2::Pool::builder()
        .connection_timeout(Duration::from_secs(settings.redis_connection_timeout))
        .build(client)
        .map_err(|err| CliError::DatabaseError(err.to_string()))?)
}

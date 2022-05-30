//! Rate limiter module
//
// TODO:
// - Add list of excluded keys (listed in .env)
//

use crate::{
    errors::AppErrorMessage,
    models::auth::{self, Claims},
};
use axum::{
    body::{Body, Full},
    extract::ConnectInfo,
    http::{response::Parts, HeaderValue, Request, StatusCode},
    response::Response,
};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use derive_more::{Display, Error};
use futures::future::BoxFuture;
use r2d2::Pool;
use redis::{Client, Commands};
use std::{
    collections::HashMap,
    net::SocketAddr,
    task::{Context, Poll},
};
use tower::{Layer, Service};

pub struct RateLimiterLayer<'a> {
    pub pool: &'a Pool<Client>,
    pub jwt_secret: String,
    pub redis_prefix: String,
    pub enabled: bool,
    pub requests_by_second: i32,
    pub expire_in_seconds: i32,
}

impl<'a> RateLimiterLayer<'a> {
    pub fn new(
        pool: &'a Pool<Client>,
        jwt_secret: String,
        redis_prefix: String,
        enabled: bool,
        requests_by_second: i32,
        expire_in_seconds: i32,
    ) -> Self {
        Self {
            pool,
            jwt_secret,
            redis_prefix,
            enabled,
            requests_by_second,
            expire_in_seconds,
        }
    }
}

impl<'a, S> Layer<S> for RateLimiterLayer<'a> {
    type Service = RateLimiterMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimiterMiddleware {
            inner,
            pool: self.pool.clone(),
            jwt_secret: self.jwt_secret.clone(),
            redis_prefix: self.redis_prefix.clone(),
            enabled: self.enabled,
            requests_by_second: self.requests_by_second,
            expire_in_seconds: self.expire_in_seconds,
        }
    }
}

#[derive(Clone)]
pub struct RateLimiterMiddleware<S> {
    inner: S,
    pool: Pool<Client>,
    jwt_secret: String,
    redis_prefix: String,
    enabled: bool,
    requests_by_second: i32,
    expire_in_seconds: i32,
}

impl<S> Service<Request<Body>> for RateLimiterMiddleware<S>
where
    S: Service<Request<Body>, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    // `BoxFuture` is a type alias for `Pin<Box<dyn Future + Send + 'a>>`
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let check_result = match self.enabled {
            true => {
                // Redis connection
                let pool = self.pool.clone();

                // Check JWT claims
                let claims = auth::Claims::extract_from_request(request.headers(), self.jwt_secret.clone());

                // Get socket address
                let addr = request.extensions().get::<ConnectInfo<SocketAddr>>();

                // Initialize RateLimiterCheck
                let check = RateLimiterCheck::init(claims, &self.redis_prefix, self.requests_by_second, addr);

                check.check_and_update(&pool, self.expire_in_seconds)
            }
            false => Ok((-1, 0, 0)), // Disabled ie. no limit
        };

        let future = self.inner.call(request);
        Box::pin(async move {
            let mut response = Response::default();

            response = match check_result {
                Ok((limit, _remaining, _reset)) if limit <= -1 => future.await?,
                Ok((limit, remaining, reset)) if remaining >= 0 => {
                    // Limit OK
                    // --------
                    let (mut parts, body) = future.await?.into_parts();

                    // Headers
                    set_headers(&mut parts, limit, remaining, reset);

                    Response::from_parts(parts, body)
                }
                Ok((limit, remaining, reset)) => {
                    // Limit KO
                    // --------
                    let (mut parts, _body) = response.into_parts();

                    // Status
                    parts.status = StatusCode::TOO_MANY_REQUESTS;

                    // Headers
                    set_headers(&mut parts, limit, remaining, reset);
                    parts.headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );

                    // Body
                    let msg = serde_json::json!(AppErrorMessage {
                        code: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                        message: String::from("Too Many Requests"),
                    });
                    let msg = Bytes::from(msg.to_string());

                    Response::from_parts(parts, axum::body::boxed(Full::from(msg)))
                }
                Err(err) => {
                    let (mut parts, _body) = response.into_parts();

                    // Status
                    parts.status = StatusCode::INTERNAL_SERVER_ERROR;

                    // Content Type
                    parts.headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );

                    // Body
                    let msg = serde_json::json!(AppErrorMessage {
                        code: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                        message: err.to_string(),
                    });
                    let msg = Bytes::from(msg.to_string());

                    Response::from_parts(parts, axum::body::boxed(Full::from(msg)))
                }
            };

            Ok(response)
        })
    }
}

/// Set middleware specific headers
fn set_headers(parts: &mut Parts, limit: i32, remaining: i32, reset: i64) {
    if remaining >= 0 {
        // Limit OK
        if let Ok(limit) = HeaderValue::from_str(limit.to_string().as_str()) {
            parts.headers.insert("x-ratelimit-limit", limit);
        }

        if let Ok(remaining) = HeaderValue::from_str(remaining.to_string().as_str()) {
            parts.headers.insert("x-ratelimit-remaining", remaining);
        }

        if let Ok(reset) = HeaderValue::from_str(reset.to_string().as_str()) {
            parts.headers.insert("x-ratelimit-reset", reset);
        }
    } else {
        // Limit reached
        if let Ok(reset) = HeaderValue::from_str(reset.to_string().as_str()) {
            parts.headers.insert("retry-after", reset);
        }
    }
}

#[derive(Display, Debug, Error, Copy, Clone, PartialEq)]
enum RateLimiterError {
    Ip,
    Redis,
    DateTime,
    Parse,
}

impl From<redis::RedisError> for RateLimiterError {
    fn from(_error: redis::RedisError) -> Self {
        Self::Redis {}
    }
}

impl From<r2d2::Error> for RateLimiterError {
    fn from(_error: r2d2::Error) -> Self {
        Self::Redis {}
    }
}

#[derive(Debug, PartialEq)]
struct RateLimiterCheck {
    error: Option<RateLimiterError>,
    key: Option<String>,
    limit: i32,
}

impl Default for RateLimiterCheck {
    fn default() -> Self {
        Self {
            error: None,
            key: None,
            limit: -1,
        }
    }
}

impl RateLimiterCheck {
    fn new(error: Option<RateLimiterError>, key: Option<String>, limit: i32) -> Self {
        Self { error, key, limit }
    }

    // Initialize `RateLimiterCheck`
    fn init(
        claims: Option<Claims>,
        redis_prefix: &str,
        requests_by_second: i32,
        addr: Option<&ConnectInfo<SocketAddr>>,
    ) -> Self {
        match claims {
            None => {
                let default_limit = requests_by_second;
                if default_limit == -1 {
                    // No limit
                    Self::default()
                } else {
                    // Client Remote IP address
                    match addr {
                        None => Self::new(Some(RateLimiterError::Ip), None, 0),
                        Some(remote_address) => {
                            let mut key = remote_address.0.ip().to_string();
                            key.insert_str(0, redis_prefix);
                            Self::new(None, Some(key), default_limit)
                        }
                    }
                }
            }
            Some(claims) => {
                let mut key = claims.user_id;
                key.insert_str(0, redis_prefix);
                Self::new(None, Some(key), claims.user_limit)
            }
        }
    }

    /// Checks limit, update Redis and reurns information for headers
    // TODO: Improve by using timestamp instead of DateTime (use https://doc.rust-lang.org/std/time/struct.SystemTime.html#method.elapsed)
    fn check_and_update(
        &self,
        pool: &Pool<Client>,
        expire_in_seconds: i32,
    ) -> Result<(i32, i32, i64), RateLimiterError> {
        if let Some(err) = self.error {
            Err(err)
        } else if self.limit == -1 {
            Ok((self.limit, 0, 0))
        } else {
            let mut conn = pool.get()?;
            let now: DateTime<Utc> = Utc::now();
            let mut remaining = self.limit - 1;
            let mut reset = expire_in_seconds as i64;
            let mut expired_at = now + Duration::seconds(expire_in_seconds as i64);

            let result: HashMap<String, String> = conn.hgetall(&self.key)?;

            if !result.is_empty() {
                let expired_at_str = result.get("expiredAt").ok_or(RateLimiterError::Redis)?;
                expired_at = DateTime::parse_from_rfc3339(expired_at_str)
                    .map_err(|_err| RateLimiterError::DateTime)?
                    .with_timezone(&Utc);

                reset = (expired_at - now).num_seconds();

                if reset <= 0 {
                    // Expired cache
                    // -------------
                    conn.del(&self.key)?; // Necesary?

                    expired_at = now + Duration::seconds(expire_in_seconds as i64);
                    reset = expire_in_seconds as i64;
                } else {
                    // Valid cache
                    // -----------
                    let remaining_str = result.get("remaining").ok_or(RateLimiterError::Redis)?;
                    remaining = remaining_str.parse::<i32>().map_err(|_err| RateLimiterError::Parse)?;

                    if remaining >= 0 {
                        remaining -= 1;
                    }
                }
            }

            conn.hset(&self.key, "remaining", remaining)?;
            conn.hset(&self.key, "expiredAt", expired_at.to_rfc3339())?;

            Ok((self.limit, remaining, reset))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_rate_limiter_check_init() {
        let mut requests_by_second = 30;
        let claims = None;
        let addr = None;
        let redis_prefix = "axum_";

        assert_eq!(
            RateLimiterCheck::init(claims, redis_prefix, requests_by_second, addr),
            RateLimiterCheck {
                error: Some(RateLimiterError::Ip),
                key: None,
                limit: 0
            }
        );

        requests_by_second = -1;
        let claims = None;
        let addr = None;
        assert_eq!(
            RateLimiterCheck::init(claims, redis_prefix, requests_by_second, addr),
            RateLimiterCheck {
                error: None,
                key: None,
                limit: -1
            }
        );

        requests_by_second = 30;
        let user_id = uuid::Uuid::new_v4().to_string();
        let claims = Some(Claims {
            sub: String::from("Subject"),
            exp: 123456789,
            iat: 123456789,
            nbf: 123456789,
            user_id: user_id.clone(),
            user_roles: String::from("ADMIN"),
            user_limit: 25,
        });
        let addr = None;
        let mut key = user_id.clone();
        key.insert_str(0, redis_prefix);
        assert_eq!(
            RateLimiterCheck::init(claims, redis_prefix, requests_by_second, addr),
            RateLimiterCheck {
                error: None,
                key: Some(key),
                limit: 25
            }
        );

        let claims = None;
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8000);
        let addr = Some(ConnectInfo(socket));
        let mut key = "127.0.0.1".to_owned();
        key.insert_str(0, redis_prefix);
        assert_eq!(
            RateLimiterCheck::init(claims, redis_prefix, requests_by_second, addr.as_ref()),
            RateLimiterCheck {
                error: None,
                key: Some(key),
                limit: 30
            }
        );
    }
}

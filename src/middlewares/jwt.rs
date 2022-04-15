//! JWT middleware

use crate::{models::auth, states};
use axum::{
    body::{Body, BoxBody, Full},
    http::{Request, StatusCode},
    response::Response,
};
use futures::future::BoxFuture;
use std::task::{Context, Poll};
use tower::{Layer, Service};

pub struct JwtLayer;

impl<S> Layer<S> for JwtLayer {
    type Service = JwtMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JwtMiddleware { inner }
    }
}

#[derive(Clone)]
pub struct JwtMiddleware<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for JwtMiddleware<S>
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
        let state = request.extensions().get::<states::SharedState>().unwrap(); // TODO: Remove unwrap()
        let state = state.clone();
        let is_authorized =
            auth::Claims::extract_from_request(request.headers(), state.jwt_secret_key.clone()).is_some();

        let future = self.inner.call(request);
        Box::pin(async move {
            let mut response: Response = future.await?;

            if !is_authorized {
                let (mut parts, _body) = response.into_parts();

                parts.headers.remove(axum::http::header::CONTENT_LENGTH);
                parts.status = StatusCode::UNAUTHORIZED;

                response = Response::from_parts(parts, BoxBody::default());
                // response = Response::from_parts(parts, body::boxed(Full::from(body_bytes)));
            }
            Ok(response)
        })
    }
}

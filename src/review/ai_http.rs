use crate::{
    error::{AppError, AppResult, ReviewErrorCode},
    rules::AiReviewConfig,
};
use std::{
    error::Error,
    io,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};
use tracing::{info, warn, Span};

pub(crate) static AI_HTTP_CLIENT: OnceLock<Result<ureq::Agent, String>> = OnceLock::new();

pub(crate) struct AiReviewHttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
}

pub(crate) fn shared_ai_http_client() -> AppResult<&'static ureq::Agent> {
    AI_HTTP_CLIENT
        .get_or_init(|| {
            Ok(ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(120))
                .timeout_write(Duration::from_secs(120))
                .build())
        })
        .as_ref()
        .map_err(|err| {
            AppError::ai_review(
                ReviewErrorCode::AiRequestFailed,
                format!("failed to build shared AI HTTP client: {err}"),
            )
        })
}

pub(crate) async fn perform_ai_review_http_attempt(
    client: &ureq::Agent,
    config: &AiReviewConfig,
    url: &str,
    api_key: &str,
    request_body: Vec<u8>,
    attempt: usize,
    request_timeout: Duration,
    timeout_code: ReviewErrorCode,
) -> AppResult<AiReviewHttpResponse> {
    let client = client.clone();
    let review_id = config.id.clone();
    let model = config.model.clone();
    let worker_review_id = review_id.clone();
    let worker_model = model.clone();
    let url = url.to_string();
    let api_key = api_key.to_string();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let span = Span::current();
    thread::Builder::new()
        .name(format!("ai-review-http-{attempt}"))
        .spawn(move || {
            let _entered = span.enter();
            let result = perform_ai_review_http_attempt_blocking(AiReviewHttpAttempt {
                client,
                review_id: worker_review_id.clone(),
                model: worker_model.clone(),
                url,
                api_key,
                request_body,
                attempt,
                request_timeout,
                timeout_code,
            });
            let result_sent = sender.send(result).is_ok();
            info!(
                ai_review_id = %worker_review_id,
                model = %worker_model,
                attempt,
                result_sent,
                "AI review blocking HTTP worker result sent"
            );
        })
        .map_err(|err| {
            AppError::ai_review(
                ReviewErrorCode::AiRequestFailed,
                format!(
                    "AI review {} failed to spawn blocking HTTP worker: {err}",
                    config.id
                ),
            )
        })?;

    let response = receiver.await.map_err(|err| {
        AppError::ai_review(
            ReviewErrorCode::AiRequestFailed,
            format!(
                "AI review {} blocking HTTP worker dropped result channel: {err}",
                config.id
            ),
        )
    })??;
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        status = response.status,
        response_bytes = response.body.len(),
        "AI review blocking HTTP task completed"
    );
    Ok(response)
}

pub(crate) fn is_retryable_ai_error(err: &AppError) -> bool {
    match err {
        AppError::Reqwest(err) => {
            err.is_timeout() || err.is_connect() || err.is_request() || err.is_body()
        }
        AppError::AiReview(failure) => {
            failure.message.contains("blocking API request failed")
                || failure
                    .message
                    .contains("blocking API response body read failed")
                || failure.message.contains("blocking HTTP task failed")
        }
        _ => false,
    }
}

struct AiReviewHttpAttempt {
    client: ureq::Agent,
    review_id: String,
    model: String,
    url: String,
    api_key: String,
    request_body: Vec<u8>,
    attempt: usize,
    request_timeout: Duration,
    timeout_code: ReviewErrorCode,
}

fn perform_ai_review_http_attempt_blocking(
    attempt_context: AiReviewHttpAttempt,
) -> AppResult<AiReviewHttpResponse> {
    let AiReviewHttpAttempt {
        client,
        review_id,
        model,
        url,
        api_key,
        request_body,
        attempt,
        request_timeout,
        timeout_code,
    } = attempt_context;
    let started = Instant::now();
    let response = match ureq_response_from_result(
        client
            .post(&url)
            .set("authorization", &format!("Bearer {api_key}"))
            .set("content-type", "application/json")
            .timeout(request_timeout)
            .send_bytes(&request_body),
        timeout_code,
    ) {
        Ok(response) => response,
        Err(err) => {
            warn!(
                ai_review_id = %review_id,
                model = %model,
                attempt,
                elapsed_ms = started.elapsed().as_millis(),
                error = %err,
                "AI review blocking API request failed before response headers"
            );
            return Err(err);
        }
    };

    let status = response.status();
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        status,
        elapsed_ms = started.elapsed().as_millis(),
        "AI review blocking API response headers received"
    );
    let body_started = Instant::now();
    let body = match response.into_string() {
        Ok(body) => body,
        Err(err) => {
            warn!(
                ai_review_id = %review_id,
                model = %model,
                attempt,
                status,
                elapsed_ms = started.elapsed().as_millis(),
                error = %err,
                "AI review blocking API response body read failed"
            );
            let code = if is_io_timeout(&err) {
                timeout_code
            } else {
                ReviewErrorCode::AiRequestFailed
            };
            return Err(AppError::ai_review(
                code,
                format!("AI review blocking API response body read failed: {err}"),
            ));
        }
    };
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        response_bytes = body.len(),
        elapsed_ms = body_started.elapsed().as_millis(),
        "AI review blocking API response body received"
    );
    Ok(AiReviewHttpResponse { status, body })
}

fn ureq_response_from_result(
    result: Result<ureq::Response, ureq::Error>,
    timeout_code: ReviewErrorCode,
) -> AppResult<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(_, response)) => Ok(response),
        Err(err) => {
            let code = if is_ureq_timeout(&err) {
                timeout_code
            } else {
                ReviewErrorCode::AiRequestFailed
            };
            Err(AppError::ai_review(
                code,
                format!("AI review blocking API request failed before response headers: {err}"),
            ))
        }
    }
}

fn is_ureq_timeout(err: &ureq::Error) -> bool {
    err.kind() == ureq::ErrorKind::Io
        && Error::source(err)
            .and_then(|source| source.downcast_ref::<io::Error>())
            .is_some_and(is_io_timeout)
}

fn is_io_timeout(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::TimedOut
}

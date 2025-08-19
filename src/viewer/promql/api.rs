use super::*;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct QueryParams {
    pub query: String,
    pub time: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct RangeQueryParams {
    pub query: String,
    pub start: f64,
    pub end: f64,
    pub step: f64,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "errorType")]
    pub error_type: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            status: "success".to_string(),
            data: Some(data),
            error: None,
            error_type: None,
        }
    }

    pub fn error(error: String, error_type: String) -> Self {
        Self {
            status: "error".to_string(),
            data: None,
            error: Some(error),
            error_type: Some(error_type),
        }
    }
}

/// Create the PromQL API routes
pub fn routes(engine: Arc<QueryEngine>) -> Router {
    Router::new()
        .route("/api/v1/query", get(instant_query))
        .route("/api/v1/query_range", get(range_query))
        .route("/api/v1/labels", get(label_names))
        .route("/api/v1/label/{name}/values", get(label_values))
        .route("/api/v1/metadata", get(metadata))
        .with_state(engine)
}

/// Handle instant queries
async fn instant_query(
    Query(params): Query<QueryParams>,
    State(engine): State<Arc<QueryEngine>>,
) -> Result<Json<ApiResponse<QueryResult>>, StatusCode> {
    match engine.query(&params.query, params.time) {
        Ok(result) => Ok(Json(ApiResponse::success(result))),
        Err(e) => {
            let error_type = match e {
                QueryError::ParseError(_) => "bad_data",
                QueryError::EvaluationError(_) => "execution",
                QueryError::Unsupported(_) => "unsupported",
                QueryError::MetricNotFound(_) => "not_found",
            };
            Ok(Json(ApiResponse::error(
                e.to_string(),
                error_type.to_string(),
            )))
        }
    }
}

/// Handle range queries
async fn range_query(
    Query(params): Query<RangeQueryParams>,
    State(engine): State<Arc<QueryEngine>>,
) -> Result<Json<ApiResponse<QueryResult>>, StatusCode> {
    match engine.query_range(&params.query, params.start, params.end, params.step) {
        Ok(result) => Ok(Json(ApiResponse::success(result))),
        Err(e) => {
            let error_type = match e {
                QueryError::ParseError(_) => "bad_data",
                QueryError::EvaluationError(_) => "execution",
                QueryError::Unsupported(_) => "unsupported",
                QueryError::MetricNotFound(_) => "not_found",
            };
            Ok(Json(ApiResponse::error(
                e.to_string(),
                error_type.to_string(),
            )))
        }
    }
}

/// Return all label names
async fn label_names(
    State(_engine): State<Arc<QueryEngine>>,
) -> Result<Json<ApiResponse<Vec<String>>>, StatusCode> {
    // TODO: Implement actual label name extraction from TSDB
    let labels = vec![
        "__name__".to_string(),
        "direction".to_string(),
        "op".to_string(),
        "state".to_string(),
        "reason".to_string(),
        "id".to_string(),
        "name".to_string(),
        "sampler".to_string(),
    ];

    Ok(Json(ApiResponse::success(labels)))
}

/// Return all values for a specific label
async fn label_values(
    axum::extract::Path(name): axum::extract::Path<String>,
    State(_engine): State<Arc<QueryEngine>>,
) -> Result<Json<ApiResponse<Vec<String>>>, StatusCode> {
    // TODO: Implement actual label value extraction from TSDB
    let values = match name.as_str() {
        "direction" => vec![
            "transmit".to_string(),
            "receive".to_string(),
            "to".to_string(),
            "from".to_string(),
        ],
        "op" => vec!["read".to_string(), "write".to_string()],
        "state" => vec!["user".to_string(), "system".to_string()],
        _ => vec![],
    };

    Ok(Json(ApiResponse::success(values)))
}

/// Handle metadata queries to get time range info
async fn metadata(
    State(engine): State<Arc<QueryEngine>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, StatusCode> {
    let time_range = engine.get_time_range();
    let metadata = serde_json::json!({
        "minTime": time_range.0,
        "maxTime": time_range.1
    });
    Ok(Json(ApiResponse::success(metadata)))
}

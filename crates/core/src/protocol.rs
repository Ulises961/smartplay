//! Messages between a connector (plugin or direct) and the engine. Shared so both sides agree.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct EnrollRequest {
    pub code: String,
}

#[derive(Serialize, Deserialize)]
pub struct EnrollResponse {
    pub token: String,
}

#[derive(Serialize, Deserialize)]
pub struct ComputeRequest {
    /// Server-side user id or name the data belongs to.
    pub user: String,
    pub starred: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ComputeResponse {
    pub playlist: String,
    pub songs: Vec<String>,
}

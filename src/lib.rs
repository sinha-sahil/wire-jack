//! # wire-jack
//!
//! Shape an API call with a [temple-dsl](https://github.com/sinha-sahil/temple-dsl)
//! template, render it against runtime inputs into a typed [`ApiRequest`], and
//! execute the resulting request.
//!
//! A single template is the contract — it branches on a `phase` flag to map both
//! sides of a call: the request phase renders user data into an [`ApiRequest`],
//! the response phase renders the [`ApiResponse`] into a free-form output. Give a
//! template and call [`run`].
//!
//! ```text
//! run(template, data): render request -> ApiRequest -> execute -> ApiResponse -> render response
//! ```
//!
//! [`run`] returns a [`RunResult`]: the rendered output plus the raw [`ApiResponse`].
//!
//! To act between the phases — for example, to stop on a non-2xx before the
//! response phase runs — call the steps yourself: [`render_request`], [`execute()`],
//! then [`render_response`].

pub mod error;
pub mod execute;
mod render;
pub mod request;
pub mod response;

pub use error::{Result, WireJackError};
pub use render::{render_request, render_response, run, RunResult};
pub use request::{ApiRequest, Auth, Retry};
pub use response::ApiResponse;

pub use temple_dsl::{Template, Value};

pub async fn execute(req: &ApiRequest) -> Result<ApiResponse> {
    execute::execute(req).await
}

//! Typed, asynchronous access to Bitbucket Cloud.
//!
//! The client does not read environment variables, configure logging, or persist credentials.
//! Each instance confines authenticated requests to its configured API origin.
//!
//! ```no_run
//! use bb_api::{Client, RepoRef};
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder().token("a-token").build()?;
//! let repo: RepoRef = "my-workspace/my-repository".parse()?;
//! let repository = client.repository(&repo).await?;
//! println!("{}", repository.full_name);
//! # Ok(())
//! # }
//! ```
#![warn(missing_docs)]

mod client;
mod download;
mod error;
mod ids;
pub mod models;
mod pagination;
pub mod requests;
mod resources;
mod transport;

#[doc(inline)]
pub use client::{Client, ClientBuilder, Method, RawResponse};
#[doc(inline)]
pub use download::{ByteRange, LogResponse};
#[doc(inline)]
pub use error::Error;
#[doc(inline)]
pub use ids::{BranchName, CommentId, ProjectKey, PullRequestId, RepoRef, TaskId, Workspace};

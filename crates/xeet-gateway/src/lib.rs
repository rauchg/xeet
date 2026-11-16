//! `xeet-gateway`: the modality gateway.
//!
//! Makes information consumable by a model regardless of the model's native
//! input modalities. Deterministic infrastructure, not a model decision:
//!
//! - [`model`] — what a target model accepts, with the basis for each claim;
//!   the host's configuration is authoritative.
//! - [`inspect`] — what each input actually is, from its content.
//! - [`gateway`] — decides, discovers providers by contract and modality,
//!   transforms through their contracts with the host's session, projects
//!   within a context budget, injects beside the unchanged instruction, and
//!   records why; falls back once on a confirmed modality rejection.
//! - [`request`] — the provider-neutral model request, the client trait a
//!   runtime implements, and error classification.
//! - [`openai`] — one concrete request encoding and client over an injected
//!   HTTP transport.
//!
//! Responsibilities stay separate: providers execute (via `Provider`), the
//! gateway coordinates, the model client talks to the model, and the host
//! grants authority.

#![forbid(unsafe_code)]

pub mod gateway;
pub mod inspect;
pub mod model;
pub mod openai;
pub mod provider;
pub mod reader;
pub mod request;

pub use gateway::{Gateway, Options, Run, RunRecord, Task, UnknownPolicy};
pub use inspect::Input;
pub use model::ModelProfile;
pub use request::{Classifier, Content, ModelClient, ModelRequest, ModelResponse, ProviderError};

mod adapters;
mod common;
mod error;
pub(crate) mod fs;
mod mcp_app_proxy;
mod provider;
pub mod server;
pub mod server_factory;
pub(crate) mod tools;
pub mod transport;

pub use common::{map_permission_response, PermissionDecision};
pub use error::{AcpErrorExt, ResultExt};
pub use goose_sdk::custom_requests;
pub use provider::{
    extension_configs_to_mcp_servers, AcpProvider, AcpProviderConfig, ACP_CURRENT_MODEL,
};

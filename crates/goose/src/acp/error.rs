/// `agent_client_protocol::Error.message` is what UIs render; the JSON-RPC
/// constructors (`internal_error`, `invalid_params`) leave it as a generic
/// default. `with_detail` overrides it with the human-readable message.
pub trait AcpErrorExt {
    fn with_detail(self, detail: impl Into<String>) -> Self;
}

impl AcpErrorExt for agent_client_protocol::Error {
    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.message = detail.into();
        self
    }
}

/// Convenience conversions from any `Display` error into an
/// `agent_client_protocol::Error`. Use `.internal_err()?` for server-side
/// failures and `.invalid_params_err()?` for bad client input; for custom
/// messages use `.internal_err_ctx("context")?`.
#[allow(dead_code)]
pub trait ResultExt<T> {
    fn internal_err(self) -> Result<T, agent_client_protocol::Error>;
    fn invalid_params_err(self) -> Result<T, agent_client_protocol::Error>;
    fn internal_err_ctx(self, context: &str) -> Result<T, agent_client_protocol::Error>;
    fn invalid_params_err_ctx(self, context: &str) -> Result<T, agent_client_protocol::Error>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn internal_err(self) -> Result<T, agent_client_protocol::Error> {
        self.map_err(|e| agent_client_protocol::Error::internal_error().with_detail(e.to_string()))
    }
    fn invalid_params_err(self) -> Result<T, agent_client_protocol::Error> {
        self.map_err(|e| agent_client_protocol::Error::invalid_params().with_detail(e.to_string()))
    }
    fn internal_err_ctx(self, context: &str) -> Result<T, agent_client_protocol::Error> {
        self.map_err(|e| {
            agent_client_protocol::Error::internal_error().with_detail(format!("{context}: {e}"))
        })
    }
    fn invalid_params_err_ctx(self, context: &str) -> Result<T, agent_client_protocol::Error> {
        self.map_err(|e| {
            agent_client_protocol::Error::invalid_params().with_detail(format!("{context}: {e}"))
        })
    }
}

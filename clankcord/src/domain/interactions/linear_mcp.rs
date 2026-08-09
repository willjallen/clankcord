use std::collections::BTreeMap;

use crate::Result;
use crate::config;

pub(crate) fn insert_linear_mcp_env(vars: &mut BTreeMap<String, String>) -> Result<()> {
    if !config::codex_linear_mcp_enabled() {
        return Ok(());
    }
    vars.insert(
        config::CODEX_LINEAR_MCP_TOKEN_ENV.to_string(),
        config::codex_linear_mcp_api_key()?,
    );
    Ok(())
}

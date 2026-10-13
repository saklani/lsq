//! The MCP stdio front-end (built on the official `rmcp` SDK) — lsq's only entry point.
//!
//! The MCP server is itself long-lived, so it holds the warm per-language
//! [`Session`](crate::protocol::session::Session)s **in-process** and calls
//! [`engine::dispatch`](crate::engine::dispatch) directly — it all runs in one process; there is
//! no daemon and no socket.
//!
//! Split by job: [`args`] is the input-parsing boundary (loose JSON → validated `Address`),
//! [`tools`] is the tool router (one thin `#[tool]` per verb), and [`server`] is the runtime
//! (`Server` + the blocking dispatch bridge). The engine is synchronous (LSP calls block); each
//! tool hands off to `spawn_blocking` so the rmcp runtime stays responsive.

mod args;
mod server;
mod tools;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use server::Server;

/// Serve the MCP protocol on stdio. Blocks until the client disconnects.
pub fn serve() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let root = std::env::current_dir()?;
        let service = Server::new(root).serve(stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })
}

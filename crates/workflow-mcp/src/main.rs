//! `workflow-mcp`: a stdio MCP server that is the typed front door to the Workflow office.
//!
//! koma's MCP client (`src-agent/src/app/mcp`) spawns this binary and speaks MCP over the
//! child's stdin/stdout. Both sides are rmcp 1.8, so the initialize/list/call handshake is
//! guaranteed compatible. The tools then write into the SAME file-inbox pipeline the daemon
//! already consumes (ARCHITECTURE.md 6.4), so this server never needs its own connection to
//! the daemon — it just drops well-formed request files and reads the store for status.
//!
//! IMPORTANT: stdout is the MCP transport. Nothing here may print to stdout; all diagnostics
//! go to stderr.

mod server;
mod status;
mod write;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use server::WorkflowServer;

#[tokio::main]
async fn main() {
    // Serve MCP over stdio. `serve` performs the initialize handshake, then `waiting`
    // drives the request loop until the client closes the connection (EOF on stdin).
    let service = match WorkflowServer::new().serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("workflow-mcp: failed to start MCP server: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = service.waiting().await {
        eprintln!("workflow-mcp: server stopped: {e}");
    }
}

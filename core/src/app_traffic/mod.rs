//! Application-like background traffic: simulated users ("agents") that
//! browse the web, sync mail, watch video or scroll a feed — or an AI agent
//! working through tasks (LLM calls with streamed replies, MCP tool calls, web
//! fetches) — so a test can show what that load does to the realtime flows
//! running beside it.
//!
//! The UE runs the agents and opens every connection (invariant 5); the server
//! side is a plain object server (data-generator `spawn_app_server`). Each
//! object is one exchange on a TCP connection (`packet::AppExchange`): bytes
//! up, a server think time, bytes down (optionally paced like a token stream).
//! Both nodes log every object as rows sharing its `object_id` (the row's
//! sequence_number), tagged with `app_activity`, `app_object_bytes` and
//! `app_up_bytes`: the request (SENT, 44 B), the upload chunks (SENT), then the
//! response chunks (RECEIVED) — or, when nothing comes back, the server's
//! RECEIVED acknowledgement. So analysis gets request → first byte →
//! completion for every object, and the gaps inside a stream.
//!
//! Layers: `model` says what each user asks for and when (no sockets);
//! `scheduler` turns its steps into operations and logs what was intended
//! (`app_events`); `executor` runs a burst's operations through the
//! request/response engine (`crate::exchange`).
//!
//! Agents are seeded: what they ask for is reproducible, when it completes is
//! up to the network. `profile = "script"` runs an explicit action timeline
//! instead — the hook for an AI agent that plays a user and writes the plan.

mod executor;
pub mod graph;
mod model;
mod scheduler;

pub use executor::Outcome;
pub use model::*;
pub use scheduler::{run, AppLog, AppTraffic, Operation};

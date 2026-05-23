//! DTOs mirroring `contracts/schemas/*.json`. 1a lands the data-shape types
//! (Capability, LoggingConfig, TestPlan/Flow/FlowBinding); auth / command /
//! upload / internal / controller DTOs arrive in their consuming sub-phases.

pub mod capability;
pub mod logging_config;
pub mod test_plan;

pub use capability::*;
pub use logging_config::*;
pub use test_plan::*;

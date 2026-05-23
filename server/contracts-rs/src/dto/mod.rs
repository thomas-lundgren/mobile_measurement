//! DTOs mirroring `contracts/schemas/*.json`. 1a lands the data-shape types
//! (Capability, LoggingConfig, TestPlan/Flow/FlowBinding); auth / command /
//! upload / internal / controller DTOs arrive in their consuming sub-phases.

pub mod auth;
pub mod capability;
pub mod command;
pub mod device_status;
pub mod internal;
pub mod logging_config;
pub mod orchestration;
pub mod test_plan;
pub mod upload;

pub use auth::*;
pub use capability::*;
pub use command::*;
pub use device_status::*;
pub use internal::*;
pub use logging_config::*;
pub use orchestration::*;
pub use test_plan::*;
pub use upload::*;

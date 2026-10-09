pub mod actions;
pub mod charts;
pub mod collections;
pub mod devices;
/**
* Domain models, entities, and validation rules for Vox Core.
*/
pub mod identity;
pub mod records;
pub mod schemas;
pub mod spaces;
pub mod spans;

pub use actions::*;
pub use charts::*;
pub use collections::*;
pub use devices::*;
pub use identity::*;
pub use records::*;
pub use schemas::*;
pub use spaces::*;
pub use spans::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcurrencyOutcome<T> {
    Success(T),
    Conflict,
    NotFound,
}

pub mod pulse;
pub mod timeline;
pub mod updates;

pub use timeline::*;
pub use updates::*;

pub mod actions;
pub mod collections;
pub mod devices;
/**
* Domain models, entities, and validation rules for Vox Core.
*/
pub mod identity;
pub mod records;
pub mod schemas;
pub mod spans;

pub use actions::*;
pub use collections::*;
pub use devices::*;
pub use identity::*;
pub use records::*;
pub use schemas::*;
pub use spans::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcurrencyOutcome<T> {
    Success(T),
    Conflict,
    NotFound,
}

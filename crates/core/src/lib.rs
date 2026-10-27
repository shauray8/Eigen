//! Editor core: the document and the only ways it changes.
//!
//! | module      | role                                                        |
//! |-------------|-------------------------------------------------------------|
//! | [`data`]    | plain data: document, scene settings, layers and objects    |
//! | [`paths`]   | every property addressable by a path, e.g. `title.loc.x`    |
//! | [`anim`]    | keyframe curves per scalar channel, with easing             |
//! | [`ops`]     | operators: the only way data changes; same for human and AI |
//! | [`lang`]    | compact text form of operators: the agent's API and file format |
//! | [`eval`]    | document + frame -> evaluated scene (deterministic)         |
//! | [`query`]   | reads, answered in the op language                          |
//! | [`session`] | live document, undo steps, operator log                     |

pub mod anim;
pub mod data;
pub mod eval;
pub mod expr;
pub mod lang;
pub mod ops;
pub mod paths;
pub mod query;
pub mod session;

pub use session::{Session, Source};

//! Authentication primitives — JWT signing/verification, cookie helpers.
//!
//! The handlers in upcoming TODOs ([17] register, [18] login, [19] refresh)
//! consume this module via `crate::auth::{jwt, set_auth_cookies}`.

pub mod jwt;

mod cookies;
// Consumed by the auth handlers landing in TODOs [17]/[18]/[19]; re-export
// now so wiring later only needs `use crate::auth::...`.
#[allow(unused_imports)]
pub use cookies::{clear_auth_cookies, set_auth_cookies, AuthCookies};

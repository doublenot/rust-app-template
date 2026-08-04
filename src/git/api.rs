//! The `/api/git` sub-router: DTOs in, `GitService` calls out, one error envelope back.
//!
//! No `git2` type crosses this file. Every route — GETs included — is authenticated by
//! the single `from_fn_with_state` layer at the bottom of `router`, so no handler can
//! forget the token.

//! Permission-mode catalog exposed to the Agents page.

use crate::domain::permission::{face, PermissionFace};

#[derive(Clone, Default)]
pub struct PermissionService;

impl PermissionService {
    pub fn new() -> Self {
        Self
    }

    pub fn face(&self, engine: &str) -> PermissionFace {
        face(engine)
    }
}

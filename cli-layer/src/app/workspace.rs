//! Folder and conversation use cases.

use anyhow::Result;
use serde_json::Value;

use crate::infra::store::Store;

#[derive(Clone)]
pub struct WorkspaceService {
    store: Store,
}

impl WorkspaceService {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub fn list_folders(&self) -> Result<Vec<Value>> {
        self.store.list_folders()
    }

    pub fn open_folder(&self, path: &str) -> Result<Value> {
        self.store.open_folder(path)
    }

    pub fn remove_folder(&self, id: &str) -> Result<()> {
        self.store.remove_folder(id)
    }

    pub fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>> {
        self.store.list_conversations(folder_ids)
    }

    pub fn create_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
    ) -> Result<String> {
        self.store.create_conversation(folder_id, agent_type, title)
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<Value>> {
        self.store.get_conversation(id)
    }

    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        self.store.delete_conversation(id)
    }
}

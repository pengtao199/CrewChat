//! Folder and conversation use cases.

use anyhow::Result;
use serde_json::Value;

use std::sync::Arc;

use crate::domain::ports::ConversationRepository;

#[derive(Clone)]
pub struct WorkspaceService {
    repo: Arc<dyn ConversationRepository>,
}

impl WorkspaceService {
    pub fn new(repo: Arc<dyn ConversationRepository>) -> Self {
        Self { repo }
    }

    pub fn list_folders(&self) -> Result<Vec<Value>> {
        self.repo.list_folders()
    }

    pub fn open_folder(&self, path: &str) -> Result<Value> {
        self.repo.open_folder(path)
    }

    pub fn remove_folder(&self, id: &str) -> Result<()> {
        self.repo.remove_folder(id)
    }

    pub fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>> {
        self.repo.list_conversations(folder_ids)
    }

    pub fn create_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
    ) -> Result<String> {
        self.repo.create_conversation(folder_id, agent_type, title)
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<Value>> {
        self.repo.get_conversation(id)
    }

    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        self.repo.delete_conversation(id)
    }
}

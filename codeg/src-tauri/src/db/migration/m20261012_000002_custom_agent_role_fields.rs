use sea_orm_migration::prelude::*;

/// [crewchat] Phase 2.1 — the custom-agent registry becomes the single roster
/// store. A `custom_agent` row is now either a launchable external ACP agent
/// (`kind = 'acp'`, the original concept) or a crewchat ROLE (`kind = 'role'`):
/// a persona bound to a built-in CLI body, carrying its own brain (model /
/// provider), Soul/Agents documents, skills and MCP list. Role rows never
/// launch as their own ACP servers — the launcher filters them out — so the
/// distribution columns keep placeholders (`'role'` / `'{}'`) and only the
/// columns below are read.
///
/// Existing rows keep their behaviour byte-for-byte: every new column has a
/// default that `CustomAgentDef` skips when serializing, and nothing reads the
/// role columns unless `kind = 'role'`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ColumnDef::new(CustomAgent::Kind)
                .string()
                .not_null()
                .default("acp")
                .to_owned(),
            ColumnDef::new(CustomAgent::Body).string().null().to_owned(),
            ColumnDef::new(CustomAgent::Instructions)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(CustomAgent::Rules)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(CustomAgent::BrainModel)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(CustomAgent::BrainProvider)
                .text()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(CustomAgent::SkillsJson)
                .text()
                .not_null()
                .default("[]")
                .to_owned(),
            ColumnDef::new(CustomAgent::McpJson)
                .text()
                .not_null()
                .default("[]")
                .to_owned(),
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(CustomAgent::Table)
                        .add_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            CustomAgent::Kind,
            CustomAgent::Body,
            CustomAgent::Instructions,
            CustomAgent::Rules,
            CustomAgent::BrainModel,
            CustomAgent::BrainProvider,
            CustomAgent::SkillsJson,
            CustomAgent::McpJson,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(CustomAgent::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CustomAgent {
    Table,
    Kind,
    Body,
    Instructions,
    Rules,
    BrainModel,
    BrainProvider,
    SkillsJson,
    McpJson,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn up_adds_role_columns_with_defaults_for_existing_rows() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared(
            "CREATE TABLE custom_agent (
                id INTEGER PRIMARY KEY,
                registry_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create stub table");
        conn.execute_unprepared(
            "INSERT INTO custom_agent (id, registry_id, name) VALUES (1, 'acme', 'Acme')",
        )
        .await
        .expect("insert row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration up");

        let rows = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT kind, body, instructions, rules, brain_model, brain_provider, \
                 skills_json, mcp_json FROM custom_agent"
                    .to_owned(),
            ))
            .await
            .expect("query rows");
        assert_eq!(rows.len(), 1);
        let kind: String = rows[0].try_get("", "kind").expect("kind column");
        assert_eq!(kind, "acp", "pre-existing rows keep the launchable kind");
        let body: Option<String> = rows[0].try_get("", "body").expect("body column");
        assert!(body.is_none());
        let skills: String = rows[0]
            .try_get("", "skills_json")
            .expect("skills_json column");
        assert_eq!(skills, "[]");

        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("run migration down");
    }
}

use sea_orm_migration::prelude::*;

/// [crewchat] Group conversations carry a team (Phase 1.1): the roster role
/// ids of the leader and the members, serialized as one JSON blob. Nullable —
/// every pre-existing row and every non-team session keeps `NULL`, which the
/// frontend reads as "not a group conversation". One column rather than a
/// join table is the v1 decision recorded in
/// `docs/plans/agents-core-collaboration.md` (picked exactly because the team
/// is a per-conversation snapshot, not a queryable registry).
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(ColumnDef::new(Conversation::Team).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::Team)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    Team,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn up_adds_a_nullable_team_column() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared("CREATE TABLE conversation (id INTEGER PRIMARY KEY)")
            .await
            .expect("create stub table");
        conn.execute_unprepared("INSERT INTO conversation (id) VALUES (1)")
            .await
            .expect("insert row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration up");

        let rows = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT team FROM conversation".to_owned(),
            ))
            .await
            .expect("query rows");
        assert_eq!(rows.len(), 1);
        let team: Option<String> = rows[0].try_get("", "team").expect("team column");
        assert!(team.is_none(), "pre-existing rows keep a NULL team");

        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("run migration down");
    }
}

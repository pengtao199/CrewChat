use sea_orm_migration::prelude::*;

/// [crewchat] Per-AGENT permissions, on the roster row.
///
/// The conversation used to be where permissions were decided: the composer
/// exposed the session's mode (a CLI approval/sandbox preset) and every
/// permission request interrupted the chat. The user's decision is the other way
/// round — the permission belongs to the Agent, configured once on the Agents
/// page, so a session never has to ask.
///
/// Two columns, both no-ops for anything that does not set them:
///
/// - `permission_mode` — the session-mode id the role's sessions START in.
///   Empty (`''`) means "leave the CLI / CCR default alone", which is exactly
///   today's behaviour, so existing role rows keep working byte-for-byte.
/// - `auto_approve` — answer this role's permission requests automatically
///   (allow, else the first offered option) instead of interrupting. `0` by
///   default: auto-approving is a trust decision the user makes per Agent.
///
/// Only `kind = 'role'` rows read either column; `kind = 'acp'` rows (the
/// original launchable agents) carry the defaults and are unaffected.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ColumnDef::new(CustomAgent::PermissionMode)
                .string()
                .not_null()
                .default("")
                .to_owned(),
            ColumnDef::new(CustomAgent::AutoApprove)
                .boolean()
                .not_null()
                .default(false)
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
        for column in [CustomAgent::PermissionMode, CustomAgent::AutoApprove] {
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
    PermissionMode,
    AutoApprove,
}

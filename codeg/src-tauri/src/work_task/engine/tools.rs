use super::*;

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// codeg-mcp `task_progress` / `task_complete` access handed to the delegation
/// listener at boot. Resolves the process-global engine at CALL time — the
/// listener is constructed before the engine, and a process that never wins the
/// engine lock cleanly rejects every report.
pub struct EngineWorkTaskTools;

#[async_trait::async_trait]
impl WorkTaskToolAccess for EngineWorkTaskTools {
    async fn report_progress(&self, parent_connection_id: &str, message: &str) -> TaskReportAck {
        let Some(engine) = engine() else {
            return TaskReportAck::rejected("no task engine running in this process");
        };
        engine.record_progress(parent_connection_id, message).await
    }

    async fn complete(
        &self,
        parent_connection_id: &str,
        verdict: &str,
        summary: Option<&str>,
    ) -> TaskReportAck {
        let Some(engine) = engine() else {
            return TaskReportAck::rejected("no task engine running in this process");
        };
        engine
            .record_complete(parent_connection_id, verdict, summary)
            .await
    }
}

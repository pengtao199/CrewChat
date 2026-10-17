use serde::{Deserialize, Serialize};

// [crewchat] Only English and Simplified Chinese remain: every other language
// (Traditional Chinese, Japanese, Korean, Spanish, German, French, Portuguese,
// Arabic) was retired across the app, and its arms were deleted from every
// `match lang` below.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lang {
    #[default]
    En,
    ZhCn,
}

impl Lang {
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "en" => Lang::En,
            "zh-cn" | "zh-CN" | "zh_CN" => Lang::ZhCn,
            // [crewchat] Retired locales (and unknown input) fall back to English.
            _ => Lang::En,
        }
    }
}

// ── Event messages ──

pub fn turn_complete_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "会话完成",
        Lang::En => "Turn Complete",
    }
}

pub fn turn_complete_body(lang: Lang, agent_type: &str) -> String {
    match lang {
        Lang::ZhCn => format!("{agent_type} 会话已完成"),
        Lang::En => format!("{agent_type} session completed"),
    }
}

pub fn stop_reason_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "结束原因",
        Lang::En => "Stop Reason",
    }
}

pub fn stop_reason_end_turn(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "正常结束",
        Lang::En => "Completed",
    }
}

pub fn stop_reason_cancelled(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已取消",
        Lang::En => "Cancelled",
    }
}

pub fn agent_error_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "代理错误",
        Lang::En => "Agent Error",
    }
}

pub fn agent_error_body(lang: Lang, agent_type: &str) -> String {
    match lang {
        Lang::ZhCn => format!("{agent_type} 发生错误"),
        Lang::En => format!("{agent_type} encountered an error"),
    }
}

pub fn error_message_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "错误信息",
        Lang::En => "Error Message",
    }
}

// ── Permission request (global event push) ──

pub fn permission_request_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "权限请求",
        Lang::En => "Permission Request",
    }
}

pub fn permission_request_body(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "智能体正在请求权限，请在 Codeg 中查看并批准。",
        Lang::En => "An agent is requesting permission. Review and approve it in Codeg.",
    }
}

pub fn permission_operation_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "请求操作",
        Lang::En => "Requested operation",
    }
}

// ── User message (global event push) ──

pub fn user_message_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "用户消息",
        Lang::En => "User Message",
    }
}

// ── Agent question (global event push) ──

pub fn question_request_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "智能体提问",
        Lang::En => "Agent Question",
    }
}

pub fn question_request_body(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "智能体正在向你提问，请在 Codeg 中回答。",
        Lang::En => "An agent is asking a question. Answer it in Codeg.",
    }
}

/// Field-label fallback for a question whose `header` chip is empty.
pub fn question_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "问题",
        Lang::En => "Question",
    }
}

// ── Daily report ──

pub fn daily_report_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "每日编码报告",
        Lang::En => "Daily Coding Report",
    }
}

pub fn daily_report_summary(lang: Lang, date: &str) -> String {
    match lang {
        Lang::ZhCn => format!("今日编码活动汇总 ({date})"),
        Lang::En => format!("Daily coding activity summary ({date})"),
    }
}

pub fn total_sessions(lang: Lang, count: u32) -> String {
    match lang {
        Lang::ZhCn => format!("会话总数: {count}"),
        Lang::En => format!("Total sessions: {count}"),
    }
}

pub fn by_agent_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "按代理分布:",
        Lang::En => "By agent:",
    }
}

pub fn agent_session_count(lang: Lang, agent: &str, count: u32) -> String {
    match lang {
        Lang::ZhCn => format!("{agent} - {count} 个会话"),
        Lang::En => format!("{agent} - {count} sessions"),
    }
}

pub fn projects_label(lang: Lang, projects: &str) -> String {
    match lang {
        Lang::ZhCn => format!("涉及项目: {projects}"),
        Lang::En => format!("Projects: {projects}"),
    }
}

pub fn key_activities_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "主要活动:",
        Lang::En => "Key activities:",
    }
}

// ── Command responses ──

pub fn query_failed_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "查询失败",
        Lang::En => "Query Failed",
    }
}

pub fn untitled(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "(无标题)",
        Lang::En => "(Untitled)",
    }
}

pub fn search_no_results(lang: Lang, keyword: &str) -> String {
    match lang {
        Lang::ZhCn => format!("未找到包含 \"{keyword}\" 的会话"),
        Lang::En => format!("No conversations found matching \"{keyword}\""),
    }
}

pub fn search_results_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "搜索结果",
        Lang::En => "Search Results",
    }
}

pub fn search_results_count_title(lang: Lang, keyword: &str, count: usize) -> String {
    match lang {
        Lang::ZhCn => format!("搜索 \"{keyword}\" - {count} 条结果"),
        Lang::En => format!("Search \"{keyword}\" - {count} results"),
    }
}

pub fn no_activity_today(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "今日暂无编码活动",
        Lang::En => "No coding activity today",
    }
}

pub fn today_activity_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "今日活动",
        Lang::En => "Today's Activity",
    }
}

pub fn today_activity_date_title(lang: Lang, date: &str) -> String {
    match lang {
        Lang::ZhCn => format!("今日活动 ({date})"),
        Lang::En => format!("Today's Activity ({date})"),
    }
}

pub fn agent_count(lang: Lang, agent: &str, count: u32) -> String {
    match lang {
        Lang::ZhCn => format!("{agent} - {count} 个"),
        Lang::En => {
            format!("{agent} - {count}")
        }
    }
}

pub fn recent_activity_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "最近活动:",
        Lang::En => "Recent activity:",
    }
}

pub fn no_active_channels(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "暂无活跃渠道",
        Lang::En => "No active channels",
    }
}

pub fn channel_status_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "渠道状态",
        Lang::En => "Channel Status",
    }
}

pub fn help_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "Codeg Bot 帮助",
        Lang::En => "Codeg Bot Help",
    }
}

pub fn help_body(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!(
            "{prefix}folder - 选择工作目录\n\
             {prefix}agent - 选择 Agent\n\
             {prefix}task <描述> - 创建会话并执行任务\n\
             {prefix}sessions - 当前目录的活跃会话\n\
             {prefix}resume [ID] - 最近会话 / 恢复指定会话\n\
             {prefix}cancel - 取消当前任务\n\
             {prefix}approve [always] - 批准权限请求\n\
             {prefix}deny - 拒绝权限请求\n\
             \n\
             {prefix}search <关键词> - 搜索会话\n\
             {prefix}today - 今日活动汇总\n\
             {prefix}status - 渠道连接状态\n\
             {prefix}help - 显示帮助\n\
             \n\
             有活跃会话时，直接发文本即可继续对话"
        ),
        Lang::En => format!(
            "{prefix}folder - Select working folder\n\
             {prefix}agent - Select agent\n\
             {prefix}task <desc> - Create session & run task\n\
             {prefix}sessions - Active sessions in folder\n\
             {prefix}resume [ID] - Recent conversations / resume a session\n\
             {prefix}cancel - Cancel current task\n\
             {prefix}approve [always] - Approve permission\n\
             {prefix}deny - Deny permission\n\
             \n\
             {prefix}search <keyword> - Search conversations\n\
             {prefix}today - Today's activity summary\n\
             {prefix}status - Channel connection status\n\
             {prefix}help - Show help\n\
             \n\
             When a session is active, just type text to continue the conversation"
        ),
    }
}

// ── Command dispatcher messages ──

pub fn invalid_args_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "参数错误",
        Lang::En => "Invalid Arguments",
    }
}

pub fn search_usage(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("用法: {prefix}search <关键词>"),
        Lang::En => format!("Usage: {prefix}search <keyword>"),
    }
}

pub fn unknown_command(lang: Lang, prefix: &str, command: &str) -> String {
    match lang {
        Lang::ZhCn => format!("未知命令: {prefix}{command}\n输入 {prefix}help 查看可用命令"),
        Lang::En => {
            format!("Unknown command: {prefix}{command}\nType {prefix}help for available commands")
        }
    }
}

pub fn unknown_command_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "未知命令",
        Lang::En => "Unknown Command",
    }
}

// ── Session command messages ──

// Folder (/folder)
pub fn folder_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "工作目录",
        Lang::En => "Working Folder",
    }
}

pub fn no_folders_found(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "没有找到项目目录。",
        Lang::En => "No folders found.",
    }
}

pub fn folder_select_hint(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("回复 {prefix}folder <数字> 选择目录。"),
        Lang::En => format!("Reply {prefix}folder <number> to select."),
    }
}

pub fn index_starts_from_one(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "序号从 1 开始。",
        Lang::En => "Index starts from 1.",
    }
}

pub fn folder_index_out_of_range(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("序号超出范围，请使用 {prefix}folder 查看列表。"),
        Lang::En => format!("Index out of range. Use {prefix}folder to list."),
    }
}

pub fn folder_selected_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已选择目录",
        Lang::En => "Folder Selected",
    }
}

pub fn folder_not_found(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "目录不存在。",
        Lang::En => "Folder not found.",
    }
}

pub fn folder_not_found_with_hint(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("目录不存在，请使用 {prefix}folder 重新选择。"),
        Lang::En => format!("Folder not found. Use {prefix}folder to select."),
    }
}

pub fn no_folder_selected(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("未选择工作目录，请先使用 {prefix}folder 选择。"),
        Lang::En => format!("No folder selected. Use {prefix}folder first."),
    }
}

// Agent (/agent)
pub fn agent_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "选择 Agent",
        Lang::En => "Agent Selection",
    }
}

pub fn agent_select_hint(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("回复 {prefix}agent <数字> 或 {prefix}agent <名称> 选择。"),
        Lang::En => format!("Reply {prefix}agent <number> or {prefix}agent <name> to select."),
    }
}

pub fn agent_index_out_of_range(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("序号超出范围，请使用 {prefix}agent 查看列表。"),
        Lang::En => format!("Index out of range. Use {prefix}agent to list."),
    }
}

pub fn agent_selected_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已选择 Agent",
        Lang::En => "Agent Selected",
    }
}

pub fn unknown_agent_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "未知 Agent: ",
        Lang::En => "Unknown agent: ",
    }
}

// Task (/task)
pub fn task_usage(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("用法: {prefix}task <任务描述>"),
        Lang::En => format!("Usage: {prefix}task <description>"),
    }
}

pub fn no_agent_selected(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("未选择 Agent，请先使用 {prefix}agent 选择，或在工作目录上设置默认 Agent。"),
        Lang::En => format!("No agent selected. Use {prefix}agent to pick one or set a default on the folder."),
    }
}

pub fn failed_to_start_agent_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "启动 Agent 失败: ",
        Lang::En => "Failed to start agent: ",
    }
}

pub fn task_started_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "任务已启动",
        Lang::En => "Task Started",
    }
}

// Sessions (/sessions)
pub fn sessions_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "会话列表",
        Lang::En => "Sessions",
    }
}

pub fn no_active_sessions_in_folder(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "当前目录没有进行中的会话。",
        Lang::En => "No active sessions in this folder.",
    }
}

pub fn sessions_resume_hint(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("回复 {prefix}resume <会话ID> 继续会话。"),
        Lang::En => format!("Reply {prefix}resume <id> to continue."),
    }
}

// Resume (/resume)
pub fn conversation_not_found(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "会话不存在。",
        Lang::En => "Conversation not found.",
    }
}

pub fn session_resumed_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "会话已恢复",
        Lang::En => "Session Resumed",
    }
}

pub fn no_conversations_found(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "暂无会话记录。",
        Lang::En => "No conversations found.",
    }
}

pub fn recent_conversations_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "最近会话",
        Lang::En => "Recent Conversations",
    }
}

pub fn recent_resume_hint(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("回复 {prefix}resume <会话ID> 恢复会话。"),
        Lang::En => format!("Reply {prefix}resume <id> to resume a session."),
    }
}

// Cancel (/cancel)
pub fn no_active_session_to_cancel(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "没有进行中的任务可取消。",
        Lang::En => "No active session to cancel.",
    }
}

pub fn task_cancelled_body(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "当前任务已取消。",
        Lang::En => "Current task has been cancelled.",
    }
}

pub fn task_cancelled_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "任务已取消",
        Lang::En => "Task Cancelled",
    }
}

// Permission (/approve, /deny)
pub fn no_active_session(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "没有活跃的会话。",
        Lang::En => "No active session.",
    }
}

pub fn no_active_session_found(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "未找到活跃的会话。",
        Lang::En => "No active session found.",
    }
}

pub fn no_pending_permission(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "没有待处理的权限请求。",
        Lang::En => "No pending permission request.",
    }
}

pub fn no_valid_permission_option(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "未找到有效的权限选项。",
        Lang::En => "No valid permission option found.",
    }
}

pub fn failed_permission_response_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "权限响应失败: ",
        Lang::En => "Failed to respond to permission: ",
    }
}

pub fn approved_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已批准",
        Lang::En => "Approved",
    }
}

pub fn denied_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已拒绝",
        Lang::En => "Denied",
    }
}

pub fn auto_approve_enabled(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "已启用自动批准。",
        Lang::En => "Auto-approve enabled for this session.",
    }
}

pub fn permission_response_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "权限响应",
        Lang::En => "Permission Response",
    }
}

// Follow-up
pub fn no_active_session_use_task(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("没有活跃的会话，请使用 {prefix}task 开始新任务。"),
        Lang::En => format!("No active session. Use {prefix}task to start one."),
    }
}

pub fn session_connection_lost(lang: Lang, prefix: &str) -> String {
    match lang {
        Lang::ZhCn => format!("会话连接已断开，请使用 {prefix}task 开始新任务。"),
        Lang::En => format!("Session connection lost. Use {prefix}task to start a new one."),
    }
}

pub fn failed_to_send_message_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "发送消息失败: ",
        Lang::En => "Failed to send message: ",
    }
}

pub fn message_sent(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "消息已发送。",
        Lang::En => "Message sent.",
    }
}

/// Shown when a prompt is rejected because the agent is still processing the
/// previous turn. Transient — the session stays alive; the user retries.
pub fn agent_busy_retry(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "智能体正在处理上一条消息，请稍后再发送。",
        Lang::En => {
            "The agent is still processing the previous message — please send again in a moment."
        }
    }
}

/// Shown when a task's initial prompt arrives while another turn is already in
/// flight on the same (shared) connection. Unlike `agent_busy_retry`, the user
/// does NOT need to resend — the kickoff is deferred and runs automatically
/// once the current turn finishes.
pub fn task_deferred_busy(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "智能体正忙，任务将在当前回合结束后自动开始。",
        Lang::En => {
            "The agent is busy — your task will start automatically once the current turn finishes."
        }
    }
}

// Internal error labels
pub fn failed_to_list_folders_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "列出目录失败: ",
        Lang::En => "Failed to list folders: ",
    }
}

pub fn failed_to_add_folder_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "添加目录失败: ",
        Lang::En => "Failed to add folder: ",
    }
}

pub fn failed_to_load_context_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "加载上下文失败: ",
        Lang::En => "Failed to load context: ",
    }
}

pub fn failed_to_create_conversation_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "创建会话失败: ",
        Lang::En => "Failed to create conversation: ",
    }
}

pub fn failed_to_list_sessions_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhCn => "列出会话失败: ",
        Lang::En => "Failed to list sessions: ",
    }
}

// ── Session progress messages ──

pub fn agent_responding(lang: Lang, agent_label: &str) -> String {
    match lang {
        Lang::ZhCn => format!("{agent_label} 正在响应中..."),
        Lang::En => format!("{agent_label} is responding..."),
    }
}

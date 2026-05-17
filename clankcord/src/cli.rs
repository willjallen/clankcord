use std::fs;
use std::io::Read;
use std::time::Duration;

use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::messages::{read as read_messages, search as search_messages};
use crate::errors::discord_tool_error;

const CLI_AFTER_HELP: &str = r#"Common agent workflows:
  Inspect recent memory:      clankcord timeline tail --since -10m --file timeline.json
  Render transcript context:  clankcord transcripts render --since -30m --file transcript.md --format markdown
  Search agent sessions:      clankcord agent-sessions search --query "floating point" --file sessions.json
  Resolve a person:           clankcord members resolve "display name"
  Publish a visible reply:    clankcord responses send <<'EOF'
                              message body
                              EOF
  Ask a clarifying question:  clankcord responses ask <<'EOF'
                              question text
                              EOF
  Send a private reply:       clankcord responses dm --to "display name" <<'EOF'
                              private message
                              EOF
  Create automation:          clankcord automations spec
                              clankcord automations validate < automation.json
                              clankcord automations create < automation.json
  Submit product feedback:    clankcord feedback submit <<'EOF'
                              feedback body
                              EOF

Most agent commands infer job, guild, channel, and requester from CLANKCORD_AGENT_* environment variables. Use --file for large outputs so command results do not flood the agent context."#;

const RESPONSE_BODY_AFTER_HELP: &str = r#"Response body input:
  Read Markdown/plain text from stdin by default. Use a single-quoted heredoc so shells do not expand backticks, dollars, quotes, or backslashes:

    clankcord responses send --job "$CLANKCORD_AGENT_JOB_ID" <<'EOF'
    Markdown with `code`, ``` fences, "$quotes", $vars, and \slashes.
    EOF

  Or read the body from a UTF-8 file:

    clankcord responses send --file response.md

Common forms:
    clankcord responses send <<'EOF'
    visible reply
    EOF

    clankcord responses ask <<'EOF'
    clarifying question
    EOF

    clankcord responses dm --to "display name" <<'EOF'
    private reply
    EOF

Targets:
  send/submit default to the current session.
  ask sends a visible question and marks expects_reply=true.
  dm resolves --to through guild member lookup and sends a private Discord DM.
  Explicit sink values include session, agent-chat, channel:<id>, and dm:<user-id>."#;

const AUTOMATION_BODY_AFTER_HELP: &str = r#"Automation JSON input:
  Read JSON from stdin by default:

    clankcord automations validate < automation.json
    clankcord automations create < automation.json

  Or read JSON from a UTF-8 file:

    clankcord automations validate --file automation.json
    clankcord automations create --file automation.json

Read the schema first with clankcord automations spec. Validate before create."#;

#[derive(Debug, Parser)]
#[command(name = "clankcord")]
#[command(
    about = "CLI action boundary for Clankcord's Discord memory, jobs, automations, and responses.",
    long_about = "Clankcord is the supported command surface for agents and operators. Use it to inspect Discord voice-room memory, render transcript context, resolve members, control room behavior, create jobs and automations, and publish responses back through Clankcord.",
    after_help = CLI_AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Run the persistent Clankcord runtime process.")]
    Start,
    #[command(about = "Show live runtime, room, voice bot, and capture-session status.")]
    Status(StatusArgs),
    #[command(
        about = "Inspect and control voice-room occupancy, bot placement, mute state, and cues."
    )]
    Rooms {
        #[command(subcommand)]
        command: Option<RoomsCommand>,
    },
    #[command(about = "Read or search Discord text messages through the bot account.")]
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    #[command(about = "Inspect raw timeline events for recent room memory and job activity.")]
    Timeline {
        #[command(subcommand)]
        command: TimelineCommand,
    },
    #[command(about = "Materialize, render, and search voice transcripts.")]
    Transcripts {
        #[command(subcommand)]
        command: TranscriptsCommand,
    },
    #[command(about = "List detected conversation windows in the timeline.")]
    Conversations {
        #[command(subcommand)]
        command: ConversationsCommand,
    },
    #[command(about = "Resolve natural-language time references into timeline ranges.")]
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    #[command(about = "Trace a Discord participant's presence and speech over time.")]
    Participants {
        #[command(subcommand)]
        command: ParticipantsCommand,
    },
    #[command(about = "Search, resolve, and inspect Discord guild members.")]
    Members {
        #[command(subcommand)]
        command: MembersCommand,
    },
    #[command(about = "Find, inspect, sunset, and resume agent sessions.")]
    AgentSessions {
        #[command(subcommand)]
        command: AgentSessionsCommand,
    },
    #[command(about = "List, inspect, retry, and run Clankcord runtime jobs.")]
    Jobs {
        #[command(subcommand)]
        command: Option<JobsCommand>,
    },
    #[command(about = "Publish public replies, questions, and DMs through Clankcord.")]
    Responses {
        #[command(subcommand)]
        command: ResponsesCommand,
    },
    #[command(about = "Print, validate, create, list, inspect, and cancel automations.")]
    Automations {
        #[command(subcommand)]
        command: AutomationsCommand,
    },
    #[command(about = "Submit durable feedback about missing or unsupported Clankcord behavior.")]
    Feedback {
        #[command(subcommand)]
        command: FeedbackCommand,
    },
    #[command(about = "Approve or cancel confirmation-required jobs.")]
    Confirmations {
        #[command(subcommand)]
        command: ConfirmationsCommand,
    },
    #[command(about = "Pause room listening for a bounded duration.")]
    Pause(PauseArgs),
    #[command(about = "Resume room listening.")]
    Resume(RoomArgs),
    #[command(about = "Forget a recent transcript window or explicit window id.")]
    Forget(ForgetArgs),
}

#[derive(Debug, Subcommand)]
enum RoomsCommand {
    #[command(about = "Show room and voice-bot status.")]
    Status(StatusArgs),
    #[command(about = "List live occupants for a room.")]
    Occupants(RoomOccupantsArgs),
    #[command(about = "Request a voice bot join a room.")]
    Join(JoinArgs),
    #[command(about = "Request a voice bot leave a room.")]
    Leave(RoomArgs),
    #[command(about = "Move a named voice bot to another room.")]
    Move(MoveArgs),
    #[command(about = "Mute the active voice session for a room.")]
    Mute(RoomArgs),
    #[command(about = "Unmute the active voice session for a room.")]
    Unmute(RoomArgs),
    #[command(about = "Play a named cue in a room.")]
    PlayCue(PlayCueArgs),
}

#[derive(Debug, Subcommand)]
enum MessagesCommand {
    #[command(about = "Read recent messages from a channel or thread.")]
    Read(read_messages::Args),
    #[command(about = "Search Discord text channels, forums, and threads.")]
    Search(search_messages::Args),
}

#[derive(Debug, Subcommand)]
enum TimelineCommand {
    #[command(about = "Show recent timeline events.")]
    Tail(TimelineTailArgs),
    #[command(about = "Show timeline events in an explicit time range.")]
    Range(TimelineRangeArgs),
}

#[derive(Debug, Subcommand)]
enum TranscriptsCommand {
    #[command(about = "Create a transcript window job.")]
    Materialize(TranscriptMaterializeArgs),
    #[command(about = "Render transcript text for a window or time range.")]
    Render(TranscriptRenderArgs),
    #[command(about = "Search transcript text over recent history.")]
    Search(TranscriptSearchArgs),
}

#[derive(Debug, Subcommand)]
enum ConversationsCommand {
    #[command(about = "List conversation windows for a room or guild.")]
    List(ConversationsListArgs),
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    #[command(about = "Resolve a natural-language reference such as 'last hour' or 'yesterday'.")]
    Resolve(ContextResolveArgs),
}

#[derive(Debug, Subcommand)]
enum ParticipantsCommand {
    #[command(about = "Trace one participant's presence and optional speech snippets.")]
    Trace(ParticipantTraceArgs),
}

#[derive(Debug, Subcommand)]
enum MembersCommand {
    #[command(about = "Search guild members by name or id.")]
    Search(MemberSearchArgs),
    #[command(about = "Resolve one member name or id to a Discord user.")]
    Resolve(MemberResolveArgs),
    #[command(about = "Get one member by Discord user id.")]
    Get(MemberGetArgs),
}

#[derive(Debug, Subcommand)]
enum AgentSessionsCommand {
    #[command(about = "Show the current active agent session for a voice route.")]
    Current(AgentSessionsCurrentArgs),
    #[command(about = "List agent sessions.")]
    List(AgentSessionsListArgs),
    #[command(about = "Search agent session history.")]
    Search(AgentSessionsSearchArgs),
    #[command(about = "Inspect one agent session.")]
    Get(AgentSessionGetArgs),
    #[command(about = "Retire one agent session.")]
    Sunset(AgentSessionSunsetArgs),
    #[command(about = "Reactivate a retired session on the requested route.")]
    Resume(AgentSessionResumeArgs),
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    #[command(about = "List runtime jobs.")]
    List(JobsListArgs),
    #[command(about = "Inspect one runtime job.")]
    Get(JobGetArgs),
    #[command(about = "Retry a failed or cancelled job.")]
    Retry(JobIdArg),
    #[command(about = "Run due jobs immediately in the runtime.")]
    RunDue,
}

#[derive(Debug, Subcommand)]
enum ResponsesCommand {
    #[command(
        about = "Send a visible response to the current session or explicit sink.",
        after_help = RESPONSE_BODY_AFTER_HELP
    )]
    Send(ResponseSubmitArgs),
    #[command(
        about = "Resolve a member and send a private DM response.",
        after_help = RESPONSE_BODY_AFTER_HELP
    )]
    Dm(ResponseDmArgs),
    #[command(
        about = "Alias for send; publishes a visible response.",
        after_help = RESPONSE_BODY_AFTER_HELP
    )]
    Submit(ResponseSubmitArgs),
    #[command(
        about = "Send a visible clarifying question.",
        after_help = RESPONSE_BODY_AFTER_HELP
    )]
    Ask(ResponseSubmitArgs),
}

#[derive(Debug, Subcommand)]
enum AutomationsCommand {
    #[command(about = "Print the automation JSON spec manual.")]
    Spec,
    #[command(
        about = "Create an automation from JSON read from stdin or --file.",
        after_help = AUTOMATION_BODY_AFTER_HELP
    )]
    Create(AutomationSpecArgs),
    #[command(
        about = "Validate automation JSON read from stdin or --file.",
        after_help = AUTOMATION_BODY_AFTER_HELP
    )]
    Validate(AutomationSpecArgs),
    #[command(
        about = "Evaluate automation JSON without creating it.",
        after_help = AUTOMATION_BODY_AFTER_HELP
    )]
    DryRun(AutomationSpecArgs),
    #[command(about = "List automations by optional guild, channel, and state.")]
    List(AutomationListArgs),
    #[command(about = "Inspect one automation by id.")]
    Get(AutomationIdArg),
    #[command(about = "Cancel one automation by id.")]
    Cancel(AutomationIdArg),
}

#[derive(Debug, Subcommand)]
enum FeedbackCommand {
    #[command(about = "Record feedback text in the current room timeline.")]
    Submit(FeedbackSubmitArgs),
}

#[derive(Debug, Subcommand)]
enum ConfirmationsCommand {
    #[command(about = "Approve a confirmation-required job.")]
    Approve(ConfirmationApproveArgs),
    #[command(about = "Cancel a confirmation-required job.")]
    Cancel(ConfirmationCancelArgs),
}

#[derive(Debug, ClapArgs, Default)]
struct StatusArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct RoomOccupantsArgs {
    room: String,
    #[arg(long)]
    guild: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct RoomArgs {
    room: Option<String>,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    requested_by_user_id: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct JoinArgs {
    room: Option<String>,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    user_id: Option<String>,
    #[arg(long, default_value = "explicit_request")]
    reason: String,
}

#[derive(Debug, ClapArgs, Default)]
struct MoveArgs {
    #[arg(long)]
    bot: String,
    #[arg(long)]
    to: String,
    #[arg(long, default_value = "admin_force_move")]
    reason: String,
}

#[derive(Debug, ClapArgs, Default)]
struct PlayCueArgs {
    cue: String,
    room: Option<String>,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    requested_by_user_id: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct TimelineTailArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, default_value = "-1h")]
    since: String,
    #[arg(long, default_value_t = 200)]
    limit: usize,
    #[arg(long)]
    ephemeral: bool,
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct TimelineRangeArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    from: String,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    all_channels: bool,
    #[arg(long, default_value_t = 500)]
    limit: usize,
    #[arg(long)]
    ephemeral: bool,
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct ConversationsListArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    all_channels: bool,
    #[arg(long, default_value = "-2d")]
    since: String,
}

#[derive(Debug, ClapArgs, Default)]
struct ContextResolveArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    channel: String,
    #[arg(long)]
    reference: String,
}

#[derive(Debug, ClapArgs, Default)]
struct ParticipantTraceArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    user: String,
    #[arg(long)]
    from: String,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    include_speech_snippets: bool,
}

#[derive(Debug, ClapArgs, Default)]
struct TranscriptMaterializeArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, default_value = "-10m")]
    since: String,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long, default_value = "local")]
    publish: String,
    #[arg(long)]
    draft_only: bool,
    #[arg(long)]
    live: bool,
    #[arg(long)]
    refine: bool,
}

#[derive(Debug, ClapArgs, Default)]
struct TranscriptRenderArgs {
    #[arg(long)]
    window: Option<String>,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, default_value = "-1h")]
    since: String,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    no_prefer_refined: bool,
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct TranscriptSearchArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    all_channels: bool,
    #[arg(long)]
    query: String,
    #[arg(long, default_value = "-7d")]
    since: String,
    #[arg(long)]
    no_prefer_refined: bool,
    #[arg(long, default_value_t = 50)]
    limit: u64,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct JobsListArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long)]
    ephemeral: bool,
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs)]
struct JobIdArg {
    job_id: String,
}

#[derive(Debug, ClapArgs)]
struct JobGetArgs {
    job_id: String,
    #[arg(long)]
    ephemeral: bool,
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct MemberSearchArgs {
    query: String,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct MemberResolveArgs {
    query: String,
    #[arg(long)]
    guild: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct MemberGetArgs {
    user_id: String,
    #[arg(long)]
    guild: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct AgentSessionsCurrentArgs {
    #[arg(long)]
    guild: String,
    #[arg(long)]
    channel: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct AgentSessionsListArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs, Default)]
struct AgentSessionsSearchArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long)]
    query: String,
    #[arg(long, default_value = "-30d")]
    since: String,
    #[arg(long, default_value_t = 25)]
    limit: usize,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs)]
struct AgentSessionGetArgs {
    agent_session_id: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, ClapArgs)]
struct AgentSessionSunsetArgs {
    agent_session_id: String,
    #[arg(long)]
    requested_by_user_id: Option<String>,
    #[arg(long)]
    reason: String,
}

#[derive(Debug, ClapArgs)]
struct AgentSessionResumeArgs {
    agent_session_id: String,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    dm_user: Option<String>,
    #[arg(long)]
    requested_by_user_id: Option<String>,
    #[arg(long)]
    message: Option<String>,
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct ResponseSubmitArgs {
    #[arg(long, help = "Source job id. Defaults to CLANKCORD_AGENT_JOB_ID.")]
    job: Option<String>,
    #[arg(
        long,
        default_value = "session",
        help = "Response target: session, agent-chat, channel:<id>, or dm:<user-id>."
    )]
    sink: String,
    #[arg(long, help = "Discord guild id. Defaults to CLANKCORD_AGENT_GUILD_ID.")]
    guild: Option<String>,
    #[arg(
        long,
        help = "Discord voice channel id. Defaults to CLANKCORD_AGENT_VOICE_CHANNEL_ID."
    )]
    channel: Option<String>,
    #[arg(
        long,
        help = "Requesting Discord user id. Defaults to CLANKCORD_AGENT_REQUESTED_BY_USER_ID."
    )]
    requested_by_user_id: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Read response body from a UTF-8 file instead of stdin."
    )]
    file: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct ResponseDmArgs {
    #[arg(
        long,
        help = "Recipient display name, username, mention, or Discord user id."
    )]
    to: String,
    #[arg(long, help = "Source job id. Defaults to CLANKCORD_AGENT_JOB_ID.")]
    job: Option<String>,
    #[arg(long, help = "Discord guild id. Defaults to CLANKCORD_AGENT_GUILD_ID.")]
    guild: Option<String>,
    #[arg(
        long,
        help = "Discord voice channel id. Defaults to CLANKCORD_AGENT_VOICE_CHANNEL_ID."
    )]
    channel: Option<String>,
    #[arg(
        long,
        help = "Requesting Discord user id. Defaults to CLANKCORD_AGENT_REQUESTED_BY_USER_ID."
    )]
    requested_by_user_id: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Read DM body from a UTF-8 file instead of stdin."
    )]
    file: Option<String>,
}

#[derive(Debug, ClapArgs, Default, Clone)]
struct OutputArgs {
    #[arg(long, default_value = "json")]
    format: String,
    #[arg(long)]
    file: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct AutomationSpecArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Read automation JSON from a UTF-8 file instead of stdin."
    )]
    file: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct AutomationListArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    state: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct AutomationIdArg {
    automation_id: String,
}

#[derive(Debug, ClapArgs, Default)]
struct FeedbackSubmitArgs {
    #[arg(long, help = "Source job id. Defaults to CLANKCORD_AGENT_JOB_ID.")]
    job: Option<String>,
    #[arg(long, help = "Discord guild id. Defaults to CLANKCORD_AGENT_GUILD_ID.")]
    guild: Option<String>,
    #[arg(
        long,
        help = "Discord voice channel id. Defaults to CLANKCORD_AGENT_VOICE_CHANNEL_ID."
    )]
    channel: Option<String>,
    #[arg(
        long,
        help = "Requesting Discord user id. Defaults to CLANKCORD_AGENT_REQUESTED_BY_USER_ID."
    )]
    requested_by_user_id: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Read feedback body from a UTF-8 file instead of stdin."
    )]
    file: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct ConfirmationApproveArgs {
    job_id: String,
    #[arg(long)]
    approved_by_user_id: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct ConfirmationCancelArgs {
    job_id: String,
    #[arg(long)]
    cancelled_by_user_id: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct PauseArgs {
    room: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, default_value = "20m")]
    duration: String,
    #[arg(long)]
    requested_by_user_id: Option<String>,
}

#[derive(Debug, ClapArgs, Default)]
struct ForgetArgs {
    #[arg(long)]
    window: Option<String>,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long, default_value = "-10m")]
    since: String,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    published: bool,
    #[arg(long)]
    requested_by_user_id: Option<String>,
}

pub fn main(argv: Vec<String>) -> i32 {
    match run(argv) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

pub fn run(argv: Vec<String>) -> Result<i32> {
    match Cli::try_parse_from(std::iter::once("clankcord".to_string()).chain(argv)) {
        Ok(cli) => run_cli(cli),
        Err(error) => {
            let _ = error.print();
            Ok(if error.use_stderr() { 2 } else { 0 })
        }
    }
}

fn run_cli(cli: Cli) -> Result<i32> {
    let Some(command) = cli.command else {
        let mut command = Cli::command();
        command.print_help()?;
        println!();
        return Ok(0);
    };
    match command {
        Command::Start => Ok(crate::runtime::start_blocking()),
        Command::Status(args) => status(args),
        Command::Rooms { command } => {
            match command.unwrap_or(RoomsCommand::Status(StatusArgs::default())) {
                RoomsCommand::Status(args) => status(args),
                RoomsCommand::Occupants(args) => room_occupants(args),
                RoomsCommand::Join(args) => join(args),
                RoomsCommand::Leave(args) => leave(args),
                RoomsCommand::Move(args) => room_move(args),
                RoomsCommand::Mute(args) => room_set_mute(args, true),
                RoomsCommand::Unmute(args) => room_set_mute(args, false),
                RoomsCommand::PlayCue(args) => room_play_cue(args),
            }
        }
        Command::Messages { command } => match command {
            MessagesCommand::Read(args) => read_messages::run(args),
            MessagesCommand::Search(args) => search_messages::run(args),
        },
        Command::Timeline { command } => match command {
            TimelineCommand::Tail(args) => timeline_tail(args),
            TimelineCommand::Range(args) => timeline_range(args),
        },
        Command::Transcripts { command } => match command {
            TranscriptsCommand::Materialize(args) => transcript_materialize(args),
            TranscriptsCommand::Render(args) => transcript_render(args),
            TranscriptsCommand::Search(args) => transcript_search(args),
        },
        Command::Conversations { command } => match command {
            ConversationsCommand::List(args) => conversations_list(args),
        },
        Command::Context { command } => match command {
            ContextCommand::Resolve(args) => context_resolve(args),
        },
        Command::Participants { command } => match command {
            ParticipantsCommand::Trace(args) => participant_trace(args),
        },
        Command::Members { command } => match command {
            MembersCommand::Search(args) => members_search(args),
            MembersCommand::Resolve(args) => members_resolve(args),
            MembersCommand::Get(args) => members_get(args),
        },
        Command::AgentSessions { command } => match command {
            AgentSessionsCommand::Current(args) => agent_sessions_current(args),
            AgentSessionsCommand::List(args) => agent_sessions_list(args),
            AgentSessionsCommand::Search(args) => agent_sessions_search(args),
            AgentSessionsCommand::Get(args) => agent_sessions_get(args),
            AgentSessionsCommand::Sunset(args) => agent_sessions_sunset(args),
            AgentSessionsCommand::Resume(args) => agent_sessions_resume(args),
        },
        Command::Jobs { command } => {
            match command.unwrap_or(JobsCommand::List(JobsListArgs::default())) {
                JobsCommand::List(args) => jobs_list(args),
                JobsCommand::Get(args) => jobs_get(args),
                JobsCommand::Retry(args) => api_emit(
                    "POST",
                    &format!("/v1/voice/jobs/{}/retry", args.job_id),
                    None,
                    None,
                ),
                JobsCommand::RunDue => api_emit("POST", "/v1/voice/jobs/run-due", None, None),
            }
        }
        Command::Responses { command } => match command {
            ResponsesCommand::Send(args) => response_submit(args, "message"),
            ResponsesCommand::Dm(args) => response_dm(args),
            ResponsesCommand::Submit(args) => response_submit(args, "message"),
            ResponsesCommand::Ask(args) => response_submit(args, "question"),
        },
        Command::Automations { command } => match command {
            AutomationsCommand::Spec => {
                print!("{}", crate::runtime::automations::AUTOMATION_SPEC_MANUAL);
                Ok(0)
            }
            AutomationsCommand::Create(args) => automation_spec(args, "/v1/voice/automations"),
            AutomationsCommand::Validate(args) => {
                automation_spec(args, "/v1/voice/automations/validate")
            }
            AutomationsCommand::DryRun(args) => {
                automation_spec(args, "/v1/voice/automations/dry-run")
            }
            AutomationsCommand::List(args) => automations_list(args),
            AutomationsCommand::Get(args) => api_emit(
                "GET",
                &format!("/v1/voice/automations/{}", args.automation_id),
                None,
                None,
            ),
            AutomationsCommand::Cancel(args) => api_emit(
                "POST",
                &format!("/v1/voice/automations/{}/cancel", args.automation_id),
                None,
                None,
            ),
        },
        Command::Feedback { command } => match command {
            FeedbackCommand::Submit(args) => feedback_submit(args),
        },
        Command::Confirmations { command } => match command {
            ConfirmationsCommand::Approve(args) => confirmation_approve(args),
            ConfirmationsCommand::Cancel(args) => confirmation_cancel(args),
        },
        Command::Pause(args) => pause(args),
        Command::Resume(args) => resume(args),
        Command::Forget(args) => forget(args),
    }
}

fn status(args: StatusArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/status",
        None,
        Some(json!({"guild": args.guild, "channel": args.channel})),
    )
}

fn room_occupants(args: RoomOccupantsArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/rooms/occupants",
        None,
        Some(json!({"guild": agent_context_guild(args.guild), "room": args.room})),
        &args.output,
    )
}

fn join(args: JoinArgs) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "join_room",
        args.guild,
        room.clone(),
        args.user_id,
        json!({"room": room, "request": args.reason}),
    )
}

fn leave(args: RoomArgs) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "leave_room",
        args.guild,
        room.clone(),
        args.requested_by_user_id,
        json!({"room": room}),
    )
}

fn room_move(args: MoveArgs) -> Result<i32> {
    submit_command(
        "join_room",
        None,
        Some(args.to.clone()),
        None,
        json!({"target_room": args.to, "request": args.reason, "bot_id": args.bot}),
    )
}

fn room_set_mute(args: RoomArgs, muted: bool) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "set_voice_mute",
        args.guild,
        room.clone(),
        args.requested_by_user_id,
        json!({"room": room, "muted": muted}),
    )
}

fn room_play_cue(args: PlayCueArgs) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "play_voice_cue",
        args.guild,
        room.clone(),
        args.requested_by_user_id,
        json!({"room": room, "cue": args.cue}),
    )
}

fn timeline_tail(args: TimelineTailArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/timeline/tail",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "since": args.since,
            "limit": args.limit,
            "ephemeral": args.ephemeral,
            "verbose": args.verbose,
        })),
        &args.output,
    )
}

fn timeline_range(args: TimelineRangeArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/timeline/range",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "from": args.from,
            "to": args.to,
            "allChannels": args.all_channels,
            "limit": args.limit,
            "ephemeral": args.ephemeral,
            "verbose": args.verbose,
        })),
        &args.output,
    )
}

fn conversations_list(args: ConversationsListArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/conversations/list",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "allChannels": args.all_channels,
            "since": args.since,
        })),
    )
}

fn context_resolve(args: ContextResolveArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/context/resolve",
        None,
        Some(json!({"guild": args.guild, "channel": args.channel, "reference": args.reference})),
    )
}

fn participant_trace(args: ParticipantTraceArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/participant/trace",
        None,
        Some(json!({
            "guild": args.guild,
            "user": args.user,
            "from": args.from,
            "to": args.to,
            "includeSpeechSnippets": args.include_speech_snippets,
        })),
    )
}

fn transcript_materialize(args: TranscriptMaterializeArgs) -> Result<i32> {
    let command_kind = if args.live {
        "start_live_transcript"
    } else {
        "materialize_transcript"
    };
    let has_from = args.from.is_some();
    submit_command(
        command_kind,
        args.guild,
        args.channel,
        None,
        json!({
            "relative_start": if has_from { String::new() } else { args.since },
            "from": args.from.unwrap_or_default(),
            "to": args.to.unwrap_or_default(),
            "publish": if args.draft_only { "local".to_string() } else { args.publish },
            "refine": args.refine,
        }),
    )
}

fn transcript_render(args: TranscriptRenderArgs) -> Result<i32> {
    let result = api_request(
        "GET",
        "/v1/voice/transcript/render",
        None,
        Some(json!({
            "window": args.window,
            "guild": args.guild,
            "channel": args.channel,
            "since": if args.from.is_some() { String::new() } else { args.since },
            "from": args.from.unwrap_or_default(),
            "to": args.to.unwrap_or_default(),
            "preferRefined": !args.no_prefer_refined,
            "format": args.output.format.clone(),
            "verbose": args.verbose,
        })),
    )?;
    emit_output(result, &args.output)
}

fn transcript_search(args: TranscriptSearchArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/transcript/search",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "allChannels": args.all_channels,
            "query": args.query,
            "since": args.since,
            "preferRefined": !args.no_prefer_refined,
            "limit": args.limit,
        })),
        &args.output,
    )
}

fn jobs_list(args: JobsListArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/jobs",
        None,
        Some(json!({
            "guild": args.guild,
            "state": args.state,
            "ephemeral": args.ephemeral,
            "verbose": args.verbose,
        })),
        &args.output,
    )
}

fn jobs_get(args: JobGetArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        &format!("/v1/voice/jobs/{}", args.job_id),
        None,
        Some(json!({"ephemeral": args.ephemeral, "verbose": args.verbose})),
        &args.output,
    )
}

fn members_search(args: MemberSearchArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/members/search",
        None,
        Some(json!({
            "guild": agent_context_guild(args.guild),
            "query": args.query,
            "limit": args.limit,
        })),
        &args.output,
    )
}

fn members_resolve(args: MemberResolveArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/members/resolve",
        None,
        Some(json!({
            "guild": agent_context_guild(args.guild),
            "query": args.query,
        })),
        &args.output,
    )
}

fn members_get(args: MemberGetArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        &format!("/v1/voice/members/{}", args.user_id),
        None,
        Some(json!({"guild": agent_context_guild(args.guild)})),
        &args.output,
    )
}

fn agent_sessions_current(args: AgentSessionsCurrentArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/agent-sessions/current",
        None,
        Some(json!({"guild": args.guild, "channel": args.channel})),
        &args.output,
    )
}

fn agent_sessions_list(args: AgentSessionsListArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/agent-sessions",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "state": args.state,
            "limit": args.limit,
        })),
        &args.output,
    )
}

fn agent_sessions_search(args: AgentSessionsSearchArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        "/v1/voice/agent-sessions/search",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "state": args.state,
            "query": args.query,
            "since": args.since,
            "limit": args.limit,
        })),
        &args.output,
    )
}

fn agent_sessions_get(args: AgentSessionGetArgs) -> Result<i32> {
    api_emit_output(
        "GET",
        &format!("/v1/voice/agent-sessions/{}", args.agent_session_id),
        None,
        None,
        &args.output,
    )
}

fn agent_sessions_sunset(args: AgentSessionSunsetArgs) -> Result<i32> {
    api_emit(
        "POST",
        &format!("/v1/voice/agent-sessions/{}/sunset", args.agent_session_id),
        Some(json!({
            "requestedByUserId": agent_context_requested_by(args.requested_by_user_id),
            "reason": args.reason,
        })),
        None,
    )
}

fn agent_sessions_resume(args: AgentSessionResumeArgs) -> Result<i32> {
    let message = read_optional_payload(args.file, args.message)?;
    let route_kind = if args.dm_user.is_some() {
        "dm"
    } else {
        "voice"
    };
    api_emit(
        "POST",
        &format!("/v1/voice/agent-sessions/{}/resume", args.agent_session_id),
        Some(json!({
            "routeKind": route_kind,
            "guildId": args.guild.unwrap_or_default(),
            "voiceChannelId": args.channel.unwrap_or_default(),
            "dmUserId": args.dm_user.unwrap_or_default(),
            "requestedByUserId": agent_context_requested_by(args.requested_by_user_id),
            "message": message,
        })),
        None,
    )
}

fn response_submit(args: ResponseSubmitArgs, response_kind: &str) -> Result<i32> {
    let content = read_required_payload(args.file, "response body")?;
    api_emit(
        "POST",
        "/v1/voice/responses",
        Some(json!({
            "intent": response_kind,
            "source_job_id": agent_context_job(args.job),
            "target": args.sink,
            "guild_id": agent_context_guild(args.guild),
            "voice_channel_id": agent_context_channel(args.channel),
            "requested_by_user_id": agent_context_requested_by(args.requested_by_user_id),
            "content": content,
            "expects_reply": response_kind == "question",
        })),
        None,
    )
}

fn response_dm(args: ResponseDmArgs) -> Result<i32> {
    let content = read_required_payload(args.file, "DM body")?;
    let guild_id = agent_context_guild(args.guild.clone());
    let resolved = api_request(
        "GET",
        "/v1/voice/members/resolve",
        None,
        Some(json!({"guild": guild_id, "query": args.to})),
    )?;
    if resolved.get("resolved").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "DM recipient resolution is ambiguous or missing: {}",
            serde_json::to_string_pretty(&resolved)?
        );
    }
    let user_id = resolved
        .get("user")
        .and_then(|user| user.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if user_id.trim().is_empty() {
        anyhow::bail!("DM recipient resolution did not return a user id");
    }
    api_emit(
        "POST",
        "/v1/voice/responses",
        Some(json!({
            "intent": "message",
            "source_job_id": agent_context_job(args.job),
            "target": format!("dm:{user_id}"),
            "guild_id": guild_id,
            "voice_channel_id": agent_context_channel(args.channel),
            "requested_by_user_id": agent_context_requested_by(args.requested_by_user_id),
            "content": content,
            "expects_reply": false,
        })),
        None,
    )
}

fn feedback_submit(args: FeedbackSubmitArgs) -> Result<i32> {
    let content = read_required_payload(args.file, "feedback body")?;
    api_emit(
        "POST",
        "/v1/voice/feedback",
        Some(json!({
            "source_job_id": agent_context_job(args.job),
            "guild_id": agent_context_guild(args.guild),
            "voice_channel_id": agent_context_channel(args.channel),
            "requested_by_user_id": agent_context_requested_by(args.requested_by_user_id),
            "content": content,
        })),
        None,
    )
}

fn automation_spec(args: AutomationSpecArgs, path: &str) -> Result<i32> {
    let content = read_required_payload(args.file, "automation JSON")?;
    let spec = serde_json::from_str::<Value>(&content)?;
    api_emit("POST", path, Some(spec), None)
}

fn automations_list(args: AutomationListArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/automations",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "state": args.state,
        })),
    )
}

fn read_required_payload(file: Option<String>, label: &str) -> Result<String> {
    let content = if let Some(path) = file {
        fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("failed to read {label} file {path:?}: {error}"))?
    } else {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    };
    if content.trim().is_empty() {
        anyhow::bail!("{label} is empty; provide text on stdin or with --file");
    }
    Ok(content)
}

fn read_optional_payload(file: Option<String>, inline: Option<String>) -> Result<String> {
    if let Some(path) = file {
        return fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("failed to read message file {path:?}: {error}"));
    }
    Ok(inline.unwrap_or_default())
}

fn duration_to_seconds(raw: &str) -> i64 {
    let value = raw.trim().to_lowercase();
    if value.ends_with("ms") {
        return value[..value.len() - 2]
            .parse::<f64>()
            .map(|number| (number / 1000.0).max(0.0) as i64)
            .unwrap_or(0);
    }
    let (number, multiplier) = if let Some(stripped) = value.strip_suffix('s') {
        (stripped, 1.0)
    } else if let Some(stripped) = value.strip_suffix('m') {
        (stripped, 60.0)
    } else if let Some(stripped) = value.strip_suffix('h') {
        (stripped, 3600.0)
    } else if let Some(stripped) = value.strip_suffix('d') {
        (stripped, 86400.0)
    } else {
        (value.as_str(), 1.0)
    };
    number
        .parse::<f64>()
        .map(|number| (number * multiplier).max(0.0) as i64)
        .unwrap_or(0)
}

fn confirmation_approve(args: ConfirmationApproveArgs) -> Result<i32> {
    api_emit(
        "POST",
        &format!("/v1/voice/confirmations/{}/approve", args.job_id),
        Some(json!({"approvedByUserId": args.approved_by_user_id.unwrap_or_default()})),
        None,
    )
}

fn confirmation_cancel(args: ConfirmationCancelArgs) -> Result<i32> {
    api_emit(
        "POST",
        &format!("/v1/voice/confirmations/{}/cancel", args.job_id),
        Some(json!({"cancelledByUserId": args.cancelled_by_user_id.unwrap_or_default()})),
        None,
    )
}

fn pause(args: PauseArgs) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "pause_listening",
        None,
        room.clone(),
        args.requested_by_user_id,
        json!({"room": room, "duration_seconds": duration_to_seconds(&args.duration)}),
    )
}

fn resume(args: RoomArgs) -> Result<i32> {
    let room = args.channel.or(args.room);
    submit_command(
        "resume_listening",
        args.guild,
        room.clone(),
        args.requested_by_user_id,
        json!({"room": room}),
    )
}

fn forget(args: ForgetArgs) -> Result<i32> {
    let channel = args.channel.clone();
    api_emit(
        "POST",
        "/v1/voice/commands",
        Some(json!({
            "action": "dispatch_now",
            "command_kind": "forget_window",
            "guild_id": args.guild.unwrap_or_default(),
            "voice_channel_id": channel.unwrap_or_default(),
            "requested_by_user_id": args.requested_by_user_id.unwrap_or_default(),
            "arguments": {
                "window_id": args.window.unwrap_or_default(),
                "relative_start": args.since,
                "to": args.to.unwrap_or_default(),
                "unpublished_only": !args.published,
            },
            "requires_confirmation": true,
        })),
        None,
    )
}

fn submit_command(
    command_kind: &str,
    guild_id: Option<String>,
    channel_id: Option<String>,
    requested_by_user_id: Option<String>,
    arguments: Value,
) -> Result<i32> {
    api_emit(
        "POST",
        "/v1/voice/commands",
        Some(json!({
            "action": "dispatch_now",
            "command_kind": command_kind,
            "guild_id": guild_id.unwrap_or_default(),
            "voice_channel_id": channel_id.unwrap_or_default(),
            "requested_by_user_id": requested_by_user_id.unwrap_or_default(),
            "arguments": arguments,
        })),
        None,
    )
}

fn api_base_url() -> String {
    crate::config::api_base_url()
}

fn api_timeout_seconds() -> u64 {
    crate::config::api_timeout_seconds()
}

fn api_request(
    method: &str,
    path: &str,
    payload: Option<Value>,
    params: Option<Value>,
) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(api_timeout_seconds()))
        .build()?;
    let url = format!("{}{}", api_base_url(), path);
    let mut request = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        other => return Err(discord_tool_error(format!("unsupported method: {other}"))),
    };
    let query = query_pairs(params.as_ref());
    if !query.is_empty() {
        request = request.query(&query);
    }
    if let Some(payload) = payload {
        request = request.json(&payload);
    }
    let response = request.send()?;
    let status = response.status();
    let text = response.text()?;
    if !status.is_success() {
        let detail = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return Err(discord_tool_error(format!(
            "clankcord runtime API {method} {path} failed ({}): {}",
            status.as_u16(),
            detail.chars().take(500).collect::<String>()
        )));
    }
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str::<Value>(&text).map_err(Into::into)
}

fn api_emit(
    method: &str,
    path: &str,
    payload: Option<Value>,
    params: Option<Value>,
) -> Result<i32> {
    let result = api_request(method, path, payload, params)?;
    Ok(emit(result, true, None))
}

fn api_emit_output(
    method: &str,
    path: &str,
    payload: Option<Value>,
    params: Option<Value>,
    output: &OutputArgs,
) -> Result<i32> {
    let result = api_request(method, path, payload, params)?;
    emit_output(result, output)
}

fn emit_output(payload: Value, output: &OutputArgs) -> Result<i32> {
    ensure_json_format(&output.format)?;
    let rendered = serde_json::to_string_pretty(&payload)?;
    if let Some(path) = output
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        fs::write(path, format!("{rendered}\n"))?;
        println!("Wrote JSON to {path}");
        if let Some(count) = payload_record_count(&payload) {
            println!("Records: {count}");
        }
        if let Some((from, to)) = payload_window(&payload) {
            println!("Window: {from} to {to}");
        }
        return Ok(0);
    }
    println!("{rendered}");
    Ok(0)
}

fn ensure_json_format(format: &str) -> Result<()> {
    if format.trim().is_empty() || format.trim() == "json" {
        Ok(())
    } else {
        Err(discord_tool_error(
            "--format json is the only supported format",
        ))
    }
}

fn payload_record_count(payload: &Value) -> Option<usize> {
    if let Some(count) = payload.get("count").and_then(Value::as_u64) {
        return Some(count as usize);
    }
    for key in [
        "events",
        "hits",
        "jobs",
        "members",
        "candidates",
        "occupants",
        "agent_sessions",
    ] {
        if let Some(count) = payload.get(key).and_then(Value::as_array).map(Vec::len) {
            return Some(count);
        }
    }
    if let Some(channels) = payload.get("channels").and_then(Value::as_array) {
        return Some(
            channels
                .iter()
                .filter_map(|channel| channel.get("events").and_then(Value::as_array))
                .map(Vec::len)
                .sum(),
        );
    }
    None
}

fn payload_window(payload: &Value) -> Option<(String, String)> {
    let from = payload
        .get("from")
        .or_else(|| payload.get("since"))
        .or_else(|| payload.pointer("/window/start_time"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let to = payload
        .get("to")
        .or_else(|| payload.pointer("/window/end_time"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (!from.is_empty() || !to.is_empty()).then_some((from, to))
}

fn emit(payload: Value, json_output: bool, text_field: Option<&str>) -> i32 {
    if !json_output {
        if let Some(text_field) = text_field
            && let Some(text) = payload
                .get(text_field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        {
            println!("{text}");
            return 0;
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
    );
    0
}

fn agent_context_job(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_value("CLANKCORD_AGENT_JOB_ID"))
        .unwrap_or_default()
}

fn agent_context_guild(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_value("CLANKCORD_AGENT_GUILD_ID"))
        .unwrap_or_default()
}

fn agent_context_channel(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_value("CLANKCORD_AGENT_VOICE_CHANNEL_ID"))
        .unwrap_or_default()
}

fn agent_context_requested_by(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_value("CLANKCORD_AGENT_REQUESTED_BY_USER_ID"))
        .unwrap_or_default()
}

fn env_value(key: &str) -> Option<String> {
    crate::config::env_context_value(key)
}

fn query_pairs(payload: Option<&Value>) -> Vec<(String, String)> {
    let Some(map) = payload.and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, value)| {
            let rendered = match value {
                Value::Null => String::new(),
                Value::String(value) => value.trim().to_string(),
                Value::Bool(value) => value.to_string(),
                Value::Number(value) => value.to_string(),
                _ => value.to_string(),
            };
            if rendered.is_empty() {
                None
            } else {
                Some((key.clone(), rendered))
            }
        })
        .collect()
}

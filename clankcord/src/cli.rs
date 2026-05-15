use std::io::Read;
use std::time::Duration;

use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::messages::{read as read_messages, search as search_messages};
use crate::errors::discord_tool_error;
use crate::runtime::duration_to_seconds;

#[derive(Debug, Parser)]
#[command(name = "clankcord")]
#[command(about = "Agent CLI surface for Clankcord memory, jobs, and Discord operations.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Start,
    Status(StatusArgs),
    Rooms {
        #[command(subcommand)]
        command: Option<RoomsCommand>,
    },
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    Timeline {
        #[command(subcommand)]
        command: TimelineCommand,
    },
    Transcripts {
        #[command(subcommand)]
        command: TranscriptsCommand,
    },
    Conversations {
        #[command(subcommand)]
        command: ConversationsCommand,
    },
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    Participants {
        #[command(subcommand)]
        command: ParticipantsCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: Option<JobsCommand>,
    },
    Responses {
        #[command(subcommand)]
        command: ResponsesCommand,
    },
    Automations {
        #[command(subcommand)]
        command: AutomationsCommand,
    },
    Confirmations {
        #[command(subcommand)]
        command: ConfirmationsCommand,
    },
    Pause(PauseArgs),
    Resume(RoomArgs),
    Forget(ForgetArgs),
}

#[derive(Debug, Subcommand)]
enum RoomsCommand {
    Status(StatusArgs),
    Join(JoinArgs),
    Leave(RoomArgs),
    Move(MoveArgs),
    Mute(RoomArgs),
    Unmute(RoomArgs),
    PlayCue(PlayCueArgs),
}

#[derive(Debug, Subcommand)]
enum MessagesCommand {
    Read(read_messages::Args),
    Search(search_messages::Args),
}

#[derive(Debug, Subcommand)]
enum TimelineCommand {
    Tail(TimelineTailArgs),
    Range(TimelineRangeArgs),
}

#[derive(Debug, Subcommand)]
enum TranscriptsCommand {
    Materialize(TranscriptMaterializeArgs),
    Render(TranscriptRenderArgs),
    Search(TranscriptSearchArgs),
}

#[derive(Debug, Subcommand)]
enum ConversationsCommand {
    List(ConversationsListArgs),
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    Resolve(ContextResolveArgs),
}

#[derive(Debug, Subcommand)]
enum ParticipantsCommand {
    Trace(ParticipantTraceArgs),
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    List(JobsListArgs),
    Get(JobIdArg),
    Retry(JobIdArg),
    RunDue,
}

#[derive(Debug, Subcommand)]
enum ResponsesCommand {
    Submit(ResponseSubmitArgs),
    Ask(ResponseSubmitArgs),
}

#[derive(Debug, Subcommand)]
enum AutomationsCommand {
    #[command(about = "Print the automation JSON spec manual.")]
    Spec,
    Create(AutomationSpecArgs),
    Validate(AutomationSpecArgs),
    DryRun(AutomationSpecArgs),
    List(AutomationListArgs),
    Get(AutomationIdArg),
    Cancel(AutomationIdArg),
}

#[derive(Debug, Subcommand)]
enum ConfirmationsCommand {
    Approve(ConfirmationApproveArgs),
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
    #[arg(long, default_value = "markdown")]
    format: String,
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
}

#[derive(Debug, ClapArgs, Default)]
struct JobsListArgs {
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug, ClapArgs)]
struct JobIdArg {
    job_id: String,
}

#[derive(Debug, ClapArgs, Default)]
struct ResponseSubmitArgs {
    #[arg(long)]
    job: String,
    #[arg(long, default_value = "agent-chat")]
    sink: String,
    #[arg(long)]
    guild: Option<String>,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    requested_by_user_id: Option<String>,
    #[arg(long)]
    content: Option<String>,
    #[arg(long)]
    stdin: bool,
}

#[derive(Debug, ClapArgs, Default)]
struct AutomationSpecArgs {
    #[arg(long)]
    content: Option<String>,
    #[arg(long)]
    stdin: bool,
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
        Command::Jobs { command } => {
            match command.unwrap_or(JobsCommand::List(JobsListArgs::default())) {
                JobsCommand::List(args) => jobs_list(args),
                JobsCommand::Get(args) => api_emit(
                    "GET",
                    &format!("/v1/voice/jobs/{}", args.job_id),
                    None,
                    None,
                ),
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
    api_emit(
        "GET",
        "/v1/voice/timeline/tail",
        None,
        Some(json!({"guild": args.guild, "channel": args.channel, "since": args.since})),
    )
}

fn timeline_range(args: TimelineRangeArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/timeline/range",
        None,
        Some(json!({
            "guild": args.guild,
            "channel": args.channel,
            "from": args.from,
            "to": args.to,
            "allChannels": args.all_channels,
        })),
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
            "format": args.format,
        })),
    )?;
    Ok(emit(result, args.format != "markdown", Some("content")))
}

fn transcript_search(args: TranscriptSearchArgs) -> Result<i32> {
    api_emit(
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
    )
}

fn jobs_list(args: JobsListArgs) -> Result<i32> {
    api_emit(
        "GET",
        "/v1/voice/jobs",
        None,
        Some(json!({"guild": args.guild, "state": args.state, "verbose": args.verbose})),
    )
}

fn response_submit(args: ResponseSubmitArgs, response_kind: &str) -> Result<i32> {
    let content = if args.stdin {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    } else {
        args.content.unwrap_or_default()
    };
    api_emit(
        "POST",
        "/v1/voice/responses",
        Some(json!({
            "response_kind": response_kind,
            "source_job_id": args.job,
            "sink": args.sink,
            "guild_id": args.guild.unwrap_or_default(),
            "voice_channel_id": args.channel.unwrap_or_default(),
            "requested_by_user_id": args.requested_by_user_id.unwrap_or_default(),
            "content": content,
            "expects_reply": response_kind == "question",
        })),
        None,
    )
}

fn automation_spec(args: AutomationSpecArgs, path: &str) -> Result<i32> {
    let content = read_cli_payload(args.stdin, args.content)?;
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

fn read_cli_payload(stdin: bool, content: Option<String>) -> Result<String> {
    if stdin {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        return Ok(input);
    }
    let Some(content) = content else {
        anyhow::bail!("provide --stdin or --content");
    };
    Ok(content)
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
    std::env::var("CLANKCORD_API_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8091".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn api_timeout_seconds() -> u64 {
    std::env::var("CLANKCORD_API_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60)
        .max(5)
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

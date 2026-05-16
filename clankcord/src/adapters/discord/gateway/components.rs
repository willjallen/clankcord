use serenity::builder::EditInteractionResponse;
use serenity::client::Context;
use serenity::model::application::ComponentInteraction;

use crate::runtime::{RuntimeControlAction, RuntimeJobSink, log};

pub async fn handle_component_interaction(
    job_sink: RuntimeJobSink,
    ctx: Context,
    component: ComponentInteraction,
) {
    let custom_id = component.data.custom_id.trim().to_string();
    let action = if let Some(job_id) = custom_id.strip_prefix("clankcord_voice_confirm:") {
        ("approve", job_id.trim().to_string())
    } else if let Some(job_id) = custom_id.strip_prefix("clankcord_voice_cancel:") {
        ("cancel", job_id.trim().to_string())
    } else {
        return;
    };
    let actor_user_id = component.user.id.get().to_string();
    if let Err(error) = component.defer(&ctx.http).await {
        log(&format!("confirmation interaction defer failed: {error}"));
    }
    let control_action = if action.0 == "approve" {
        RuntimeControlAction::ApproveConfirmation
    } else {
        RuntimeControlAction::CancelConfirmation
    };
    let result = job_sink
        .submit_runtime_control_for_target(&action.1, control_action, actor_user_id)
        .await;
    let content = match result {
        Ok(_) if action.0 == "approve" => {
            format!("Clanky voice confirmation `{}` approval queued.", action.1)
        }
        Ok(_) => format!(
            "Clanky voice confirmation `{}` cancellation queued.",
            action.1
        ),
        Err(error) => format!(
            "Could not complete Clanky voice confirmation `{}`: {}",
            action.1, error
        ),
    };
    if let Err(error) = component
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new()
                .content(clipped_text(&content, 1900))
                .components(Vec::new()),
        )
        .await
    {
        log(&format!("confirmation interaction finish failed: {error}"));
    }
}

fn clipped_text(content: &str, limit: usize) -> String {
    let mut clipped = content.chars().take(limit).collect::<String>();
    if clipped.len() < content.len() {
        clipped.push_str("...");
    }
    clipped
}

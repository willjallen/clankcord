//! The automation spec JSON boundary: validate, create, list, and cancel
//! automations for the HTTP/CLI surfaces.

use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::model::automations::{AutomationRecord, AutomationSpec, AutomationState};

pub fn validate_automation_from_value(_ctx: &Ctx, value: &Value) -> Result<Value> {
    let spec = AutomationSpec::from_json(value)?;
    Ok(json!({"valid": true, "automation": spec.to_json()}))
}

pub async fn create_automation_from_value(ctx: &Ctx, value: &Value) -> Result<Value> {
    let spec = AutomationSpec::from_json(value)?;
    let record = ctx.store.create_automation(spec).await?;
    Ok(json!({"created": true, "automation": record.to_json()}))
}

pub async fn list_automation_records(
    ctx: &Ctx,
    guild_id: Option<&str>,
    scope_id: Option<&str>,
    state: Option<AutomationState>,
) -> Result<Value> {
    let records = ctx
        .store
        .list_automations(guild_id, scope_id, state)
        .await?;
    Ok(json!({
        "automations": records.iter().map(AutomationRecord::to_json).collect::<Vec<_>>(),
    }))
}

pub async fn get_automation_record(ctx: &Ctx, automation_id: &str) -> Result<Value> {
    Ok(ctx.store.get_automation(automation_id).await?.to_json())
}

pub async fn cancel_automation_record(ctx: &Ctx, automation_id: &str) -> Result<Value> {
    let record = ctx.store.cancel_automation(automation_id).await?;
    Ok(record.to_json())
}

//! Timeline search: the one term normalization and the one set of
//! search-SQL expressions over events and job rows, shared by the
//! dashboard views and the transcript surface.

use sqlx::{Postgres, QueryBuilder};

#[derive(Debug, Clone, Copy)]
pub(crate) enum SearchField {
    All,
    Detail,
    Feedback,
    Kind,
    JobKind,
    State,
    Command,
    Room,
    Actor,
}

pub(crate) fn push_event_member_joins(
    query: &mut QueryBuilder<'_, Postgres>,
    requester: bool,
    scope: bool,
) {
    if requester {
        query.push(
            r#" LEFT JOIN LATERAL (
              SELECT COALESCE(NULLIF(m.display_name, ''), NULLIF(m.global_name, ''), NULLIF(m.username, ''), '') AS label
              FROM discord_members m
              WHERE m.user_id = COALESCE(
                NULLIF(e.payload_json->>'requested_by_user_id', ''),
                NULLIF(e.payload_json->>'requestedByUserId', ''),
                NULLIF(e.speaker_user_id, '')
              )
                AND (e.guild_id = '' OR m.guild_id = e.guild_id)
              ORDER BY m.updated_at_ms DESC
              LIMIT 1
            ) requester_member ON TRUE"#,
        );
    }
    if scope {
        query.push(
            r#" LEFT JOIN LATERAL (
              SELECT COALESCE(NULLIF(m.display_name, ''), NULLIF(m.global_name, ''), NULLIF(m.username, ''), '') AS label
              FROM discord_members m
              WHERE e.scope_kind = 'dm' AND m.user_id = e.scope_id
              ORDER BY m.updated_at_ms DESC
              LIMIT 1
            ) scope_member ON TRUE"#,
        );
    }
}

pub(crate) fn push_job_member_joins(
    query: &mut QueryBuilder<'_, Postgres>,
    requester: bool,
    scope: bool,
) {
    if requester {
        query.push(
            r#" LEFT JOIN LATERAL (
              SELECT COALESCE(NULLIF(m.display_name, ''), NULLIF(m.global_name, ''), NULLIF(m.username, ''), '') AS label
              FROM discord_members m
              WHERE m.user_id = j.requested_by_user_id
                AND (j.guild_id = '' OR m.guild_id = j.guild_id)
              ORDER BY m.updated_at_ms DESC
              LIMIT 1
            ) requester_member ON TRUE"#,
        );
    }
    if scope {
        query.push(
            r#" LEFT JOIN LATERAL (
              SELECT COALESCE(NULLIF(m.display_name, ''), NULLIF(m.global_name, ''), NULLIF(m.username, ''), '') AS label
              FROM discord_members m
              WHERE j.scope_kind = 'dm' AND m.user_id = j.scope_id
              ORDER BY m.updated_at_ms DESC
              LIMIT 1
            ) scope_member ON TRUE"#,
        );
    }
}

pub(crate) fn search_terms(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|term| term.trim_start_matches('/').to_ascii_lowercase())
        .filter(|term| !term.is_empty())
        .collect()
}

pub(crate) fn push_event_search(
    query: &mut QueryBuilder<'_, Postgres>,
    raw: &str,
    field: SearchField,
) {
    let terms = search_terms(raw);
    if terms.is_empty() {
        return;
    }
    if matches!(field, SearchField::Feedback) {
        query.push(
            " AND (lower(e.event_kind) = 'feedback' OR lower(e.payload_json->>'kind') = 'feedback')",
        );
    }
    let expression = event_search_sql(field);
    for term in terms {
        query
            .push(" AND strpos(lower(")
            .push(expression)
            .push("), ")
            .push_bind(term)
            .push(") > 0");
    }
}

pub(crate) fn push_job_search(
    query: &mut QueryBuilder<'_, Postgres>,
    raw: &str,
    field: SearchField,
) {
    let terms = search_terms(raw);
    if terms.is_empty() {
        return;
    }
    if matches!(field, SearchField::Feedback) {
        query.push(" AND FALSE");
        return;
    }
    let expression = job_search_sql(field);
    for term in terms {
        query
            .push(" AND strpos(lower(")
            .push(expression)
            .push("), ")
            .push_bind(term)
            .push(") > 0");
    }
}

fn event_search_sql(field: SearchField) -> &'static str {
    match field {
        SearchField::All => {
            r#"concat_ws(' ', e.event_id, e.event_kind, e.text, e.scope_kind, e.guild_id,
                e.scope_id, e.speaker_user_id, e.speaker_label, e.payload_json->>'kind',
                e.payload_json->>'text', e.payload_json->>'feedback_message',
                e.payload_json->>'reason', e.payload_json->>'quality',
                e.payload_json->>'job_kind', e.payload_json->>'state',
                e.payload_json->>'command_kind', e.payload_json->>'command_name',
                r.guild_slug, r.voice_channel_name, r.voice_channel_slug,
                e.payload_json->>'guild_slug', e.payload_json->>'voice_channel_name',
                e.payload_json->>'voice_channel_slug', e.payload_json->>'speaker_label',
                e.payload_json->>'speaker_username', e.payload_json #>> '{result,kind}',
                requester_member.label, scope_member.label,
                e.payload_json #>> '{result,status}', e.payload_json #>> '{result,reason}',
                e.payload_json #>> '{result,action}', e.payload_json #>> '{result,message}',
                e.payload_json #>> '{result,summary}', e.payload_json #>> '{command_result,kind}',
                e.payload_json #>> '{command_result,status}', e.payload_json #>> '{command_result,reason}',
                e.payload_json #>> '{command_result,action}', e.payload_json #>> '{command_result,message}',
                e.payload_json #>> '{command_result,summary}', e.payload_json #>> '{command_response,kind}',
                e.payload_json #>> '{command_response,status}', e.payload_json #>> '{command_response,reason}',
                e.payload_json #>> '{command_response,action}', e.payload_json #>> '{command_response,message}',
                e.payload_json #>> '{command_response,summary}')"#
        }
        SearchField::Detail => {
            r#"concat_ws(' ', e.text, e.payload_json->>'text', e.payload_json->>'feedback_message',
                e.payload_json->>'reason', e.payload_json->>'quality',
                e.payload_json #>> '{result,kind}', e.payload_json #>> '{result,status}',
                e.payload_json #>> '{result,reason}', e.payload_json #>> '{result,action}',
                e.payload_json #>> '{result,message}', e.payload_json #>> '{result,summary}',
                e.payload_json #>> '{command_result,kind}', e.payload_json #>> '{command_result,status}',
                e.payload_json #>> '{command_result,reason}', e.payload_json #>> '{command_result,action}',
                e.payload_json #>> '{command_result,message}', e.payload_json #>> '{command_result,summary}',
                e.payload_json #>> '{command_response,kind}', e.payload_json #>> '{command_response,status}',
                e.payload_json #>> '{command_response,reason}', e.payload_json #>> '{command_response,action}',
                e.payload_json #>> '{command_response,message}', e.payload_json #>> '{command_response,summary}')"#
        }
        SearchField::Feedback => {
            "concat_ws(' ', e.event_kind, e.text, e.payload_json->>'kind', e.payload_json->>'feedback_message', e.payload_json->>'reason')"
        }
        SearchField::Kind => "concat_ws(' ', e.event_kind, e.payload_json->>'kind')",
        SearchField::JobKind => "concat_ws(' ', e.payload_json->>'job_kind')",
        SearchField::State => "concat_ws(' ', e.payload_json->>'state')",
        SearchField::Command => {
            "concat_ws(' ', e.payload_json->>'command_kind', e.payload_json->>'command_name')"
        }
        SearchField::Room => {
            "concat_ws(' ', e.scope_kind, e.guild_id, e.scope_id, r.guild_slug, r.voice_channel_name, r.voice_channel_slug, scope_member.label, e.payload_json->>'guild_slug', e.payload_json->>'voice_channel_name', e.payload_json->>'voice_channel_slug')"
        }
        SearchField::Actor => {
            "concat_ws(' ', e.speaker_user_id, e.speaker_label, requester_member.label, e.payload_json->>'speaker_label', e.payload_json->>'speaker_username')"
        }
    }
}

fn job_search_sql(field: SearchField) -> &'static str {
    match field {
        SearchField::All => {
            "concat_ws(' ', j.job_id, j.root_job_id, j.parent_job_id, j.kind, j.state, j.command_kind, j.scope_kind, j.guild_id, j.scope_id, j.requested_by_user_id, j.source_job_id, j.stream_id, j.target_job_id, j.speaker_user_id, r.guild_slug, r.voice_channel_name, r.voice_channel_slug, requester_member.label, scope_member.label)"
        }
        SearchField::Detail => {
            "concat_ws(' ', j.job_id, j.root_job_id, j.parent_job_id, j.source_job_id, j.stream_id, j.target_job_id)"
        }
        SearchField::Feedback => "''",
        SearchField::Kind => "'job'",
        SearchField::JobKind => "j.kind",
        SearchField::State => "j.state",
        SearchField::Command => "j.command_kind",
        SearchField::Room => {
            "concat_ws(' ', j.scope_kind, j.guild_id, j.scope_id, r.guild_slug, r.voice_channel_name, r.voice_channel_slug, scope_member.label)"
        }
        SearchField::Actor => {
            "concat_ws(' ', j.requested_by_user_id, j.speaker_user_id, requester_member.label)"
        }
    }
}

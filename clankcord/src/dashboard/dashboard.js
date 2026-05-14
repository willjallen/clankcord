const rootPrefix = location.pathname.startsWith('/__clawcord/') ? '/__clawcord' : '';
const state = {
  data: null,
  timer: null,
  selectedJobId: '',
};
const $ = (id) => document.getElementById(id);

const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (ch) => ({
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
}[ch]));

const short = (value, n = 16) => {
  const text = String(value ?? '');
  return text.length > n ? `${text.slice(0, n)}...` : text;
};

const ago = (iso) => {
  if (!iso) return '';
  const ms = Date.now() - Date.parse(iso);
  if (!Number.isFinite(ms)) return iso;
  const sec = Math.max(0, Math.floor(ms / 1000));
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 48) return `${hr}h ago`;
  return `${Math.floor(hr / 24)}d ago`;
};

const statusClass = (value) => {
  const text = String(value ?? '').toLowerCase();
  if (['queued', 'running', 'waiting', 'active', 'approved', 'capturing'].some((part) => text.includes(part))) return 'ok';
  if (['failed', 'error', 'timeout'].some((part) => text.includes(part))) return 'bad';
  if (['cancel', 'pending', 'released', 'absent', 'paused'].some((part) => text.includes(part))) return 'warn';
  return 'info';
};

const pill = (value) => `<span class="pill ${statusClass(value)}">${esc(value || 'none')}</span>`;
const code = (value, n = 18) => `<code title="${esc(value)}">${esc(short(value, n))}</code>`;
const text = (value) => esc(value || '');
const td = (value, cls = '') => `<td class="${cls}">${value}</td>`;
const metric = (label, value, cls = '') => `<div class="metric"><div class="label">${esc(label)}</div><div class="value ${cls}">${esc(value)}</div></div>`;

const table = (headers, rows, empty = 'No rows') => {
  if (!rows.length) return `<div class="empty">${esc(empty)}</div>`;
  return `<table><thead><tr>${headers.map((h) => `<th>${esc(h)}</th>`).join('')}</tr></thead><tbody>${rows.join('')}</tbody></table>`;
};

const denseRows = (rows) => {
  if (!rows.length) return '<div class="empty">No rows</div>';
  return rows.map(([label, value]) => `<div class="dense-row"><span>${esc(label)}</span><strong>${value}</strong></div>`).join('');
};

const jobCommand = (job) => job.payload?.command || {};
const jobArgs = (job) => jobCommand(job).arguments || {};
const jobMetadata = (job) => job.metadata || {};
const agentMetadata = (job) => jobMetadata(job).agent_task || {};
const confirmationMetadata = (job) => jobMetadata(job).confirmation || {};
const jobTime = (job) => job.updated_at || job.created_at || job.started_at || '';
const jobs = (data) => data.jobs || {};
const jobList = (data) => jobs(data).recent || [];
const activeJobs = (data) => jobs(data).active || [];
const timelineEvents = (data) => data.timeline?.recentEvents || [];

const eventKind = (event) => event.kind || event.event_kind || 'event';
const eventChannelId = (event) => event.channelId || event.voice_channel_id || event.voiceChannelId || '';
const eventChannelName = (event) => event.channelName || event.voice_channel_name || event.channelSlug || eventChannelId(event);
const eventSpeaker = (event) => event.speakerLabel || event.speaker_label || event.speakerId || event.speaker_user_id || '';
const eventWhen = (event) => event.startedAt || event.started_at || event.created_at || event.timestamp || '';
const eventId = (event) => event.job_id || event.eventId || event.event_id || '';
const eventDetail = (event) => {
  const result = event.router_result || event.router_response || event.result || {};
  return event.text || event.reason || result.reason || result.action || event.state || '';
};
const transcriptText = (event) => event.text || event.text_draft || event.transcript || '';

function channelOptions(data) {
  const channels = new Map();
  (data.status?.rooms || []).forEach((room) => {
    if (!room.channelId) return;
    channels.set(room.channelId, {
      id: room.channelId,
      label: room.channelName || room.channelSlug || room.channelId,
    });
  });
  timelineEvents(data).forEach((event) => {
    const id = eventChannelId(event);
    if (!id || channels.has(id)) return;
    channels.set(id, { id, label: eventChannelName(event) || id });
  });
  return [...channels.values()].sort((left, right) => left.label.localeCompare(right.label));
}

function setSelectOptions(select, options, allLabel) {
  const current = select.value;
  select.innerHTML = [`<option value="">${esc(allLabel)}</option>`]
    .concat(options.map((option) => `<option value="${esc(option.value)}">${esc(option.label)}</option>`))
    .join('');
  if (options.some((option) => option.value === current)) {
    select.value = current;
  }
}

function renderFilterOptions(data) {
  const kinds = new Set(timelineEvents(data).map(eventKind).filter(Boolean));
  setSelectOptions(
    $('timelineKind'),
    [...kinds].sort().map((kind) => ({ value: kind, label: kind })),
    'All',
  );
  const channels = channelOptions(data).map((channel) => ({ value: channel.id, label: channel.label }));
  setSelectOptions($('timelineChannel'), channels, 'All');
  setSelectOptions($('transcriptChannel'), channels, 'All');
}

function eventMatchesSearch(event, query) {
  if (!query) return true;
  const haystack = [
    eventKind(event),
    eventChannelName(event),
    eventChannelId(event),
    eventSpeaker(event),
    eventDetail(event),
    eventId(event),
  ].join(' ').toLowerCase();
  return haystack.includes(query.toLowerCase());
}

function filteredTimelineEvents(data) {
  const kind = $('timelineKind').value;
  const channel = $('timelineChannel').value;
  const query = $('timelineSearch').value.trim();
  return timelineEvents(data).filter((event) => {
    if (kind && eventKind(event) !== kind) return false;
    if (channel && eventChannelId(event) !== channel) return false;
    return eventMatchesSearch(event, query);
  });
}

function transcriptEvents(data) {
  const channel = $('transcriptChannel').value;
  const query = $('transcriptSearch').value.trim().toLowerCase();
  return timelineEvents(data)
    .filter((event) => {
      const kind = eventKind(event);
      const body = transcriptText(event);
      if (!body) return false;
      if (!['speech_segment', 'transcript'].includes(kind)) return false;
      if (channel && eventChannelId(event) !== channel) return false;
      if (!query) return true;
      return [
        body,
        eventSpeaker(event),
        eventChannelName(event),
        eventChannelId(event),
      ].join(' ').toLowerCase().includes(query);
    })
    .sort((left, right) => eventWhen(left).localeCompare(eventWhen(right)));
}

function jobDetailText(job) {
  const command = jobCommand(job);
  const args = jobArgs(job);
  const metadata = jobMetadata(job);
  const agent = agentMetadata(job);
  const confirmation = confirmationMetadata(job);
  return [
    metadata.error,
    agent.dispatch_error,
    confirmation.post_error,
    command.command_kind,
    args.request,
    args.question,
    args.target_room,
    agent.response_text,
    agent.dispatch_stdout_preview,
  ].filter(Boolean).join(' · ');
}

function jobRows(list, selectedId = '') {
  return list.map((job) => {
    const command = jobCommand(job);
    const args = jobArgs(job);
    const selected = job.job_id === selectedId ? ' selected' : '';
    return `<tr class="selectable${selected}" data-job-id="${esc(job.job_id)}">
      ${td(code(job.job_id))}
      ${td(pill(job.kind))}
      ${td(pill(job.state))}
      ${td(text(command.command_kind || args.action || ''))}
      ${td(code(job.parent_job_id || job.root_job_id || '', 14))}
      ${td(text(job.attempts ?? 0))}
      ${td(text(ago(jobTime(job))))}
      ${td(text(short(jobDetailText(job), 140)), 'text-cell')}
    </tr>`;
  });
}

function countRows(rows, keyName, labelName) {
  return rows.map((row) => `<tr>${td(text(row[labelName]))}${td(text(row.count))}</tr>`);
}

function bindJobRows(data) {
  document.querySelectorAll('[data-job-id]').forEach((row) => {
    row.addEventListener('click', () => {
      state.selectedJobId = row.getAttribute('data-job-id') || '';
      renderJobs(data);
    });
  });
}

function selectDefaultJob(data) {
  const all = activeJobs(data).concat(jobList(data));
  if (!state.selectedJobId || !all.some((job) => job.job_id === state.selectedJobId)) {
    state.selectedJobId = all[0]?.job_id || '';
  }
}

function render(data) {
  state.data = data;
  selectDefaultJob(data);
  const summary = jobs(data).summary || {};
  const status = data.status || {};
  const failed = Number(summary.failed || 0);
  $('subtitle').textContent = `Generated ${ago(data.generatedAt)} · uptime ${data.process?.uptimeSeconds ?? 0}s · ${summary.total || 0} jobs tracked`;
  $('metrics').innerHTML = [
    metric('Active Jobs', summary.active || 0, summary.active ? 'ok' : 'muted'),
    metric('Queued', summary.queued || 0, summary.queued ? 'warn' : 'muted'),
    metric('Running', summary.running || 0, summary.running ? 'ok' : 'muted'),
    metric('Waiting', summary.waiting || 0, summary.waiting ? 'warn' : 'muted'),
    metric('Failed', failed, failed ? 'bad' : 'muted'),
    metric('Rooms', (status.rooms || []).length),
  ].join('');

  renderOverview(data);
  renderJobs(data);
  renderRooms(data);
  renderRouter(data);
  renderFilterOptions(data);
  renderTimeline(data);
  renderTranscript(data);
  $('raw').textContent = JSON.stringify(data, null, 2);
  $('jsonLink').href = `${rootPrefix}/v1/voice/debug/overview?since=${encodeURIComponent($('since').value)}&limit=${encodeURIComponent($('limit').value)}`;
}

function renderOverview(data) {
  const summary = jobs(data).summary || {};
  const active = activeJobs(data);
  const pressureRows = active.slice(0, 16).map((job) => `<tr>
    ${td(code(job.job_id))}
    ${td(pill(job.kind))}
    ${td(pill(job.state))}
    ${td(text(jobCommand(job).command_kind || ''))}
    ${td(text(ago(jobTime(job))))}
    ${td(text(short(jobDetailText(job), 120)), 'text-cell')}
  </tr>`);
  $('queuePressureCount').textContent = `${active.length}`;
  $('queuePressure').innerHTML = table(['Job', 'Kind', 'State', 'Command', 'Updated', 'Detail'], pressureRows, 'No active jobs');

  const roomRows = (summary.byRoom || []).map((room) => `<tr>
    ${td(code(room.guild_id, 12))}
    ${td(code(room.voice_channel_id, 16))}
    ${td(text(room.total))}
    ${td(text(room.active))}
    ${td(text(room.failed))}
    ${td(text(ago(room.latest_at)))}
  </tr>`);
  $('roomJobLoadCount').textContent = `${roomRows.length}`;
  $('roomJobLoad').innerHTML = table(['Guild', 'Channel', 'Total', 'Active', 'Failed', 'Latest'], roomRows, 'No job history');

  $('stateMix').innerHTML = denseRows((summary.byState || []).map((row) => [row.state, text(row.count)]));
  const failures = jobList(data).filter((job) => statusClass(job.state) === 'bad' || jobMetadata(job).error || agentMetadata(job).dispatch_error);
  $('failureCount').textContent = `${failures.length}`;
  $('recentFailures').innerHTML = table(
    ['Job', 'Kind', 'State', 'Updated', 'Detail'],
    failures.slice(0, 12).map((job) => `<tr>${td(code(job.job_id))}${td(pill(job.kind))}${td(pill(job.state))}${td(text(ago(jobTime(job))))}${td(text(short(jobDetailText(job), 160)), 'text-cell')}</tr>`),
    'No recent failures',
  );
}

function renderJobs(data) {
  const active = activeJobs(data);
  const recent = jobList(data);
  $('activeJobsCount').textContent = `${active.length}`;
  $('recentJobsCount').textContent = `${recent.length}`;
  $('activeJobs').innerHTML = table(['Job', 'Kind', 'State', 'Command', 'Parent/Root', 'Attempts', 'Updated', 'Detail'], jobRows(active, state.selectedJobId), 'No active jobs');
  $('recentJobs').innerHTML = table(['Job', 'Kind', 'State', 'Command', 'Parent/Root', 'Attempts', 'Updated', 'Detail'], jobRows(recent, state.selectedJobId), 'No recent jobs');
  renderSelectedJob(active.concat(recent).find((job) => job.job_id === state.selectedJobId));
  bindJobRows(data);
}

function renderSelectedJob(job) {
  if (!job) {
    $('selectedJobTitle').textContent = '';
    $('selectedJob').innerHTML = '<div class="empty">No job selected.</div>';
    return;
  }
  const command = jobCommand(job);
  const args = jobArgs(job);
  const metadata = jobMetadata(job);
  $('selectedJobTitle').textContent = job.job_id || '';
  $('selectedJob').innerHTML = `<div class="detail">
    <div class="detail-head">${pill(job.kind)} ${pill(job.state)} ${command.command_kind ? pill(command.command_kind) : ''}</div>
    <div class="kv-grid">
      <div class="kv"><div class="k">Job</div><div class="v">${code(job.job_id, 42)}</div></div>
      <div class="kv"><div class="k">Root</div><div class="v">${code(job.root_job_id || '', 42)}</div></div>
      <div class="kv"><div class="k">Parent</div><div class="v">${code(job.parent_job_id || '', 42)}</div></div>
      <div class="kv"><div class="k">Lineage</div><div class="v">${esc(job.lineage_depth ?? 0)}</div></div>
      <div class="kv"><div class="k">Guild</div><div class="v">${code(job.guild_id || '', 42)}</div></div>
      <div class="kv"><div class="k">Channel</div><div class="v">${code(job.voice_channel_id || '', 42)}</div></div>
      <div class="kv"><div class="k">Requested By</div><div class="v">${code(job.requested_by_user_id || '', 42)}</div></div>
      <div class="kv"><div class="k">Attempts</div><div class="v">${esc(job.attempts ?? 0)}</div></div>
      <div class="kv"><div class="k">Created</div><div class="v">${esc(job.created_at || '')}</div></div>
      <div class="kv"><div class="k">Updated</div><div class="v">${esc(job.updated_at || '')}</div></div>
    </div>
    ${jobDetailText(job) ? `<div class="kv"><div class="k">Detail</div><div class="v">${esc(jobDetailText(job))}</div></div>` : ''}
    <details open><summary>Payload</summary><pre>${esc(JSON.stringify(job.payload || {}, null, 2))}</pre></details>
    ${Object.keys(metadata).length ? `<details><summary>Metadata</summary><pre>${esc(JSON.stringify(metadata, null, 2))}</pre></details>` : ''}
    ${Object.keys(args).length ? `<details><summary>Command Arguments</summary><pre>${esc(JSON.stringify(args, null, 2))}</pre></details>` : ''}
  </div>`;
}

function renderRooms(data) {
  const status = data.status || {};
  const rows = (status.rooms || []).map((room) => {
    const occ = room.occupancy || {};
    const control = room.control || {};
    return `<tr>
      ${td(text(room.channelName || room.channelSlug || room.channelId))}
      ${td(code(room.channelId))}
      ${td(room.activeSessionId ? pill('active') : pill('absent'))}
      ${td(text(room.autoJoin ? 'yes' : 'no'))}
      ${td(text(occ.effective_human_count ?? occ.effectiveHumanCount ?? ''))}
      ${td(pill(control.target_state || control.targetState || ''))}
      ${td(text(ago(occ.last_speech_at || occ.lastSpeechAt)))}
    </tr>`;
  });
  $('roomsCount').textContent = `${rows.length}`;
  $('rooms').innerHTML = table(['Room', 'Channel', 'Session', 'Auto', 'Humans', 'Control', 'Last Speech'], rows, 'No rooms');

  const sessionRows = (status.sessions || []).map((session) => `<tr>
    ${td(text(session.room?.channelName || session.roomId || session.voiceChannelId || ''))}
    ${td(code(session.captureRunId || session.sessionId || session.assignmentId))}
    ${td(pill(session.mode || 'active'))}
    ${td(text(session.botId || ''))}
    ${td(text(session.captureStats?.transcriptEvents ?? session.transcriptEventCount ?? 0))}
    ${td(text(ago(session.captureStats?.lastTranscriptAt || session.lastTranscriptAt)))}
  </tr>`);
  $('sessionsCount').textContent = `${sessionRows.length}`;
  $('sessions').innerHTML = table(['Room', 'Capture Run', 'Mode', 'Bot', 'Transcript Events', 'Last Transcript'], sessionRows, 'No active sessions');

  const botRows = (status.bots || []).map((bot) => {
    const debug = bot.voiceDebug || {};
    return `<tr>
      ${td(text(bot.botId || bot.voice_bot_id || ''))}
      ${td(pill(bot.state || (bot.ready ? 'ready' : 'starting')))}
      ${td(code(bot.assignedSessionId || ''))}
      ${td(code(debug.voiceClientChannelId || debug.voiceStateChannelId || ''))}
      ${td(text(debug.selfDeaf ? 'deaf' : 'listening'))}
    </tr>`;
  });
  $('botsCount').textContent = `${botRows.length}`;
  $('bots').innerHTML = table(['Bot', 'State', 'Assignment', 'Voice Channel', 'Audio'], botRows, 'No bots configured');

  const pool = status.pool || {};
  $('capacity').innerHTML = denseRows([
    ['Configured bots', text(pool.configuredBots ?? 0)],
    ['Active assignments', text(pool.activeAssignments ?? 0)],
    ['Available bots', text(pool.availableBots ?? 0)],
  ]);
}

function renderRouter(data) {
  const routerJobs = jobs(data).router || [];
  const rows = routerJobs.map((job) => {
    const command = jobCommand(job);
    const args = jobArgs(job);
    return `<tr>
      ${td(code(job.job_id))}
      ${td(pill(job.kind))}
      ${td(pill(job.state))}
      ${td(text(command.action || args.action || ''))}
      ${td(text(command.command_kind || ''))}
      ${td(code(command.target_job_id || args.previous_job_id || args.target_job_id || '', 14))}
      ${td(text(ago(jobTime(job))))}
      ${td(text(short(args.request || args.question || command.acknowledgement_text || jobDetailText(job), 140)), 'text-cell')}
    </tr>`;
  });
  $('routerJobsCount').textContent = `${rows.length}`;
  $('routerJobs').innerHTML = table(['Job', 'Kind', 'State', 'Action', 'Command', 'Target', 'Updated', 'Request/Result'], rows, 'No router-origin jobs');

  const routerEvents = timelineEvents(data).filter((event) => {
    const kind = String(eventKind(event));
    return kind.includes('router') || kind.includes('agent_task') || kind.includes('job_');
  });
  $('routerEventsCount').textContent = `${routerEvents.length}`;
  $('routerEvents').innerHTML = routerEvents.length ? routerEvents.map(renderTimelineCard).join('') : '<div class="empty">No router activity in this window.</div>';
}

function renderTimeline(data) {
  const timeline = data.timeline || {};
  const filtered = filteredTimelineEvents(data);
  const eventRows = filtered.map((event) => `<tr>
    ${td(text(ago(eventWhen(event))))}
    ${td(pill(eventKind(event)))}
    ${td(text(eventChannelName(event)))}
    ${td(text(eventSpeaker(event)))}
    ${td(text(short(eventDetail(event), 160)), 'text-cell')}
    ${td(code(eventId(event), 18))}
  </tr>`);
  $('eventsCount').textContent = `${eventRows.length}/${timelineEvents(data).length}`;
  $('events').innerHTML = table(['When', 'Kind', 'Room', 'Speaker', 'Text/Detail', 'Id'], eventRows, 'No timeline events');

  const kindRows = (timeline.eventKindCounts || []).map((row) => `<tr>
    ${td(text(row.eventKind))}
    ${td(text(row.count))}
    ${td(text(ago(row.latestAt)))}
  </tr>`);
  $('eventKinds').innerHTML = table(['Kind', 'Count', 'Latest'], kindRows, 'No event kinds');

  const pubRows = (data.publications || []).map((pub) => `<tr>
    ${td(code(pub.publication_id))}
    ${td(pill(pub.state))}
    ${td(code(pub.window_id))}
    ${td(code(pub.discord_thread_id || ''))}
    ${td(text(ago(pub.created_at)))}
  </tr>`);
  $('publicationsCount').textContent = `${pubRows.length}`;
  $('publications').innerHTML = table(['Publication', 'State', 'Window', 'Thread', 'Created'], pubRows, 'No publications');
}

function renderTranscript(data) {
  const events = transcriptEvents(data);
  $('transcriptCount').textContent = `${events.length}`;
  if (!events.length) {
    $('transcript').innerHTML = '<div class="empty">No transcript events match the current window and filters.</div>';
    return;
  }
  const groups = new Map();
  events.forEach((event) => {
    const channelId = eventChannelId(event) || 'unknown';
    const channelName = eventChannelName(event) || channelId;
    if (!groups.has(channelId)) {
      groups.set(channelId, { channelId, channelName, events: [] });
    }
    groups.get(channelId).events.push(event);
  });
  $('transcript').innerHTML = [...groups.values()].map((group) => [
    `<div class="transcript-channel">${esc(group.channelName)} <span class="muted">${esc(group.channelId)}</span></div>`,
    group.events.map(renderTranscriptLine).join(''),
  ].join('')).join('');
}

function renderTranscriptLine(event) {
  const when = eventWhen(event);
  const time = when && Number.isFinite(Date.parse(when))
    ? new Date(when).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
    : when;
  return `<div class="transcript-line">
    <div class="transcript-time">${esc(time)}</div>
    <div class="transcript-speaker">${esc(eventSpeaker(event) || 'unknown')}</div>
    <div class="transcript-text">${esc(transcriptText(event))}</div>
  </div>`;
}

function renderTimelineCard(event) {
  const result = event.router_result || event.router_response || event.result || {};
  return `<article class="timeline-card">
    <div class="timeline-head">
      <div class="detail-tags">
        ${pill(event.kind || event.event_kind)}
        ${result.action ? pill(result.action) : ''}
        ${event.state ? pill(event.state) : ''}
        <span class="muted">${esc(ago(eventWhen(event)))}</span>
      </div>
      ${code(eventId(event), 20)}
    </div>
    <div class="timeline-body">
      <div>${esc(eventDetail(event) || 'No detail recorded.')}</div>
      <details><summary>Event</summary><pre>${esc(JSON.stringify(event, null, 2))}</pre></details>
    </div>
  </article>`;
}

function showError(message, sticky = true) {
  const el = $('error');
  el.textContent = message;
  el.style.display = 'block';
  if (!sticky) setTimeout(() => {
    if (el.textContent === message) el.style.display = 'none';
  }, 5000);
}

async function refresh() {
  const url = `${rootPrefix}/v1/voice/debug/overview?since=${encodeURIComponent($('since').value)}&limit=${encodeURIComponent($('limit').value)}`;
  try {
    const response = await fetch(url, { cache: 'no-store' });
    if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
    $('error').style.display = 'none';
    render(await response.json());
  } catch (error) {
    showError(`Dashboard refresh failed: ${error.message}`);
  }
}

document.querySelectorAll('[data-view]').forEach((button) => {
  button.addEventListener('click', () => {
    document.querySelectorAll('[data-view]').forEach((node) => node.classList.toggle('active', node === button));
    document.querySelectorAll('.view').forEach((node) => node.classList.toggle('active', node.id === `view-${button.dataset.view}`));
  });
});

$('refresh').addEventListener('click', refresh);
$('since').addEventListener('change', refresh);
$('limit').addEventListener('change', refresh);
$('timelineKind').addEventListener('change', () => {
  if (state.data) renderTimeline(state.data);
});
$('timelineChannel').addEventListener('change', () => {
  if (state.data) renderTimeline(state.data);
});
$('timelineSearch').addEventListener('input', () => {
  if (state.data) renderTimeline(state.data);
});
$('transcriptChannel').addEventListener('change', () => {
  if (state.data) renderTranscript(state.data);
});
$('transcriptSearch').addEventListener('input', () => {
  if (state.data) renderTranscript(state.data);
});
$('auto').addEventListener('change', () => {
  clearInterval(state.timer);
  if ($('auto').checked) state.timer = setInterval(refresh, 3000);
});

state.timer = setInterval(refresh, 3000);
refresh();

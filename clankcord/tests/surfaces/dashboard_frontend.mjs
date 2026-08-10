import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const dashboardDirectory = path.resolve(testDirectory, '../../src/dashboard');
const storage = new Map();

globalThis.location = { pathname: '/dashboard' };
globalThis.localStorage = {
  getItem: (key) => storage.get(key) ?? null,
  setItem: (key, value) => storage.set(key, value),
};
globalThis.window = {
  ClankDashboardCharts: { render() {} },
  ClankDashboardJson: { render() {} },
  ClankDashboardTables: { render() {} },
};

for (const asset of ['dashboard-explorer.js', 'dashboard.js']) {
  const source = fs.readFileSync(path.join(dashboardDirectory, asset), 'utf8');
  vm.runInThisContext(source, { filename: asset });
}

function dashboard() {
  storage.clear();
  const app = window.dashboard();
  app.autoRefresh = false;
  app.timelineFilterChanged = () => {};
  return app;
}

function facets() {
  return {
    recordTypes: ['event', 'job'],
    categories: [
      { id: 'conversation', label: 'Conversation', count: 0, kinds: [] },
      { id: 'agent', label: 'Agent', count: 1, kinds: ['agent_task'] },
      { id: 'messaging_control', label: 'Messaging & Control', count: 1, kinds: ['feedback'] },
      { id: 'automation', label: 'Automation', count: 0, kinds: [] },
      { id: 'operations', label: 'Operations', count: 0, kinds: [] },
      { id: 'other', label: 'Other', count: 0, kinds: [] },
      { id: 'voice_detail', label: 'Voice Detail', count: 1, kinds: ['speech_segment'] },
      { id: 'background', label: 'Background', count: 2, kinds: ['ephemeral_job_gc', 'runtime_maintenance'] },
    ],
    defaultCategories: ['conversation', 'agent', 'messaging_control', 'automation', 'operations', 'other'],
    kinds: ['agent_task', 'ephemeral_job_gc', 'feedback', 'runtime_maintenance', 'speech_segment'],
    eventKinds: ['feedback'],
    jobKinds: ['agent_task'],
    states: ['complete', 'failed'],
    scopes: [{ id: '152012299465418345', kind: 'dm', guildId: '', label: 'Direct message with Will', count: 2 }],
  };
}

test('timeline multi-selects preserve All, None, and subset as distinct queries', () => {
  const app = dashboard();
  app.timelinePage.facets = facets();
  app.data = { timeline: { recentEvents: [{ event_id: 'overview-only-row' }] } };

  assert.deepEqual(app.timelinePageRecords(), []);

  assert.equal(app.timelineFilterSummary('timelineCategories'), 'Default · 6/8 categories');
  assert.equal(app.timelineFilterSummary('timelineKinds'), 'All · 6/8 categories');
  assert.equal(app.timelineQueryParams().has('categories'), false);
  assert.equal(app.timelineQueryParams().has('kinds'), false);

  app.selectNoTimelineFilterValues('timelineKinds');
  assert.deepEqual(app.filters.timelineKinds, []);
  assert.equal(app.timelineFilterSummary('timelineKinds'), 'None');
  assert.equal(app.timelineQueryParams().get('kinds'), 'none');

  app.filters.timelineKinds = ['feedback'];
  assert.equal(app.timelineQueryParams().get('kinds'), 'feedback');
  app.selectAllTimelineFilterValues('timelineKinds');
  assert.equal(app.filters.timelineKinds, null);
  assert.equal(app.timelineQueryParams().has('kinds'), false);

  app.selectAllTimelineFilterValues('timelineCategories');
  assert.deepEqual(app.filters.timelineCategories, facets().categories.map((category) => category.id));
  assert.equal(app.timelineQueryParams().get('categories'), 'all');
  app.selectNoTimelineFilterValues('timelineCategories');
  assert.deepEqual(app.filters.timelineCategories, []);
  assert.equal(app.timelineQueryParams().get('categories'), 'none');
});

test('kind groups visibly opt low-level categories in and out', () => {
  const app = dashboard();
  app.timelinePage.facets = facets();

  const groups = app.timelineKindGroups();
  assert.deepEqual(groups.slice(-2).map((group) => [group.label, group.defaultSelected]), [
    ['Voice Detail', false],
    ['Background', false],
  ]);
  assert.equal(app.timelineCategorySelected('background'), false);
  assert.equal(app.timelineKindSelected('background', 'runtime_maintenance'), false);

  app.toggleTimelineKindValue('background', 'runtime_maintenance');
  assert.equal(app.timelineCategorySelected('background'), true);
  assert.equal(app.timelineKindSelected('background', 'runtime_maintenance'), true);
  assert.equal(app.timelineKindSelected('background', 'ephemeral_job_gc'), false);
  assert.deepEqual(app.filters.timelineKinds, ['agent_task', 'feedback', 'runtime_maintenance']);

  app.selectNoTimelineKindGroup('background');
  assert.equal(app.filters.timelineCategories, null);
  assert.equal(app.timelineCategorySelected('background'), false);

  app.filters.timelineKinds = null;
  app.selectAllTimelineKindGroup('background');
  assert.equal(app.timelineCategorySelected('background'), true);
  assert.equal(app.timelineKindSelected('background', 'runtime_maintenance'), true);
  assert.equal(app.timelineKindSelected('background', 'ephemeral_job_gc'), true);
  assert.equal(app.timelineQueryParams().get('categories'), 'conversation,agent,messaging_control,automation,operations,other,background');

  app.selectNoTimelineKindGroup('background');
  assert.equal(app.filters.timelineCategories, null);
  assert.equal(app.timelineCategorySelected('background'), false);
  assert.equal(app.timelineQueryParams().has('categories'), false);
});

test('exact job drilldowns override default categories and quiet defaults explain an empty window', () => {
  const app = dashboard();
  app.timelinePage.facets = facets();
  app.timelinePage.loaded = true;
  app.timelinePage.matched = 0;
  app.data = {
    jobs: {
      active: [{ job_id: 'background-job', category: 'background' }],
      recent: [],
    },
  };

  assert.equal(app.timelineDefaultEmpty(), true);
  app.selectJob('background-job');
  assert.deepEqual(app.filters.timelineCategories, ['background']);
  assert.equal(app.filters.timelineWindow, 'all');
  assert.equal(app.timelineQueryParams().get('categories'), 'background');
  assert.equal(app.timelineDefaultEmpty(), false);

  app.filters.timelineCategories = ['background'];
  app.openFeedbackInTimeline({ category: 'messaging_control', event_id: 'feedback-1', feedback_message: 'Add a dashboard export' });
  assert.deepEqual(app.filters.timelineCategories, ['messaging_control']);
  assert.equal(app.timelineQueryParams().get('categories'), 'messaging_control');

  const uncategorized = dashboard();
  uncategorized.timelinePage.facets = facets();
  uncategorized.data = { jobs: { active: [{ job_id: 'exact-job' }], recent: [] } };
  uncategorized.selectJob('exact-job');
  assert.deepEqual(uncategorized.filters.timelineCategories, facets().categories.map((category) => category.id));
  assert.equal(uncategorized.timelineQueryParams().get('categories'), 'all');
});

test('timeline facets remain typed and All sends an unbounded time window', () => {
  const app = dashboard();
  app.timelinePage.facets = facets();
  app.filters.timelineWindow = 'all';
  app.filters.timelineStart = '';

  assert.deepEqual(app.timelineFilterOptionRows('timelineKinds').map((option) => option.id), ['agent_task', 'ephemeral_job_gc', 'feedback', 'runtime_maintenance', 'speech_segment']);
  assert.deepEqual(app.timelineFilterOptionRows('timelineJobStates').map((option) => option.id), ['complete', 'failed']);
  assert.equal(app.timelineFilterDisplay('timelineChannels', '152012299465418345'), 'Direct message with Will');
  assert.equal(app.timelineQueryParams().get('from'), 'all');
  assert.equal(app.timelineQueryParams('cursor-v1').get('metadata'), 'none');

  app.filters.feedbackSince = 'all';
  const feedbackParams = new URL(app.feedbackUrl(), 'http://dashboard.local').searchParams;
  assert.equal(feedbackParams.get('from'), 'all');
  assert.equal(feedbackParams.get('metadata'), 'count');
});

test('metadata-free cursor pages preserve first-page counts and facets', () => {
  const app = dashboard();
  const firstFacets = facets();
  app.applyTimelinePage({
    snapshotAt: '2026-08-08T12:00:00Z',
    matched: 2,
    returned: 1,
    hasMore: true,
    nextCursor: 'cursor-v1',
    records: [{ recordType: 'event', id: 'first', event: { event_id: 'first' } }],
    facets: firstFacets,
  });
  app.applyTimelinePage({
    snapshotAt: '2026-08-08T12:00:00Z',
    returned: 1,
    hasMore: false,
    nextCursor: null,
    records: [{ recordType: 'event', id: 'second', event: { event_id: 'second' } }],
  }, true);

  assert.equal(app.timelinePage.matched, 2);
  assert.deepEqual(app.timelinePage.facets, firstFacets);
  assert.deepEqual(app.timelinePage.records.map((record) => record.id), ['first', 'second']);
});

test('transcripts render oldest first and timeline rows use canonical event ids', () => {
  const app = dashboard();
  app.data = {
    transcript: {
      events: [
        { event_id: 'older', job_id: 'shared-job', startedAt: '2026-08-08T10:00:00Z', text: 'older' },
        { event_id: 'newer', job_id: 'shared-job', startedAt: '2026-08-08T11:00:00Z', text: 'newer' },
      ],
    },
  };

  assert.deepEqual(app.filteredTranscriptEvents().map((event) => event.event_id), ['older', 'newer']);
  assert.equal(app.eventId(app.transcriptEvents[0]), 'older');
  assert.notEqual(app.eventId(app.transcriptEvents[0]), app.transcriptEvents[0].job_id);
});

test('primary scope labels use resolved names instead of numeric ids', () => {
  const app = dashboard();
  const id = '152012299465418345';

  assert.equal(app.eventScopeLabel({ scope_kind: 'dm', scope_id: id, scopeLabel: 'Direct message with Will' }), 'Direct message with Will');
  assert.equal(app.eventSpeaker({ requestedByLabel: 'Will', speaker_user_id: id }), 'Will');
  assert.equal(app.jobScopeLabel({ scope_kind: 'voice_channel', scope_id: id, scopeLabel: 'Code Lounge' }), 'Code Lounge');
  assert.equal(app.agentSessionScopeLabel({ key: `voice:guild:${id}`, scopeLabel: 'Code Lounge' }), 'Code Lounge');
  assert.equal(app.eventScopeLabel({ scope_kind: 'dm', scope_id: id }), 'Direct message');
  assert.equal(app.jobScopeLabel({ scope_kind: 'voice_channel', scope_id: id }), 'Unresolved voice channel');
  app.data = {
    transcript: {
      events: [{ event_id: 'dm-transcript', scope_kind: 'dm', scope_id: id, scopeLabel: 'Direct message with Will', startedAt: '2026-08-08T11:00:00Z', text: 'hello' }],
    },
  };
  assert.equal(app.transcriptGroups()[0].channelName, 'Direct message with Will');
  app.data = {
    charts: {
      scopeActivity: [{ scopeKind: 'voice_channel', scopeId: id, guildId: 'guild', scopeLabel: 'Code Lounge', jobs: 1, speech: 0, transcripts: 0, wake: 0, total: 1 }],
    },
  };
  assert.equal(app.roomActivityRows()[0].label, 'Code Lounge');
});

test('failure metrics are explicitly windowed and expose actionable rows', () => {
  const app = dashboard();
  app.data = {
    generatedAt: '2026-08-08T12:00:00Z',
    health: {
      status: 'degraded',
      observedAt: '2026-08-08T12:00:00Z',
      components: [{ component: 'scheduler', status: 'degraded', reason: 'due backlog 42s' }],
      inventory: { configuredRooms: 2 },
      failures: {
        window: '1h',
        count: 1,
        complete: true,
        recent: [{ jobId: 'job-failed', category: 'agent', kind: 'agent_task', state: 'failed', reason: 'provider timed out' }],
      },
    },
    jobs: { summary: { active: 3, running: 1 } },
    operations: {
      backlog: { dueQueued: 2 },
      failures: {
        window: '1h',
        count: 1,
        complete: true,
        recent: [{ jobId: 'job-failed', category: 'agent', kind: 'agent_task', state: 'failed', reason: 'provider timed out' }],
      },
    },
  };

  const metrics = app.metrics();
  assert.deepEqual(metrics.map((metric) => metric.label), ['Health', 'Active Jobs', 'Due Queued', 'Running', 'Failures (1h)', 'Rooms']);
  assert.equal(metrics.find((metric) => metric.label === 'Failures (1h)').value, 1);
  assert.equal(app.recentFailures()[0].reason, 'provider timed out');
  assert.deepEqual(app.healthRows()[1], {
    label: 'scheduler',
    status: 'degraded',
    reason: 'due backlog 42s',
    className: 'bad',
  });
  assert.equal('value' in app.healthRows()[1], false);
  app.traceFailure(app.recentFailures()[0]);
  assert.deepEqual(app.filters.timelineCategories, ['agent']);
  assert.equal(app.filters.timelineWindow, 'all');
  assert.equal(app.timelineQueryParams().get('categories'), 'agent');

  app.data.health.failures.complete = false;
  assert.equal(app.metrics().find((metric) => metric.label === 'Failures (1h, partial)').value, '≥1');
});

test('overview charts use exact aggregate arrays instead of displayed record samples', () => {
  const app = dashboard();
  app.data = {
    jobs: {
      active: [{ job_id: 'sample-only', kind: 'sample_kind', state: 'running', scope_id: 'sample' }],
      recent: [],
    },
    timeline: { recentEvents: [{ event_id: 'sample-event', event_kind: 'sample_event', startedAt: '2026-08-08T10:02:00Z' }] },
    charts: {
      window: { from: '2026-08-08T10:00:00Z', to: '2026-08-08T10:10:00Z', eventBucketSeconds: 300 },
      jobsByKindState: [
        { kind: 'agent_task', state: 'complete', count: 9 },
        { kind: 'agent_task', state: 'failed', count: 2 },
        { kind: 'text_delivery', state: 'complete', count: 4 },
      ],
      eventsByBucketKind: [
        { bucketAt: '2026-08-08T10:00:00Z', kind: 'feedback', count: 4 },
        { bucketAt: '2026-08-08T10:10:00Z', kind: 'transcript', count: 2 },
      ],
      scopeActivity: [
        { scopeKind: 'dm', guildId: '', scopeId: 'user', scopeLabel: 'Direct message with Will', jobs: 7, speech: 0, transcripts: 2, wake: 0, total: 9, latestAt: '2026-08-08T10:10:00Z' },
      ],
    },
  };

  assert.deepEqual(app.jobMixRows(), [
    { kind: 'agent_task', total: 11, states: { complete: 9, failed: 2 } },
    { kind: 'text_delivery', total: 4, states: { complete: 4 } },
  ]);
  const trend = app.eventTrendRows();
  assert.equal(trend.labels.length, 3);
  assert.deepEqual(trend.series, [
    { kind: 'feedback', values: [4, 0, 0] },
    { kind: 'transcript', values: [0, 0, 2] },
  ]);
  assert.deepEqual(app.roomActivityRows()[0], {
    channelId: 'user',
    scopeKind: 'dm',
    guildId: '',
    label: 'Direct message with Will',
    jobs: 7,
    speech: 0,
    transcripts: 2,
    wake: 0,
    total: 9,
    latestAt: '2026-08-08T10:10:00Z',
  });
});

test('overview chart drilldowns explicitly include low-level categories', () => {
  const clickHandlers = new Map();
  const applied = [];
  const chartContext = {
    document: { getElementById: (id) => ({ id }) },
    window: {
      addEventListener() {},
      echarts: {
        init(element) {
          return {
            off() {},
            on(_event, handler) { clickHandlers.set(element.id, handler); },
            setOption() {},
            resize() {},
          };
        },
      },
    },
  };
  const chartSource = fs.readFileSync(path.join(dashboardDirectory, 'dashboard-charts.js'), 'utf8');
  vm.runInNewContext(chartSource, chartContext, { filename: 'dashboard-charts.js' });
  const app = {
    applyTimelineFilter: (filter) => applied.push(JSON.parse(JSON.stringify(filter))),
    eventTrendRows: () => ({ labels: ['12:00'], series: [{ kind: 'speech_segment', values: [1] }] }),
    filteredLatencyKindRows: () => [{ kind: 'runtime_maintenance' }],
    jobMixRows: () => [{ kind: 'ephemeral_job_gc', states: { failed: 1 } }],
    jobMixStates: () => ['failed'],
    latencyNumber: () => 10,
    millis: (value) => `${value}ms`,
    roomActivityRows: () => [{ channelId: 'room-1', label: 'Code Lounge', jobs: 0, speech: 1, transcripts: 0, wake: 0 }],
    timelineDrilldownCategories: () => facets().categories.map((category) => category.id),
  };

  chartContext.window.ClankDashboardCharts.render(app);
  clickHandlers.get('job-mix-chart')({ dataIndex: 0, seriesName: 'failed' });
  clickHandlers.get('latency-kind-chart')({ dataIndex: 0 });
  clickHandlers.get('event-trend-chart')({ seriesName: 'speech_segment' });
  clickHandlers.get('room-activity-chart')({ dataIndex: 0 });

  assert.equal(applied.length, 4);
  for (const filter of applied) {
    assert.deepEqual(filter.timelineCategories, facets().categories.map((category) => category.id));
  }
});

test('agent detail derives an operator-readable run narrative from the detail contract', () => {
  const app = dashboard();
  const detail = {
    job: {
      job_id: 'job-child',
      kind: 'agent_task',
      state: 'failed',
      scope_kind: 'dm',
      scope_id: 'will',
      scopeLabel: 'Direct message with Will',
      requestedByLabel: 'Will',
      request: 'Investigate the failed delivery and report the cause.',
      durationMs: 5000,
      attempts: 2,
      created_at: '2026-08-08T10:00:00Z',
      started_at: '2026-08-08T10:00:01Z',
      completed_at: '2026-08-08T10:00:05Z',
      updated_at: '2026-08-08T10:00:05Z',
      root_job_id: 'job-root',
      parent_job_id: 'job-parent',
      lineage_depth: 2,
      payload: { command: { arguments: { request: 'This nested request must not be rendered.' } } },
      metadata: {
        error: 'delivery failed',
        agent_task: {
          dispatch_attempts: 3,
          dispatch_error: 'provider exited 1',
          dispatch_stderr: 'rate limit nearing threshold',
          agent: { model: 'gpt-5.6', reasoning_effort: 'high' },
          preflight: {
            checks: [
              { command: 'git status', ok: true },
              { command: 'gh auth status', ok: false, error: 'not authenticated', stderr_preview: '' },
            ],
          },
        },
      },
    },
    result: { exists: true, bytes: 27, truncated: false, content: 'The delivery token expired.' },
    prompt: { exists: true, content: 'raw prompt' },
    raw: { exists: true, content: '{"type":"session_meta"}' },
    workdir: { path: '/tmp/work', files: ['report.md'] },
    codex: {
      model: 'gpt-5.6',
      reasoningEffort: 'high',
      sessionId: 'session-1',
      modelContextWindow: 100000,
      contextUsedTokens: 12000,
      tokenUsage: { total_token_usage: { input_tokens: 12000, cached_input_tokens: 8000, output_tokens: 900, reasoning_output_tokens: 300 } },
      toolCalls: [{ kind: 'tool_call', name: 'command_execution' }],
      timeline: [
        { kind: 'message', role: 'assistant', text: 'Found the cause.', timestamp: '2026-08-08T10:00:03Z' },
        { kind: 'tool_call', name: 'command_execution', arguments: 'inspect logs', output: 'expired', status: 'completed', timestamp: '2026-08-08T10:00:02Z' },
      ],
    },
    session: {
      totalJobCount: 2,
      truncated: false,
      jobs: [
        { job_id: 'job-root', state: 'complete', request: 'Start investigation', durationMs: 60000, created_at: '2026-08-08T09:00:00Z', updated_at: '2026-08-08T09:01:00Z' },
        { job_id: 'job-child', state: 'failed', request: 'Investigate the failed delivery and report the cause.', durationMs: 5000, created_at: '2026-08-08T10:00:00Z', updated_at: '2026-08-08T10:00:05Z' },
      ],
    },
  };
  app.selectedAgentJobId = 'job-child';
  app.agentDetails = { 'job-child': detail };
  app.data = { agents: { summary: { total: 12, active: 1, completed: 9, failed: 2 }, jobs: [], sessions: [] } };

  assert.equal(app.selectedAgentRequest(), 'Investigate the failed delivery and report the cause.');
  assert.equal(app.selectedAgentFinalResult(), 'The delivery token expired.');
  assert.equal(app.selectedAgentRunFacts().find((row) => row.label === 'Duration').value, '5.00s');
  assert.equal(app.selectedAgentRunFacts().find((row) => row.label === 'Scope').value, 'Direct message with Will');
  assert.deepEqual(app.selectedAgentPhases().map((phase) => phase.label), ['Queued', 'Execute', 'Outcome']);
  assert.deepEqual(app.selectedAgentExecutionTimeline().map((event) => event.title), [
    'Run queued',
    'Execution started',
    'Command execution',
    'Assistant message',
    'Run failed',
  ]);
  assert.deepEqual(app.selectedAgentErrorRows().map((row) => row.source), ['Run', 'Dispatch', 'Preflight', 'Codex stderr']);
  assert.deepEqual(app.selectedAgentAttemptRows().map((row) => row.value), ['2', '3', '1 / 2 passed', '1']);
  assert.equal(app.selectedAgentUsageRows().find((row) => row.label === 'Input tokens').value, '12,000');
  assert.deepEqual(app.selectedAgentLineage().map((row) => row.role), ['Root', 'Parent', 'Selected']);
  assert.deepEqual(app.selectedAgentRelatedRuns().map((run) => run.job_id), ['job-child', 'job-root']);
  assert.deepEqual(app.agentSummaryRows().map((row) => row.value), ['12', '1', '9', '2']);
  assert.deepEqual(app.agentSummaryRows().map((row) => row.label), ['Runs · 24h', 'Active · now', 'Completed · 24h', 'Failed · 24h']);

  const listOnly = dashboard();
  listOnly.selectedAgentJobId = 'job-child';
  listOnly.data = { agents: { jobs: [{ job: detail.job, codex: detail.codex, detailUrl: '/v1/dashboard/agents/job-child' }] } };
  assert.equal(listOnly.selectedAgentEntry(), null);
});

test('a stale load-more response cannot append across a timeline refresh', async () => {
  const app = dashboard();
  app.applyTimelinePage({
    snapshotAt: '2026-08-08T11:00:00Z',
    matched: 2,
    returned: 1,
    hasMore: true,
    nextCursor: 'old-cursor',
    records: [{ recordType: 'event', id: 'old-top', event: { event_id: 'old-top' } }],
    facets: facets(),
  });
  let resolveFetch;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = () => new Promise((resolve) => { resolveFetch = resolve; });

  const pending = app.loadOlderTimeline();
  app.applyTimelinePage({
    snapshotAt: '2026-08-08T12:00:00Z',
    matched: 1,
    returned: 1,
    hasMore: false,
    nextCursor: null,
    records: [{ recordType: 'event', id: 'fresh', event: { event_id: 'fresh' } }],
    facets: facets(),
  });
  resolveFetch({ ok: true, json: async () => ({
    snapshotAt: '2026-08-08T11:00:00Z',
    matched: 2,
    returned: 1,
    hasMore: false,
    nextCursor: null,
    records: [{ recordType: 'event', id: 'old-page', event: { event_id: 'old-page' } }],
    facets: facets(),
  }) });
  await pending;
  globalThis.fetch = originalFetch;

  assert.deepEqual(app.timelinePage.records.map((record) => record.id), ['fresh']);
});

test('dashboard URLs use view-scoped dashboard routes', () => {
  const app = dashboard();
  for (const view of ['overview', 'timeline', 'agents', 'automations', 'feedback', 'health', 'rooms', 'control', 'transcript', 'raw']) {
    app.activeView = view;
    assert.match(app.activeViewUrl(), /^\/v1\/dashboard\//);
    assert.doesNotMatch(app.activeViewUrl(), /debug/);
  }
});

test('dashboard markup exposes None, Feedback, and dashboard-scoped assets', () => {
  const html = fs.readFileSync(path.join(dashboardDirectory, 'index.html'), 'utf8');
  assert.match(html, /selectNoTimelineFilterValues/);
  assert.match(html, /Feedback Requests/);
  assert.match(html, /\.\/dashboard\/dashboard\.js/);
  assert.doesNotMatch(html, /\.\/debug\//);
  assert.match(html, />Request</);
  assert.match(html, />Result</);
  assert.match(html, />Execution</);
  assert.match(html, />Related Work</);
  assert.match(html, />Timing</);
  assert.match(html, />Errors</);
  assert.match(html, /<details class="advanced-details">\s*<summary>Advanced<\/summary>/);
  assert.doesNotMatch(html, /Operator run|Session History Text|Session Trace/);
  assert.match(html, /oldest first/);
  assert.match(html, /health-component-value/);
  assert.match(html, /No high-signal activity in this window\. Enable Voice Detail or Background in Kind to inspect low-level records\./);
});

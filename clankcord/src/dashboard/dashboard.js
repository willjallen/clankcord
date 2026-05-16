const rootPrefix = location.pathname.startsWith('/__clankcord/') ? '/__clankcord' : '';
const viewStorageKey = 'clankcord.dashboard.view';
const filterStorageKey = 'clankcord.dashboard.filters';

const defaultFilters = {
  jobsLimit: 120,
  agentLimit: 120,
  timelineSince: '-1h',
  timelineLimit: 120,
  timelineKind: '',
  timelineChannel: '',
  timelineSearch: '',
  transcriptSince: '-24h',
  transcriptLimit: 250,
  transcriptChannel: '',
  transcriptSearch: '',
  publicationLimit: 120,
  ...window.ClankDashboardExplorer.defaultFilters,
};

function storedJson(key, fallback) {
  try {
    const value = localStorage.getItem(key);
    return value ? JSON.parse(value) : fallback;
  } catch {
    return fallback;
  }
}

function storeJson(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {}
}

function textValue(value) {
  return value === undefined || value === null ? '' : String(value);
}

function firstText(values) {
  return values.map(textValue).find((value) => value.trim() !== '') || '';
}

window.dashboard = function dashboard() {
  return {
    tabs: [
      { id: 'explore', label: 'Explore' },
      { id: 'overview', label: 'Overview' },
      { id: 'jobs', label: 'Jobs' },
      { id: 'agents', label: 'Agent Jobs' },
      { id: 'automations', label: 'Automations' },
      { id: 'health', label: 'Health' },
      { id: 'rooms', label: 'Rooms' },
      { id: 'control', label: 'Control' },
      { id: 'timeline', label: 'Timeline' },
      { id: 'transcript', label: 'Transcript' },
      { id: 'raw', label: 'Raw' },
    ],
    data: null,
    loading: false,
    error: '',
    activeView: localStorage.getItem(viewStorageKey) || 'explore',
    filters: { ...defaultFilters, ...storedJson(filterStorageKey, {}) },
    selectedJobId: '',
    selectedAgentJobId: '',
    selectedAutomationId: '',
    ...window.ClankDashboardExplorer.initialState(),
    agentDetails: {},
    agentDetailLoadingId: '',
    agentDetailErrors: {},
    autoRefresh: true,
    timer: null,
    lastSubwindowScrollAt: 0,
    control: {
      roomId: '',
      requestedByUserId: 'dashboard',
      cue: 'ack',
      agentTask: '',
      result: null,
      lastKind: '',
      lastAt: '',
    },

    init() {
      if (!this.tabs.some((tab) => tab.id === this.activeView)) {
        this.activeView = 'explore';
      }
      document.addEventListener('scroll', (event) => {
        if (event.target?.closest?.('.scroll-region')) {
          this.lastSubwindowScrollAt = Date.now();
        }
      }, true);
      this.syncAutoRefresh();
      this.refresh({ force: true });
    },

    syncAutoRefresh() {
      if (this.timer) clearInterval(this.timer);
      this.timer = null;
      if (this.autoRefresh) {
        this.timer = setInterval(() => this.refresh({ auto: true }), 3000);
      }
    },

    activateView(view) {
      if (!this.tabs.some((tab) => tab.id === view)) return;
      this.activeView = view;
      try {
        localStorage.setItem(viewStorageKey, view);
      } catch {}
      if (view === 'agents') {
        this.loadSelectedAgentDetail();
      }
      this.scheduleRenderInteractive();
    },

    filterChanged() {
      storeJson(filterStorageKey, this.filters);
      this.refresh({ force: true });
      this.scheduleRenderInteractive();
    },

    timelineFilterChanged() {
      storeJson(filterStorageKey, this.filters);
      this.scheduleRenderInteractive();
    },

    clearTimelineFilters() {
      Object.assign(this.filters, {
        timelineKind: '',
        timelineChannel: '',
        timelineSearch: '',
      });
      this.timelineFilterChanged();
    },

    ...window.ClankDashboardExplorer.methods,

    jsonUrl() {
      return `${rootPrefix}/v1/voice/debug/overview?${this.queryParams().toString()}`;
    },

    queryParams() {
      return new URLSearchParams({
        jobsLimit: String(this.filters.jobsLimit),
        agentLimit: String(this.filters.agentLimit),
        timelineSince: this.filters.timelineSince,
        timelineLimit: String(this.filters.timelineLimit),
        transcriptSince: this.filters.transcriptSince,
        transcriptLimit: String(this.filters.transcriptLimit),
        publicationLimit: String(this.filters.publicationLimit),
      });
    },

    shouldDeferAutoRefresh() {
      if (Date.now() - this.lastSubwindowScrollAt < 1800) return true;
      if (document.querySelector('.scroll-region:hover')) return true;
      const active = document.activeElement;
      return Boolean(active?.closest?.('.scroll-region'));
    },

    async refresh(options = {}) {
      if (options.auto && this.shouldDeferAutoRefresh()) return;
      const scrollState = this.captureScrollState();
      this.loading = true;
      try {
        const response = await fetch(this.jsonUrl(), { cache: 'no-store' });
        if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
        const next = await response.json();
        this.data = next;
        this.error = '';
        this.ensureSelections();
        setTimeout(() => {
          this.restoreScrollState(scrollState);
          this.loadSelectedAgentDetail();
          this.scheduleRenderInteractive();
          this.renderExplorerJson();
        }, 0);
      } catch (error) {
        this.error = `Dashboard refresh failed: ${error.message}`;
      } finally {
        this.loading = false;
      }
    },

    captureScrollState() {
      return {
        windowX: window.scrollX,
        windowY: window.scrollY,
        regions: Array.from(document.querySelectorAll('.scroll-region')).map((element, index) => ({
          index,
          left: element.scrollLeft,
          top: element.scrollTop,
        })),
      };
    },

    restoreScrollState(snapshot) {
      if (!snapshot) return;
      window.scrollTo(snapshot.windowX, snapshot.windowY);
      const regions = Array.from(document.querySelectorAll('.scroll-region'));
      snapshot.regions.forEach((region) => {
        const element = regions[region.index];
        if (element) {
          element.scrollLeft = region.left;
          element.scrollTop = region.top;
        }
      });
    },

    ensureSelections() {
      const allJobs = this.activeJobs.concat(this.recentJobs);
      if (!this.selectedJobId || !allJobs.some((job) => job.job_id === this.selectedJobId)) {
        this.selectedJobId = allJobs[0]?.job_id || '';
      }
      const selectedAgentInOverview = this.agentJobs.some((entry) => entry.job?.job_id === this.selectedAgentJobId);
      const selectedAgentHasDetail = Boolean(this.agentDetails[this.selectedAgentJobId]);
      if (!this.selectedAgentJobId || (!selectedAgentInOverview && !selectedAgentHasDetail)) {
        this.selectedAgentJobId = this.agentJobs[0]?.job?.job_id || '';
      }
      if (!this.selectedAutomationId || !this.automations.some((record) => record.automation_id === this.selectedAutomationId)) {
        this.selectedAutomationId = this.automations[0]?.automation_id || '';
      }
      if (!this.control.roomId || !this.rooms.some((room) => room.channelId === this.control.roomId)) {
        this.control.roomId = this.rooms[0]?.channelId || '';
      }
    },

    selectJob(jobId) {
      this.selectedJobId = jobId || '';
      this.activateView('jobs');
    },

    selectAgentJob(jobId) {
      this.selectedAgentJobId = jobId || '';
      if (this.selectedAgentJobId) {
        this.loadSelectedAgentDetail({ force: true });
      }
    },

    selectAutomation(automationId) {
      this.selectedAutomationId = automationId || '';
    },

    async loadSelectedAgentDetail(options = {}) {
      const jobId = options.jobId || this.selectedAgentJobId;
      if (!jobId) return;
      const cached = this.agentDetails[jobId];
      if (cached && !options.force && !this.isActiveState(cached.job?.state)) return;
      this.agentDetailLoadingId = jobId;
      try {
        const response = await fetch(`${rootPrefix}/v1/voice/debug/agents/${encodeURIComponent(jobId)}`, { cache: 'no-store' });
        if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
        const detail = await response.json();
        const returnedJobId = detail?.job?.job_id || '';
        if (returnedJobId !== jobId) {
          throw new Error(`requested ${jobId}, received ${returnedJobId || 'empty job id'}`);
        }
        this.agentDetails = { ...this.agentDetails, [jobId]: detail };
        this.agentDetailErrors = { ...this.agentDetailErrors, [jobId]: '' };
        if (this.selectedAgentJobId === jobId && this.error.startsWith('Agent detail load failed:')) {
          this.error = '';
        }
      } catch (error) {
        const message = `Agent detail load failed: ${error.message}`;
        this.agentDetailErrors = { ...this.agentDetailErrors, [jobId]: message };
        if (this.selectedAgentJobId === jobId) {
          this.error = message;
        }
      } finally {
        if (this.agentDetailLoadingId === jobId) {
          this.agentDetailLoadingId = '';
        }
      }
    },

    async sendCommand(commandKind, args = {}) {
      const room = this.rooms.find((entry) => entry.channelId === this.control.roomId);
      if (!room) {
        this.error = 'Select a room before sending a command.';
        return;
      }
      if (commandKind === 'agent_task' && !textValue(args.request).trim()) {
        this.error = 'Agent task text is required.';
        return;
      }
      const payload = {
        action: 'dispatch_now',
        command_kind: commandKind,
        guild_id: room.guildId,
        voice_channel_id: room.channelId,
        requested_by_user_id: this.control.requestedByUserId.trim() || 'dashboard',
        target_voice_channel_id: room.channelId,
        arguments: {
          channel: room.channelId,
          target_channel: room.channelId,
          ...args,
        },
      };
      try {
        const response = await fetch(`${rootPrefix}/v1/voice/commands`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(payload),
        });
        if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
        this.control.result = await response.json();
        this.control.lastKind = commandKind;
        this.control.lastAt = new Date().toLocaleTimeString();
        this.error = '';
        await this.refresh({ force: true });
      } catch (error) {
        this.error = `Command failed: ${error.message}`;
      }
    },

    isActiveState(state) {
      return ['queued', 'running', 'waiting', 'cancel_requested', 'confirmation_pending'].includes(textValue(state));
    },

    subtitle() {
      if (!this.data) return 'Loading...';
      const summary = this.jobSummary();
      return `Generated ${this.ago(this.data.generatedAt)} | uptime ${this.data.process?.uptimeSeconds ?? 0}s | ${summary.total || 0} jobs tracked`;
    },

    metrics() {
      const summary = this.jobSummary();
      const status = this.status();
      return [
        { label: 'Active Jobs', value: summary.active || 0, className: summary.active ? 'ok' : 'muted' },
        { label: 'Queued', value: summary.queued || 0, className: summary.queued ? 'warn' : 'muted' },
        { label: 'Running', value: summary.running || 0, className: summary.running ? 'ok' : 'muted' },
        { label: 'Waiting', value: summary.waiting || 0, className: summary.waiting ? 'warn' : 'muted' },
        { label: 'Failed', value: summary.failed || 0, className: summary.failed ? 'bad' : 'muted' },
        { label: 'Rooms', value: (status.rooms || []).length, className: '' },
      ];
    },

    status() {
      return this.data?.status || {};
    },

    jobSummary() {
      return this.data?.jobs?.summary || {};
    },

    database() {
      return this.data?.database || {};
    },

    get activeJobs() {
      return this.data?.jobs?.active || [];
    },

    get recentJobs() {
      return this.data?.jobs?.recent || [];
    },

    get agentJobs() {
      return this.data?.agents?.jobs || [];
    },

    get agentSessions() {
      return this.data?.agents?.sessions || [];
    },

    get automations() {
      return this.data?.automations?.records || [];
    },

    automationSummary() {
      return this.data?.automations?.summary || {};
    },

    get rooms() {
      return this.status().rooms || [];
    },

    get sessions() {
      return this.status().sessions || [];
    },

    get bots() {
      return this.status().bots || [];
    },

    get timelineEvents() {
      return this.data?.timeline?.recentEvents || [];
    },

    get transcriptEvents() {
      return this.data?.transcript?.events || [];
    },

    selectedJob() {
      return this.allJobs().find((job) => job.job_id === this.selectedJobId) || null;
    },

    selectedAgentEntry() {
      const jobId = this.selectedAgentJobId;
      const detail = this.agentDetails[jobId];
      if (detail) return detail;
      const overview = this.agentJobs.find((entry) => entry.job?.job_id === jobId);
      if (overview) return overview;
      if (jobId && this.agentDetailLoadingId === jobId) {
        return { job: { job_id: jobId, state: 'loading', kind: 'agent_task' }, codex: {}, session: null };
      }
      return null;
    },

    selectedAgentJob() {
      return this.selectedAgentEntry()?.job || null;
    },

    selectedAgentCodex() {
      return this.selectedAgentEntry()?.codex || {};
    },

    selectedAgentSession() {
      return this.selectedAgentEntry()?.session || null;
    },

    selectedAgentDetailLoading() {
      return this.selectedAgentJobId !== '' && this.agentDetailLoadingId === this.selectedAgentJobId;
    },

    selectedAgentDetailError() {
      return this.agentDetailErrors[this.selectedAgentJobId] || '';
    },

    selectedAutomation() {
      return this.automations.find((record) => record.automation_id === this.selectedAutomationId) || null;
    },

    selectedJobFacts() {
      const job = this.selectedJob();
      if (!job) return [];
      return [
        ['Job', job.job_id],
        ['Root', job.root_job_id],
        ['Parent', job.parent_job_id],
        ['Guild', job.guild_id],
        ['Channel', job.voice_channel_id],
        ['Requested By', job.requested_by_user_id],
        ['Attempts', job.attempts ?? 0],
        ['Created', job.created_at],
        ['Updated', job.updated_at],
        ['Started', job.started_at],
        ['Completed', job.completed_at],
      ].map(([label, value]) => ({ label, value: textValue(value) }));
    },

    selectedAgentFacts() {
      const entry = this.selectedAgentEntry();
      const job = entry?.job || {};
      const metadata = this.agentMetadata(job);
      const codex = entry?.codex || {};
      const stats = this.codexUsageStats(codex, metadata);
      return [
        ['Job', job.job_id],
        ['Channel', job.voice_channel_id],
        ['Requester', job.requested_by_user_id],
        ['Model', codex.model || metadata.agent?.model || ''],
        ['Session', codex.sessionId || metadata.agent?.session_id || ''],
        ['Trace Scope', entry?.session?.scope || ''],
        ['Workdir', entry?.workdir?.path || metadata.workdir_path || ''],
        ['Context', this.contextUsageLabel(stats)],
        ['Events', codex.eventCount ?? 0],
        ['Session Jobs', entry?.session?.jobCount ?? ''],
        ['Channel Jobs', entry?.session?.channelJobCount ?? ''],
      ].map(([label, value]) => ({ label, value: textValue(value) }));
    },

    selectedAutomationFacts() {
      const record = this.selectedAutomation();
      if (!record) return [];
      return [
        ['Automation', record.automation_id],
        ['Name', record.spec?.name],
        ['State', record.state],
        ['Scope', this.automationScope(record)],
        ['Trigger', this.automationTrigger(record)],
        ['Fires', `${record.fire_count ?? 0}${record.spec?.expiry?.max_fires ? `/${record.spec.expiry.max_fires}` : ''}`],
        ['Created', record.created_at],
        ['Updated', record.updated_at],
        ['Last Evaluated', record.last_evaluated_at],
        ['Last Fired', record.last_fired_at],
      ].map(([label, value]) => ({ label, value: textValue(value) }));
    },

    recentFailures() {
      return this.recentJobs.filter((job) => this.statusClass(job.state) === 'bad' || this.jobMetadata(job).error || this.agentMetadata(job).dispatch_error);
    },

    roomJobLoad() {
      return this.jobSummary().byRoom || [];
    },

    healthRows() {
      const health = this.data?.health || {};
      return [
        { label: 'Runtime', value: health.ok ? 'ok' : 'degraded', className: health.ok ? 'ok' : 'bad' },
        { label: 'Postgres', value: health.postgres ? 'ok' : 'error', className: health.postgres ? 'ok' : 'bad' },
        { label: 'Ready bots', value: `${health.readyBots ?? 0}/${health.configuredBots ?? 0}` },
        { label: 'Active sessions', value: health.activeSessions ?? 0 },
        { label: 'Active agent jobs', value: health.activeAgentJobs ?? 0 },
        { label: 'Loaded automations', value: health.automationsLoaded ?? 0 },
        { label: 'Failed jobs', value: health.failedJobs ?? 0, className: health.failedJobs ? 'bad' : '' },
      ];
    },

    loadRows() {
      const backlog = this.operationBacklog();
      return [
        { label: 'Active jobs', value: this.int(backlog.total) },
        { label: 'Due queued jobs', value: this.int(backlog.dueQueued) },
        { label: 'Queued jobs', value: this.int(backlog.queued) },
        { label: 'Running jobs', value: this.int(backlog.running) },
        { label: 'Waiting jobs', value: this.int(backlog.waiting) },
        { label: 'Oldest queued age', value: this.seconds(backlog.oldestQueuedAgeSeconds) },
        { label: 'Oldest running age', value: this.seconds(backlog.oldestRunningAgeSeconds) },
        { label: 'Cancellable jobs', value: this.int(backlog.cancellable) },
      ];
    },

    databaseRows() {
      const database = this.database();
      return [
        { label: 'URL', value: database.url || '' },
        { label: 'Database', value: database.database || '' },
        { label: 'User', value: database.user || '' },
        { label: 'Root', value: database.root || '' },
      ];
    },

    operations() {
      return this.data?.operations || {};
    },

    operationBacklog() {
      return this.operations().backlog || {};
    },

    serverLoadRows() {
      const load = this.data?.process?.load || {};
      const avg = load.loadAverage || {};
      const memory = load.memory || {};
      const cpu = load.cpu || {};
      const process = cpu.process || {};
      const cgroup = cpu.cgroup || {};
      return [
        { label: 'PID', value: load.pid || '' },
        { label: 'Threads', value: this.int(load.threads) },
        { label: 'Open files', value: this.int(load.openFileDescriptors) },
        { label: 'Load avg', value: [avg.oneMinute, avg.fiveMinute, avg.fifteenMinute].map((value) => Number(value || 0).toFixed(2)).join(' / ') },
        { label: 'Runnable threads', value: `${this.int(avg.runnableThreads)} / ${this.int(avg.totalThreads)}` },
        { label: 'RSS', value: this.bytes(memory.rssBytes) },
        { label: 'Virtual memory', value: this.bytes(memory.vmSizeBytes) },
        { label: 'Cgroup memory', value: `${this.bytes(memory.cgroupCurrentBytes)}${memory.cgroupMaxBytes ? ` / ${this.bytes(memory.cgroupMaxBytes)}` : ''}` },
        { label: 'Host available RAM', value: `${this.bytes(memory.hostAvailableBytes)} / ${this.bytes(memory.hostTotalBytes)}` },
        { label: 'CPU ticks', value: this.int(process.totalTicks) },
        { label: 'Cgroup CPU', value: this.micros(cgroup.usage_usec) },
      ];
    },

    backlogKindRows() {
      return this.operationBacklog().byKind || [];
    },

    speechWakeWindows() {
      return this.operations().windows || [];
    },

    latencyWindows() {
      return this.operations().latencies?.windows || [];
    },

    latencyKindRows() {
      return this.operations().latencies?.byKind || [];
    },

    codexUsageWindows() {
      return this.data?.agents?.codex?.usage?.windows || [];
    },

    channelOptions() {
      const channels = new Map();
      this.rooms.forEach((room) => {
        if (room.channelId) channels.set(room.channelId, { id: room.channelId, label: this.roomName(room) });
      });
      this.timelineEvents.concat(this.transcriptEvents).forEach((event) => {
        const id = this.eventChannelId(event);
        if (id && !channels.has(id)) channels.set(id, { id, label: this.eventChannelName(event) || id });
      });
      return Array.from(channels.values()).sort((left, right) => left.label.localeCompare(right.label));
    },

    timelineKinds() {
      return Array.from(new Set(this.timelineEvents.map((event) => this.eventKind(event)).filter(Boolean))).sort();
    },

    filteredTimelineEvents(options = {}) {
      const includeGlobal = options.global !== false;
      const kind = this.filters.timelineKind;
      const channel = this.filters.timelineChannel;
      const globalKind = includeGlobal ? this.filters.globalEventKind : '';
      const globalChannel = includeGlobal ? this.filters.globalRoom : '';
      const globalGuild = includeGlobal ? this.filters.globalGuild : '';
      const queries = [
        this.filters.timelineSearch,
        includeGlobal ? this.filters.globalSearch : '',
      ]
        .map((value) => textValue(value).trim().toLowerCase())
        .filter(Boolean);
      return this.timelineEvents.filter((event) => {
        if (kind && this.eventKind(event) !== kind) return false;
        if (globalKind && this.eventKind(event) !== globalKind) return false;
        if (channel && this.eventChannelId(event) !== channel) return false;
        if (globalChannel && this.eventChannelId(event) !== globalChannel) return false;
        if (globalGuild && this.eventGuildId(event) !== globalGuild) return false;
        if (!queries.length) return true;
        const haystack = [
          this.eventKind(event),
          this.eventGuildId(event),
          this.eventChannelName(event),
          this.eventChannelId(event),
          this.eventSpeaker(event),
          this.eventDetail(event),
          this.eventId(event),
        ].join(' ').toLowerCase();
        return queries.every((query) => haystack.includes(query));
      });
    },

    timelinePageEvents() {
      return this.filteredTimelineEvents({ global: false });
    },

    filteredTranscriptEvents() {
      const channel = this.filters.transcriptChannel;
      const globalChannel = this.filters.globalRoom;
      const globalGuild = this.filters.globalGuild;
      const queries = [this.filters.transcriptSearch, this.filters.globalSearch]
        .map((value) => textValue(value).trim().toLowerCase())
        .filter(Boolean);
      return this.transcriptEvents
        .filter((event) => {
          if (!this.transcriptText(event)) return false;
          if (channel && this.eventChannelId(event) !== channel) return false;
          if (globalChannel && this.eventChannelId(event) !== globalChannel) return false;
          if (globalGuild && this.eventGuildId(event) !== globalGuild) return false;
          if (!queries.length) return true;
          const haystack = [
            this.transcriptText(event),
            this.eventSpeaker(event),
            this.eventChannelName(event),
            this.eventChannelId(event),
            this.eventGuildId(event),
          ].join(' ').toLowerCase();
          return queries.every((query) => haystack.includes(query));
        })
        .sort((left, right) => this.eventWhen(left).localeCompare(this.eventWhen(right)));
    },

    transcriptGroups() {
      const groups = new Map();
      this.filteredTranscriptEvents().forEach((event) => {
        const channelId = this.eventChannelId(event) || 'unknown';
        if (!groups.has(channelId)) {
          groups.set(channelId, { channelId, channelName: this.eventChannelName(event) || channelId, events: [] });
        }
        groups.get(channelId).events.push(event);
      });
      return Array.from(groups.values());
    },

    selectedAgentTrace() {
      const codex = this.selectedAgentCodex();
      return codex.timeline?.length ? codex.timeline : this.mergedCodexEvents(codex);
    },

    selectedAgentSessionTrace() {
      return this.selectedAgentSession()?.codex?.timeline || [];
    },

    sessionTrace() {
      return this.selectedAgentTrace();
    },

    mergedCodexEvents(codex) {
      return [
        ...(codex.messages || []).map((event) => ({ ...event, kind: 'message' })),
        ...(codex.toolCalls || []).map((event) => ({ ...event, kind: 'tool_call' })),
      ];
    },

    traceBody(event) {
      if (event.text) return event.text;
      const parts = [];
      if (event.arguments !== undefined && event.arguments !== '') {
        parts.push(typeof event.arguments === 'string' ? event.arguments : this.json(event.arguments));
      }
      if (event.output !== undefined && event.output !== '') {
        parts.push(typeof event.output === 'string' ? event.output : this.json(event.output));
      }
      return parts.join('\n\n') || this.json(event);
    },

    commandKind(job) {
      return firstText([
        job?.command_kind,
        job?.payload?.command?.command_kind,
        job?.payload?.command?.arguments?.action,
        job?.payload?.action,
      ]);
    },

    jobCommand(job) {
      return job?.payload?.command || {};
    },

    jobArgs(job) {
      return this.jobCommand(job).arguments || {};
    },

    jobMetadata(job) {
      return job?.metadata || {};
    },

    agentMetadata(job) {
      return this.jobMetadata(job).agent_task || {};
    },

    jobDetail(job) {
      if (!job) return '';
      const command = this.jobCommand(job);
      const args = this.jobArgs(job);
      const metadata = this.jobMetadata(job);
      const agent = this.agentMetadata(job);
      const result = metadata.result || {};
      return firstText([
        metadata.error,
        agent.dispatch_error,
        result.message,
        result.status,
        command.acknowledgement_text,
        args.request,
        args.question,
        args.instruction_text,
        args.target_room,
        args.target_channel,
        agent.response_text,
        agent.dispatch_stdout_preview,
        job.request,
        job.response_preview,
      ]);
    },

    agentRequest(entry) {
      const job = entry?.job || {};
      return firstText([
        job.request,
        job.payload?.command?.arguments?.request,
        job.payload?.command?.arguments?.instruction_text,
        job.payload?.command?.arguments?.question,
      ]);
    },

    agentSessionId(entry) {
      const metadata = this.agentMetadata(entry?.job || {});
      return entry?.codex?.sessionId || metadata.agent?.session_id || '';
    },

    agentModel(entry) {
      const metadata = this.agentMetadata(entry?.job || {});
      return entry?.codex?.model || metadata.agent?.model || '';
    },

    codexUsageStats(codex, metadata = {}) {
      const rawUsage = codex.tokenUsage || metadata.agent?.usage || {};
      const usage = rawUsage.info || rawUsage;
      const total = usage.total_token_usage || {};
      const last = usage.last_token_usage || {};
      const inputTokens = Number(codex.contextUsedTokens || total.input_tokens || last.input_tokens || 0);
      const contextWindow = Number(codex.modelContextWindow || usage.model_context_window || usage.modelContextWindow || 0);
      const percent = Number(codex.contextUsedPercent || (contextWindow > 0 && inputTokens > 0 ? (inputTokens / contextWindow) * 100 : 0));
      return { usage, inputTokens, contextWindow, percent };
    },

    contextUsageLabel(stats) {
      if (stats.percent > 0) return this.pct(stats.percent);
      if (stats.inputTokens > 0) return `${this.int(stats.inputTokens)} tok`;
      return '';
    },

    eventKind(event) {
      return event?.kind || event?.event_kind || 'event';
    },

    eventGuildId(event) {
      return event?.guild_id || event?.guildId || '';
    },

    eventChannelId(event) {
      return event?.channelId || event?.voice_channel_id || event?.voiceChannelId || '';
    },

    eventChannelName(event) {
      return event?.channelName || event?.voice_channel_name || event?.channelSlug || this.eventChannelId(event);
    },

    eventSpeaker(event) {
      return event?.speakerLabel || event?.speaker_label || event?.speakerId || event?.speaker_user_id || '';
    },

    eventWhen(event) {
      return event?.startedAt || event?.started_at || event?.created_at || event?.timestamp || '';
    },

    eventId(event) {
      return event?.job_id || event?.eventId || event?.event_id || '';
    },

    eventDetail(event) {
      const result = event?.command_result || event?.command_response || event?.result || {};
      return firstText([
        event?.text,
        event?.reason,
        result.reason,
        result.action,
        event?.state,
      ]);
    },

    automationScope(record) {
      const scope = record?.spec?.scope || {};
      return [scope.guild_id || scope.guildId, scope.voice_channel_id || scope.voiceChannelId || scope.channelId].filter(Boolean).join(' / ');
    },

    automationTrigger(record) {
      const trigger = record?.spec?.trigger || {};
      if (trigger.Event) return `event: ${(trigger.Event.event_kinds || trigger.Event.eventKinds || []).join(', ')}`;
      if (trigger.Job) return `job: ${(trigger.Job.job_kinds || trigger.Job.jobKinds || []).join(', ')} -> ${(trigger.Job.states || []).join(', ')}`;
      if (trigger.Tick) return `tick: ${trigger.Tick.interval_seconds || trigger.Tick.intervalSeconds || 0}s`;
      if (trigger.RoomStateChanged !== undefined) return 'room state changed';
      if (trigger.kind === 'event') return `event: ${(trigger.event_kinds || trigger.eventKinds || []).join(', ')}`;
      if (trigger.kind === 'job') return `job: ${(trigger.job_kinds || trigger.jobKinds || []).join(', ')} -> ${(trigger.states || []).join(', ')}`;
      if (trigger.kind === 'tick') return `tick: ${trigger.interval_seconds || trigger.intervalSeconds || 0}s`;
      return this.short(this.json(trigger), 120);
    },

    automationActions(record) {
      return (record?.spec?.actions || []).map((action) => {
        if (action.ResponseSend) return `response.send -> ${this.automationSink(action.ResponseSend.sink)}`;
        if (action.AgentTaskStart) return 'agent_task.start';
        if (action.SoundPlay) return `sound.play ${action.SoundPlay.name || ''}`.trim();
        if (action.TranscriptStartLive) return 'transcript.start_live';
        if (action.kind) return action.kind;
        return this.short(this.json(action), 80);
      }).join(', ');
    },

    automationSink(sink) {
      if (!sink) return '';
      const kind = sink.kind || Object.keys(sink)[0] || '';
      const id = sink.id || sink.channel_id || sink.channelId || sink.user_id || sink.userId || '';
      return [kind, id].filter(Boolean).join(':');
    },

    transcriptText(event) {
      return event?.text || event?.text_draft || event?.transcript || '';
    },

    jobTime(job) {
      return job?.updated_at || job?.created_at || job?.started_at || '';
    },

    roomName(room) {
      return room?.channelName || room?.channelSlug || room?.channelId || '';
    },

    roomHumanCount(room) {
      const live = this.liveRoomOccupants(room).length;
      return live || room?.occupancy?.effective_human_count || room?.occupancy?.effectiveHumanCount || '';
    },

    liveRoomOccupants(room) {
      const guildId = room?.guildId || room?.guild_id || '';
      const channelId = room?.channelId || room?.voice_channel_id || '';
      const rooms = this.status().liveVoiceOccupancy?.rooms || [];
      const match = rooms.find((entry) => (
        (entry.guild_id || entry.guildId) === guildId
        && (entry.voice_channel_id || entry.voiceChannelId || entry.channelId) === channelId
      ));
      return match?.occupants || [];
    },

    statusClass(value) {
      const text = textValue(value).toLowerCase();
      if (['ok', 'ready', 'present', 'complete', 'queued', 'running', 'waiting', 'active', 'approved', 'capturing', 'idle'].some((part) => text.includes(part))) return 'ok';
      if (['failed', 'error', 'timeout', 'missing', 'degraded'].some((part) => text.includes(part))) return 'bad';
      if (['cancel', 'pending', 'released', 'absent', 'paused', 'truncated'].some((part) => text.includes(part))) return 'warn';
      return 'info';
    },

    ago(iso) {
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
    },

    clock(iso) {
      if (!iso || !Number.isFinite(Date.parse(iso))) return iso || '';
      return new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    },

    unixTime(seconds) {
      const value = Number(seconds || 0);
      if (!value) return '';
      return new Date(value * 1000).toLocaleString();
    },

    short(value, n = 16) {
      const text = textValue(value);
      return text.length > n ? `${text.slice(0, n)}...` : text;
    },

    seconds(value) {
      const total = Number(value || 0);
      if (!Number.isFinite(total) || total <= 0) return '0s';
      if (total < 60) return `${Math.round(total)}s`;
      const minutes = total / 60;
      if (minutes < 60) return `${minutes.toFixed(minutes >= 10 ? 0 : 1)}m`;
      const hours = minutes / 60;
      if (hours < 48) return `${hours.toFixed(hours >= 10 ? 0 : 1)}h`;
      return `${(hours / 24).toFixed(1)}d`;
    },

    millis(value) {
      const ms = Number(value);
      if (!Number.isFinite(ms)) return '';
      if (ms < 1000) return `${Math.round(ms)}ms`;
      if (ms < 60000) return `${(ms / 1000).toFixed(ms >= 10000 ? 1 : 2)}s`;
      return `${(ms / 60000).toFixed(1)}m`;
    },

    durationBetween(startIso, endIso) {
      if (!startIso || !endIso) return '';
      const start = Date.parse(startIso);
      const end = Date.parse(endIso);
      if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return '';
      return this.millis(end - start);
    },

    micros(value) {
      const us = Number(value);
      if (!Number.isFinite(us) || us <= 0) return '0ms';
      return this.millis(us / 1000);
    },

    latencyValue(stats, name, field = 'p95') {
      return this.millis(stats?.[name]?.[field]);
    },

    jobWindowLabel(window, key) {
      const stats = window?.[key] || {};
      return `${this.int(stats.total)} / ${this.int(stats.active)} active / ${this.int(stats.failed)} failed`;
    },

    bytes(value) {
      const size = Number(value || 0);
      if (!Number.isFinite(size) || size <= 0) return '0 B';
      const units = ['B', 'KB', 'MB', 'GB'];
      let current = size;
      let unit = 0;
      while (current >= 1024 && unit < units.length - 1) {
        current /= 1024;
        unit += 1;
      }
      return `${current.toFixed(unit ? 1 : 0)} ${units[unit]}`;
    },

    pct(value) {
      return `${Number(value || 0).toFixed(1)}%`;
    },

    int(value) {
      return Number(value || 0).toLocaleString();
    },

    json(value) {
      return JSON.stringify(value ?? {}, null, 2);
    },
  };
};

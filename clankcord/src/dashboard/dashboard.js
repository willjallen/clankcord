const rootPrefix = location.pathname.startsWith('/__clankcord/') ? '/__clankcord' : '';
const viewStorageKey = 'clankcord.dashboard.view';
const filterStorageKey = 'clankcord.dashboard.filters.v3';
const dashboardCategoryIds = ['conversation', 'agent', 'messaging_control', 'automation', 'operations', 'other', 'voice_detail', 'background'];

const defaultFilters = {
  jobsLimit: 120,
  agentLimit: 120,
  timelineWindow: '-1h',
  timelineStart: '',
  timelineEnd: '',
  timelineLimit: 120,
  timelineRecordTypes: null,
  timelineCategories: null,
  timelineKinds: null,
  timelineJobStates: null,
  timelineChannels: null,
  timelineSearch: '',
  timelineSearchField: 'all',
  transcriptSince: '-24h',
  transcriptLimit: 250,
  transcriptChannel: '',
  transcriptSearch: '',
  feedbackSince: '-30d',
  feedbackLimit: 250,
  feedbackSearch: '',
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
      { id: 'overview', label: 'Overview' },
      { id: 'timeline', label: 'Timeline' },
      { id: 'agents', label: 'Agent Jobs' },
      { id: 'automations', label: 'Automations' },
      { id: 'feedback', label: 'Feedback' },
      { id: 'health', label: 'Health' },
      { id: 'rooms', label: 'Rooms' },
      { id: 'control', label: 'Control' },
      { id: 'transcript', label: 'Transcript' },
      { id: 'raw', label: 'Data' },
    ],
    data: null,
    timelinePage: {
      loaded: false,
      snapshotAt: '',
      matched: 0,
      returned: 0,
      hasMore: false,
      nextCursor: '',
      records: [],
      facets: { recordTypes: [], categories: [], defaultCategories: [], kinds: [], eventKinds: [], jobKinds: [], states: [], scopes: [] },
    },
    timelineLoadingMore: false,
    timelineGeneration: 0,
    timelineLoadMoreController: null,
    loading: false,
    error: '',
    activeView: localStorage.getItem(viewStorageKey) || 'overview',
    filters: { ...defaultFilters, ...storedJson(filterStorageKey, {}) },
    selectedJobId: '',
    selectedAgentJobId: '',
    selectedAutomationId: '',
    timelineFilterEditor: '',
    ...window.ClankDashboardExplorer.initialState(),
    agentDetails: {},
    agentDetailSequences: {},
    agentDetailLoadingId: '',
    agentDetailErrors: {},
    autoRefresh: true,
    timer: null,
    refreshController: null,
    refreshSequence: 0,
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
        this.activeView = 'overview';
      }
      this.ensureTimelineWindowDefaults();
      this.syncAutoRefresh();
      this.refresh({ force: true });
    },

    syncAutoRefresh() {
      if (this.timer) clearInterval(this.timer);
      this.timer = null;
      if (this.autoRefresh) {
        this.timer = setInterval(() => this.refresh({ auto: true }), this.autoRefreshDelay());
      }
    },

    autoRefreshDelay() {
      return ({
        overview: 10000,
        timeline: 3000,
        agents: 10000,
        automations: 30000,
        feedback: 30000,
        health: 30000,
        rooms: 3000,
        control: 3000,
        transcript: 5000,
        raw: 15000,
      })[this.activeView];
    },

    activateView(view, options = {}) {
      if (!this.tabs.some((tab) => tab.id === view)) return;
      const changed = this.activeView !== view;
      this.activeView = view;
      try {
        localStorage.setItem(viewStorageKey, view);
      } catch {}
      if (changed) {
        this.syncAutoRefresh();
        if (options.refresh !== false) this.refresh({ force: true });
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
      this.refresh({ force: true });
      this.scheduleRenderInteractive();
    },

    clearTimelineSearch() {
      Object.assign(this.filters, {
        timelineSearch: '',
        timelineSearchField: 'all',
      });
      this.timelineFilterChanged();
    },

    clearTimelineFilters() {
      this.timelineFilterEditor = '';
      Object.assign(this.filters, {
        timelineRecordTypes: null,
        timelineCategories: null,
        timelineKinds: null,
        timelineJobStates: null,
        timelineChannels: null,
        timelineWindow: '-1h',
        timelineStart: this.localDateTimeInput(new Date(Date.now() - 60 * 60 * 1000)),
        timelineEnd: '',
        timelineSearch: '',
        timelineSearchField: 'all',
      });
      this.timelineFilterChanged();
    },

    applyTimelineFilter(values = {}) {
      this.timelineFilterEditor = '';
      Object.assign(this.filters, values);
      if ('timelineWindow' in values && values.timelineWindow !== 'custom') {
        this.applyTimelineWindowPreset({ refresh: false });
      }
      this.activateView('timeline', { refresh: false });
      this.timelineFilterChanged();
    },

    ...window.ClankDashboardExplorer.methods,

    jsonUrl() {
      return this.activeViewUrl();
    },

    summaryUrl() {
      return `${rootPrefix}/v1/dashboard/summary`;
    },

    activeViewUrl() {
      if (this.activeView === 'timeline') return this.timelineUrl();
      if (this.activeView === 'feedback') return this.feedbackUrl();
      if (this.activeView === 'overview') return `${rootPrefix}/v1/dashboard/overview?jobsLimit=${this.filters.jobsLimit}`;
      if (this.activeView === 'agents') return `${rootPrefix}/v1/dashboard/agents?limit=${this.filters.agentLimit}`;
      if (this.activeView === 'automations') return `${rootPrefix}/v1/dashboard/automations`;
      if (this.activeView === 'health') return `${rootPrefix}/v1/dashboard/health`;
      if (this.activeView === 'rooms' || this.activeView === 'control') return `${rootPrefix}/v1/dashboard/rooms`;
      if (this.activeView === 'transcript') {
        const params = new URLSearchParams({
          since: this.filters.transcriptSince,
          limit: String(this.filters.transcriptLimit),
          channel: textValue(this.filters.transcriptChannel).trim(),
          search: textValue(this.filters.transcriptSearch).trim(),
        });
        return `${rootPrefix}/v1/dashboard/transcript?${params.toString()}`;
      }
      return this.summaryUrl();
    },

    timelineQueryParams(cursor = '') {
      const params = new URLSearchParams({
        limit: String(this.filters.timelineLimit),
        search: textValue(this.filters.timelineSearch).trim(),
        searchField: this.filters.timelineSearchField,
      });
      const from = this.timelineInputIso(this.filters.timelineStart);
      const to = this.timelineInputIso(this.filters.timelineEnd);
      params.set('from', this.filters.timelineWindow === 'all' ? 'all' : from);
      if (to) params.set('to', to);
      if (cursor) {
        params.set('cursor', cursor);
        params.set('metadata', 'none');
      }
      this.setTimelineCategoricalParam(params, 'recordTypes', this.filters.timelineRecordTypes);
      this.setTimelineCategoryParam(params);
      this.setTimelineCategoricalParam(params, 'kinds', this.filters.timelineKinds);
      this.setTimelineCategoricalParam(params, 'states', this.filters.timelineJobStates);
      this.setTimelineCategoricalParam(params, 'scopeIds', this.filters.timelineChannels);
      return params;
    },

    setTimelineCategoricalParam(params, name, selection) {
      if (selection === null) return;
      params.set(name, selection.length ? selection.join(',') : 'none');
    },

    setTimelineCategoryParam(params) {
      const selection = this.filters.timelineCategories;
      if (selection === null) return;
      if (!selection.length) {
        params.set('categories', 'none');
        return;
      }
      const all = this.timelineCategoryOptionIds();
      const selectsAll = all.length > 0 && all.every((category) => selection.includes(category));
      params.set('categories', selectsAll ? 'all' : selection.join(','));
    },

    timelineUrl(cursor = '') {
      return `${rootPrefix}/v1/dashboard/timeline?${this.timelineQueryParams(cursor).toString()}`;
    },

    feedbackUrl() {
      const params = new URLSearchParams({
        recordTypes: 'event',
        eventKinds: 'feedback',
        jobKinds: 'none',
        search: textValue(this.filters.feedbackSearch).trim(),
        searchField: 'feedback',
        limit: String(this.filters.feedbackLimit),
        metadata: 'count',
      });
      if (this.filters.feedbackSince !== 'all') {
        params.set('from', new Date(Date.now() - this.timelineWindowDurationMs(this.filters.feedbackSince)).toISOString());
      } else {
        params.set('from', 'all');
      }
      return `${rootPrefix}/v1/dashboard/timeline?${params.toString()}`;
    },

    applyFeedbackPage(page) {
      this.data = {
        ...this.data,
        feedback: {
          snapshotAt: page.snapshotAt,
          matched: page.matched,
          returned: page.returned,
          hasMore: page.hasMore,
          events: page.records.map((record) => record.event),
        },
      };
    },

    applyTimelinePage(page, append = false) {
      if (!append) this.timelineGeneration += 1;
      const records = append ? this.timelinePage.records.concat(page.records) : page.records;
      this.timelinePage = {
        loaded: true,
        snapshotAt: append ? this.timelinePage.snapshotAt : page.snapshotAt,
        matched: append ? this.timelinePage.matched : page.matched,
        returned: records.length,
        hasMore: page.hasMore,
        nextCursor: page.nextCursor || '',
        records,
        facets: append ? this.timelinePage.facets : page.facets,
      };
    },

    async loadOlderTimeline() {
      if (!this.timelinePage.hasMore || !this.timelinePage.nextCursor || this.timelineLoadingMore) return;
      const generation = this.timelineGeneration;
      const controller = new AbortController();
      this.timelineLoadMoreController = controller;
      this.timelineLoadingMore = true;
      try {
        const response = await fetch(this.timelineUrl(this.timelinePage.nextCursor), { cache: 'no-store', signal: controller.signal });
        if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
        const page = await response.json();
        if (generation !== this.timelineGeneration) return;
        this.applyTimelinePage(page, true);
        this.error = '';
        this.scheduleRenderInteractive();
      } catch (error) {
        if (error.name === 'AbortError') return;
        this.error = `Loading older timeline records failed: ${error.message}`;
      } finally {
        if (this.timelineLoadMoreController === controller) {
          this.timelineLoadingMore = false;
          this.timelineLoadMoreController = null;
        }
      }
    },

    async refresh(options = {}) {
      if (options.auto && (this.loading || this.timelineLoadingMore)) return;
      const sequence = ++this.refreshSequence;
      if (this.refreshController) this.refreshController.abort();
      if (this.activeView === 'timeline' && this.timelineLoadMoreController) {
        this.timelineLoadMoreController.abort();
      }
      const controller = new AbortController();
      this.refreshController = controller;
      const scrollState = this.captureScrollState();
      this.loading = true;
      try {
        const summaryUrl = this.summaryUrl();
        const viewUrl = this.activeViewUrl();
        const summaryPromise = this.fetchDashboardJson(summaryUrl, controller.signal);
        const viewPromise = viewUrl === summaryUrl
          ? summaryPromise
          : this.fetchDashboardJson(viewUrl, controller.signal);
        const [summary, view] = await Promise.all([summaryPromise, viewPromise]);
        if (sequence !== this.refreshSequence) return;
        this.data = { ...this.data, ...summary };
        if (this.activeView === 'timeline') {
          this.applyTimelinePage(view);
        } else if (this.activeView === 'feedback') {
          this.applyFeedbackPage(view);
        } else {
          this.data = { ...this.data, ...view };
        }
        this.error = '';
        this.ensureSelections();
        this.refreshExplorerSelection();
        setTimeout(() => {
          this.restoreScrollState(scrollState);
          if (this.activeView === 'agents' && this.selectedAgentJobId) {
            this.loadSelectedAgentDetail();
          }
          this.scheduleRenderInteractive();
          this.renderExplorerJson();
        }, 0);
      } catch (error) {
        if (error.name === 'AbortError') return;
        this.error = `Dashboard refresh failed: ${error.message}`;
      } finally {
        if (sequence === this.refreshSequence) {
          this.loading = false;
          this.refreshController = null;
        }
      }
    },

    async fetchDashboardJson(url, signal) {
      const response = await fetch(url, { cache: 'no-store', signal });
      if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
      return response.json();
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
        tableHolders: Array.from(document.querySelectorAll('.tabulator-host')).map((element) => {
          const holder = element.querySelector('.tabulator-tableholder');
          return {
            id: element.id,
            left: holder?.scrollLeft || 0,
            top: holder?.scrollTop || 0,
          };
        }),
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
      (snapshot.tableHolders || []).forEach((region) => {
        const holder = document.getElementById(region.id)?.querySelector('.tabulator-tableholder');
        if (holder) {
          holder.scrollLeft = region.left;
          holder.scrollTop = region.top;
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

    refreshExplorerSelection() {
      const kind = this.explorerSelection.kind;
      const id = this.explorerSelectionId();
      if (!kind || !id) return;
      const records = kind === 'job'
        ? this.timelinePage.records.filter((record) => record.recordType === 'job').map((record) => record.job).concat(this.allJobs())
        : this.timelinePage.records.filter((record) => record.recordType === 'event').map((record) => record.event).concat(this.timelineEvents, this.transcriptEvents, this.feedbackEvents);
      const current = records.find((record) => (kind === 'job' ? record.job_id : this.eventId(record)) === id);
      if (current) {
        this.explorerSelection = { kind, record: current };
      } else if (this.activeView === 'timeline') {
        this.explorerSelection = { kind: '', record: null };
      }
    },

    selectJob(jobId) {
      this.selectedJobId = jobId || '';
      const job = this.allJobs().find((record) => record.job_id === this.selectedJobId);
      if (job) {
        this.selectExplorerRecord('job', job);
      }
      this.applyTimelineFilter({
        timelineCategories: this.timelineDrilldownCategories(job?.category),
        timelineSearch: this.selectedJobId,
        timelineSearchField: 'all',
        timelineWindow: 'all',
      });
    },

    selectAgentJob(jobId) {
      this.selectedAgentJobId = jobId || '';
      if (this.selectedAgentJobId) {
        this.loadSelectedAgentDetail({ force: true });
      }
    },

    selectAgentSession(session) {
      const jobId = session?.active_job_id || session?.latest_job_id || '';
      if (jobId) {
        this.selectAgentJob(jobId);
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
      const sequence = (this.agentDetailSequences[jobId] || 0) + 1;
      this.agentDetailSequences = { ...this.agentDetailSequences, [jobId]: sequence };
      this.agentDetailLoadingId = jobId;
      try {
        const response = await fetch(`${rootPrefix}/v1/dashboard/agents/${encodeURIComponent(jobId)}`, { cache: 'no-store' });
        if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
        const detail = await response.json();
        const returnedJobId = detail?.job?.job_id || '';
        if (returnedJobId !== jobId) {
          throw new Error(`requested ${jobId}, received ${returnedJobId || 'empty job id'}`);
        }
        if (this.agentDetailSequences[jobId] !== sequence) return;
        this.agentDetails = { ...this.agentDetails, [jobId]: detail };
        this.agentDetailErrors = { ...this.agentDetailErrors, [jobId]: '' };
        if (this.selectedAgentJobId === jobId && this.error.startsWith('Agent detail load failed:')) {
          this.error = '';
        }
      } catch (error) {
        if (this.agentDetailSequences[jobId] !== sequence) return;
        const message = `Agent detail load failed: ${error.message}`;
        this.agentDetailErrors = { ...this.agentDetailErrors, [jobId]: message };
        if (this.selectedAgentJobId === jobId) {
          this.error = message;
        }
      } finally {
        if (this.agentDetailLoadingId === jobId && this.agentDetailSequences[jobId] === sequence) {
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
        scope_id: room.channelId,
        requested_by_user_id: this.control.requestedByUserId.trim() || 'dashboard',
        target_channel_id: room.channelId,
        arguments: {
          channel: room.channelId,
          target_channel: room.channelId,
          ...args,
        },
      };
      try {
        const response = await fetch(`${rootPrefix}/v1/commands`, {
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
      return `Updated ${this.ago(this.data.generatedAt)} | ${this.data.health?.status || 'unknown'} | ${summary.active || 0} active jobs`;
    },

    metrics() {
      const summary = this.jobSummary();
      const health = this.data?.health || {};
      const failures = health.failures || {};
      const backlog = this.operationBacklog();
      return [
        { label: 'Health', value: health.status || 'unknown', className: this.statusClass(health.status || 'unknown') },
        { label: 'Active Jobs', value: summary.active || 0, className: summary.active ? 'ok' : 'muted' },
        { label: 'Due Queued', value: backlog.dueQueued || 0, className: backlog.dueQueued ? 'warn' : 'muted' },
        { label: 'Running', value: summary.running || 0, className: summary.running ? 'ok' : 'muted' },
        { label: failures.complete ? 'Failures (1h)' : 'Failures (1h, partial)', value: failures.complete ? (failures.count || 0) : `≥${failures.count || 0}`, className: failures.count ? 'bad' : 'muted' },
        { label: 'Rooms', value: health.inventory?.configuredRooms || 0, className: '' },
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

    agentSummary() {
      return this.data?.agents?.summary || {};
    },

    agentSummaryRows() {
      const summary = this.agentSummary();
      return [
        { label: 'Runs · 24h', value: this.int(summary.total), className: '' },
        { label: 'Active · now', value: this.int(summary.active), className: summary.active ? 'info' : 'muted' },
        { label: 'Completed · 24h', value: this.int(summary.completed), className: summary.completed ? 'ok' : 'muted' },
        { label: 'Failed · 24h', value: this.int(summary.failed), className: summary.failed ? 'bad' : 'muted' },
      ];
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

    get feedbackEvents() {
      return this.data?.feedback?.events || [];
    },

    feedbackCountLabel() {
      const feedback = this.data?.feedback || {};
      const returned = feedback.returned ?? this.feedbackEvents.length;
      const matched = feedback.matched ?? returned;
      return feedback.hasMore ? `${returned} of ${matched}` : `${matched}`;
    },

    feedbackText(event) {
      return firstText([event?.feedback_message, event?.text, event?.reason]);
    },

    openFeedbackInTimeline(event = null) {
      this.applyTimelineFilter({
        timelineRecordTypes: ['event'],
        timelineCategories: this.timelineDrilldownCategories(event?.category),
        timelineKinds: ['feedback'],
        timelineJobStates: null,
        timelineChannels: event ? [this.eventChannelId(event)].filter(Boolean) : null,
        timelineSearch: event ? this.feedbackText(event) : textValue(this.filters.feedbackSearch).trim(),
        timelineSearchField: 'feedback',
        timelineWindow: this.filters.feedbackSince,
      });
    },

    selectedJob() {
      return this.allJobs().find((job) => job.job_id === this.selectedJobId) || null;
    },

    selectedAgentEntry() {
      return this.agentDetails[this.selectedAgentJobId] || null;
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

    agentOutcomeLabel(state) {
      return ({
        queued: 'Queued',
        running: 'Running',
        waiting: 'Waiting',
        complete: 'Completed',
        cancelled: 'Cancelled',
        cancel_requested: 'Cancellation requested',
        confirmation_pending: 'Awaiting confirmation',
        approved: 'Approved',
        approval_failed: 'Approval failed',
        failed: 'Failed',
        failed_timeout: 'Timed out',
        failed_draft_retained: 'Failed · draft retained',
      })[state] || textValue(state).replaceAll('_', ' ') || 'Unknown';
    },

    agentRunDuration(job) {
      return this.millis(job?.durationMs);
    },

    agentAttemptLabel(entry) {
      return this.int(entry?.job?.attempts);
    },

    agentRunRequest(entry) {
      return entry?.job?.request || '';
    },

    agentRunModel(entry) {
      return entry?.codex?.model || '';
    },

    agentRunScopeLabel(job) {
      return job?.scopeLabel || '';
    },

    selectedAgentRunTitle() {
      return this.short(this.selectedAgentRequest() || 'Agent run without request text', 180);
    },

    selectedAgentRequest() {
      return this.agentRunRequest(this.selectedAgentEntry());
    },

    selectedAgentFinalResult() {
      return textValue(this.selectedAgentEntry()?.result?.content).trim();
    },

    selectedAgentFinalResultStatus() {
      const artifact = this.selectedAgentEntry()?.result || {};
      if (!artifact.exists) return 'not captured';
      const size = artifact.bytes ? this.bytes(artifact.bytes) : 'captured';
      return artifact.truncated ? `${size} · truncated` : size;
    },

    selectedAgentFinalResultEmptyMessage() {
      return this.isTerminalState(this.selectedAgentJob()?.state)
        ? 'No final result artifact was captured for this run.'
        : 'The run has not produced a final result yet.';
    },

    selectedAgentRunFacts() {
      const job = this.selectedAgentJob() || {};
      const finishedAt = job.completed_at || (this.isTerminalState(job.state) ? job.updated_at : '');
      return [
        ['Duration', this.agentRunDuration(job)],
        ['Scope', this.agentRunScopeLabel(job)],
        ['Requester', job.requestedByLabel || 'Unresolved requester'],
        ['Created', this.dateTime(job.created_at)],
        ['Started', job.started_at ? this.dateTime(job.started_at) : 'Not started'],
        ['Finished', finishedAt ? this.dateTime(finishedAt) : 'In progress'],
      ].map(([label, value]) => ({ label, value }));
    },

    selectedAgentPhases() {
      const job = this.selectedAgentJob();
      if (!job) return [];
      const terminal = this.isTerminalState(job.state);
      const finishedAt = job.completed_at || (terminal ? job.updated_at : '');
      const queueDuration = job.started_at
        ? this.durationBetween(job.created_at, job.started_at)
        : this.agentRunDuration(job);
      const executionDuration = job.started_at
        ? (finishedAt
          ? this.durationBetween(job.started_at, finishedAt)
          : this.millis(Math.max(0, Date.now() - Date.parse(job.started_at))))
        : '';
      return [
        {
          label: 'Queued',
          detail: job.started_at ? `${queueDuration} · ${this.dateTime(job.created_at)}` : `waiting ${queueDuration}`,
          className: job.started_at ? 'complete' : 'active',
        },
        {
          label: 'Execute',
          detail: job.started_at ? `${executionDuration} · ${this.dateTime(job.started_at)}` : 'not started',
          className: terminal ? 'complete' : (job.started_at ? 'active' : 'pending'),
        },
        {
          label: 'Outcome',
          detail: terminal ? `${this.agentOutcomeLabel(job.state)} · ${this.dateTime(finishedAt)}` : this.agentOutcomeLabel(job.state),
          className: terminal ? this.statusClass(job.state) : 'pending',
        },
      ];
    },

    selectedAgentErrorRows() {
      const job = this.selectedAgentJob() || {};
      const metadata = this.jobMetadata(job);
      const task = this.agentMetadata(job);
      const rows = [];
      const seen = new Set();
      const add = (source, detail, level = 'error') => {
        const text = textValue(detail).trim();
        if (!text || seen.has(text)) return;
        seen.add(text);
        rows.push({ source, detail: text, level, className: level === 'error' ? 'bad' : 'warn' });
      };
      add('Run', metadata.error);
      add('Dispatch', task.dispatch_error);
      add('Cancellation', task.dispatch_error_after_cancel);
      for (const check of task.preflight?.checks || []) {
        if (!check.ok) {
          add('Preflight', [check.command, check.error, check.stderr_preview].filter(Boolean).join(' · '));
        }
      }
      add('Codex stderr', task.dispatch_stderr, 'warning');
      if (task.result_suppressed) {
        add('Delivery', 'The agent result was intentionally suppressed from delivery.', 'warning');
      }
      return rows;
    },

    selectedAgentAttemptRows() {
      const job = this.selectedAgentJob() || {};
      const task = this.agentMetadata(job);
      const checks = task.preflight?.checks || [];
      const passed = checks.filter((check) => check.ok).length;
      return [
        { label: 'Scheduler attempts', value: this.int(job.attempts) },
        { label: 'Codex dispatch attempts', value: this.int(task.dispatch_attempts) },
        { label: 'Preflight checks', value: checks.length ? `${passed} / ${checks.length} passed` : 'not run' },
        { label: 'Tool calls', value: this.int(this.selectedAgentCodex().toolCalls?.length) },
      ];
    },

    selectedAgentUsageRows() {
      const codex = this.selectedAgentCodex();
      const total = codex.tokenUsage?.total_token_usage || {};
      return [
        { label: 'Input tokens', value: this.int(total.input_tokens) },
        { label: 'Cached input', value: this.int(total.cached_input_tokens) },
        { label: 'Output tokens', value: this.int(total.output_tokens) },
        { label: 'Reasoning output', value: this.int(total.reasoning_output_tokens) },
        { label: 'Context window', value: this.int(codex.modelContextWindow) },
      ];
    },

    selectedAgentContextLabel() {
      const codex = this.selectedAgentCodex();
      if (codex.contextUsedPercent > 0) return this.pct(codex.contextUsedPercent);
      return codex.contextUsedTokens > 0 ? `${this.int(codex.contextUsedTokens)} tok` : '';
    },

    selectedAgentExecutionTimeline() {
      const job = this.selectedAgentJob();
      if (!job) return [];
      const rows = [];
      const add = (row) => rows.push({ sequence: rows.length, body: '', status: '', ...row });
      add({ kind: 'phase', title: 'Run queued', timestamp: job.created_at, status: 'queued' });
      if (job.next_run_at && job.next_run_at !== job.created_at) {
        add({ kind: 'phase', title: 'Run became eligible', timestamp: job.next_run_at, status: 'ready' });
      }
      if (job.started_at) {
        add({ kind: 'phase', title: 'Execution started', timestamp: job.started_at, status: 'running' });
      }
      for (const event of this.selectedAgentTrace()) {
        add({
          kind: event.kind || 'codex',
          title: this.agentTimelineTitle(event),
          timestamp: event.timestamp || '',
          status: event.status || event.phase || '',
          body: this.traceBody(event),
        });
      }
      const terminalAt = job.completed_at || (this.isTerminalState(job.state) ? job.updated_at : '');
      if (terminalAt) {
        add({ kind: 'phase', title: `Run ${this.agentOutcomeLabel(job.state).toLowerCase()}`, timestamp: terminalAt, status: job.state });
      }
      return rows.sort((left, right) => {
        const leftAt = Date.parse(left.timestamp);
        const rightAt = Date.parse(right.timestamp);
        if (Number.isFinite(leftAt) && Number.isFinite(rightAt) && leftAt !== rightAt) return leftAt - rightAt;
        if (Number.isFinite(leftAt) !== Number.isFinite(rightAt)) return Number.isFinite(leftAt) ? -1 : 1;
        return left.sequence - right.sequence;
      });
    },

    agentTimelineTitle(event) {
      if (event.kind === 'message') {
        const role = textValue(event.role || 'agent');
        return `${role.charAt(0).toUpperCase()}${role.slice(1)} message`;
      }
      if (event.kind === 'tool_call') {
        return event.name === 'command_execution' ? 'Command execution' : (event.name || 'Tool call');
      }
      return textValue(event.kind).replaceAll('_', ' ') || 'Codex activity';
    },

    agentTimelineTime(event) {
      return event.timestamp ? this.dateTime(event.timestamp) : 'Time not recorded';
    },

    selectedAgentLineage() {
      const job = this.selectedAgentJob();
      if (!job) return [];
      const rows = [];
      if (job.root_job_id && job.root_job_id !== job.job_id) {
        rows.push({ role: 'Root', jobId: job.root_job_id, current: false });
      }
      if (job.parent_job_id && job.parent_job_id !== job.root_job_id && job.parent_job_id !== job.job_id) {
        rows.push({ role: 'Parent', jobId: job.parent_job_id, current: false });
      }
      rows.push({ role: job.root_job_id === job.job_id ? 'Root / selected' : 'Selected', jobId: job.job_id, current: true });
      return rows;
    },

    selectedAgentRelatedRuns() {
      return [...(this.selectedAgentSession()?.jobs || [])]
        .sort((left, right) => textValue(right.created_at).localeCompare(textValue(left.created_at)));
    },

    selectedAgentRelatedWorkLabel() {
      const session = this.selectedAgentSession() || {};
      const shown = this.selectedAgentRelatedRuns().length;
      if (session.truncated) return `${shown} latest of ${this.int(session.totalJobCount)} session runs`;
      return `${shown} session ${shown === 1 ? 'run' : 'runs'}`;
    },

    agentSessionScopeLabel(session) {
      const resolved = textValue(session?.scopeLabel).trim();
      if (resolved) return resolved;
      if (textValue(session?.key).startsWith('dm:')) return this.scopeKindLabel('dm');
      if (textValue(session?.key).startsWith('thread:')) return this.scopeKindLabel('thread');
      if (textValue(session?.key).startsWith('voice:')) return this.scopeKindLabel('voice_channel');
      return 'Unresolved session route';
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
        ['Scope', this.jobScopeLabel(job)],
        ['Requested By', job.requestedByLabel || 'Unresolved requester'],
        ['Attempts', job.attempts ?? 0],
        ['Created', job.created_at],
        ['Updated', job.updated_at],
        ['Started', job.started_at],
        ['Completed', job.completed_at],
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
      return this.data?.operations?.failures?.recent || [];
    },

    failureCoverageLabel() {
      const failures = this.data?.operations?.failures || {};
      if (failures.complete) return `${failures.count || 0} in the last hour`;
      return `at least ${failures.count || 0} since ${this.ago(failures.coverageStartsAt)}`;
    },

    failureScopeLabel(failure) {
      if (failure.scopeLabel) return failure.scopeLabel;
      if (failure.scopeKind === 'voice_channel') return this.roomLabel(failure.scopeId || '');
      return this.scopeKindLabel(failure.scopeKind || '');
    },

    traceFailure(failure) {
      this.applyTimelineFilter({
        timelineRecordTypes: ['job'],
        timelineCategories: this.timelineDrilldownCategories(failure.category),
        timelineKinds: [failure.kind],
        timelineJobStates: [failure.state],
        timelineChannels: failure.scopeId ? [failure.scopeId] : null,
        timelineSearch: failure.jobId,
        timelineSearchField: 'all',
        timelineWindow: 'all',
      });
    },

    scopeJobLoad() {
      return this.jobSummary().byScope || [];
    },

    scopeJobLabel(scope) {
      const resolved = textValue(scope?.scopeLabel).trim();
      if (resolved) return resolved;
      if (scope?.scope_kind === 'voice_channel') return this.roomLabel(scope?.scope_id || '');
      return this.scopeKindLabel(scope?.scope_kind || '');
    },

    healthRows() {
      const health = this.data?.health || {};
      const overall = {
        label: 'Overall',
        status: health.status,
        reason: `Observed ${this.ago(health.observedAt)}`,
        className: this.statusClass(health.status),
      };
      const components = (health.components || []).map((component) => ({
        label: component.component.replaceAll('_', ' '),
        status: component.status,
        reason: component.reason,
        className: this.statusClass(component.status),
      }));
      return [overall].concat(components);
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
      const stats = database.statistics || {};
      const pool = database.pool || {};
      return [
        { label: 'URL', value: database.url || '' },
        { label: 'Database', value: database.database || '' },
        { label: 'User', value: database.user || '' },
        { label: 'Root', value: database.root || '' },
        { label: 'Database size', value: this.bytes(stats.databaseSizeBytes) },
        { label: 'Cache hit', value: stats.cacheHitPercent === null || stats.cacheHitPercent === undefined ? '' : this.pct(stats.cacheHitPercent) },
        { label: 'Backends', value: this.int(stats.backends) },
        { label: 'Pool in use', value: `${this.int(pool.inUseConnections)} / ${this.int(pool.openConnections)} open / ${this.int(pool.configuredMaxConnections)} max` },
        { label: 'Pool idle', value: this.int(pool.idleConnections) },
        { label: 'Transactions', value: `${this.int(stats.transactions)}${stats.rollbackPercent === null || stats.rollbackPercent === undefined ? '' : ` (${this.pct(stats.rollbackPercent)} rollback)`}` },
        { label: 'Temp files', value: this.int(stats.tempFiles) },
        { label: 'Temp bytes', value: this.bytes(stats.tempBytes) },
        { label: 'Deadlocks', value: this.int(stats.deadlocks) },
        { label: 'Stats reset', value: stats.statsResetAt || '' },
      ];
    },

    requestRows() {
      const requests = this.data?.requests || {};
      return [
        { label: 'Started', value: this.int(requests.totalStarted) },
        { label: 'Completed', value: this.int(requests.completed) },
        { label: 'In flight', value: this.int(requests.inFlight) },
        { label: 'Successful', value: this.int(requests.successful) },
        { label: 'Client errors', value: this.int(requests.clientErrors), className: requests.clientErrors ? 'bad' : '' },
        { label: 'Server errors', value: this.int(requests.serverErrors), className: requests.serverErrors ? 'bad' : '' },
        { label: 'Avg latency', value: this.micros(requests.averageLatencyMicros) },
        { label: 'Max latency', value: this.micros(requests.maxLatencyMicros) },
        { label: 'Tracking since', value: requests.startedAt || '' },
      ];
    },

    requestRouteRows() {
      return this.data?.requests?.routes || [];
    },

    postgresActivityRows() {
      return this.database().activity || [];
    },

    postgresLockRows() {
      return this.database().locks || [];
    },

    postgresSettingRows() {
      return (this.database().settings || []).map((setting) => ({
        label: setting.name,
        value: `${setting.setting || ''}${setting.unit ? ` ${setting.unit}` : ''}`,
      }));
    },

    postgresTableActivityRows() {
      return this.database().tableActivity || [];
    },

    databaseErrorRows() {
      return this.database().errors || [];
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

    latencyCoverageLabel(window) {
      const coverage = window?.coverage || {};
      return coverage.complete ? 'complete' : `${this.seconds(coverage.coveredSeconds)} observed`;
    },

    latencyKindCoverageLabel() {
      return this.latencyCoverageLabel(this.latencyWindows().find((window) => window.label === '1h'));
    },

    codexUsageWindows() {
      return this.data?.agents?.codex?.usage?.windows || [];
    },

    channelOptions() {
      const channels = new Map();
      this.rooms.forEach((room) => {
        if (room.channelId) channels.set(room.channelId, { id: room.channelId, label: this.roomName(room) });
      });
      this.allJobs().forEach((job) => {
        if (job.scope_id && !channels.has(job.scope_id)) {
          channels.set(job.scope_id, { id: job.scope_id, label: this.jobScopeLabel(job) });
        }
      });
      this.timelineEvents.concat(this.transcriptEvents).forEach((event) => {
        const id = this.eventChannelId(event);
        if (id && !channels.has(id)) channels.set(id, { id, label: this.eventScopeLabel(event) });
      });
      return Array.from(channels.values()).sort((left, right) => left.label.localeCompare(right.label));
    },

    timelineKinds() {
      return Array.from(new Set(this.timelineEvents.map((event) => this.eventKind(event)).filter(Boolean))).sort();
    },

    timelineRecordTypeOptions() {
      return [
        { id: 'event', label: 'Events' },
        { id: 'job', label: 'Jobs' },
      ];
    },

    timelineKindOptions() {
      return Array.from(new Set([
        ...this.timelineEvents.map((event) => this.eventKind(event)),
        ...this.timelineEvents.map((event) => event?.job_kind),
        ...this.allJobs().map((job) => job.kind),
      ].filter(Boolean))).sort();
    },

    timelineJobStateOptions() {
      return Array.from(new Set(this.allJobs().map((job) => job.state).filter(Boolean))).sort();
    },

    timelineSearchFieldOptions() {
      return [
        { id: 'all', label: 'All Fields' },
        { id: 'detail', label: 'Text / Detail' },
        { id: 'feedback', label: 'Feedback' },
        { id: 'kind', label: 'Event Kind' },
        { id: 'job_kind', label: 'Job Type' },
        { id: 'state', label: 'State' },
        { id: 'command', label: 'Command' },
        { id: 'room', label: 'Scope' },
        { id: 'actor', label: 'Actor' },
      ];
    },

    ensureTimelineWindowDefaults() {
      if (!this.filters.timelineWindow) {
        this.filters.timelineWindow = '-1h';
      }
      if (this.filters.timelineWindow !== 'custom') {
        this.applyTimelineWindowPreset({ refresh: false });
      }
    },

    timelineWindowPresetChanged() {
      this.applyTimelineWindowPreset({ refresh: true });
    },

    timelineDateRangeChanged() {
      this.filters.timelineWindow = 'custom';
      this.timelineFilterChanged();
    },

    applyTimelineWindowPreset(options = {}) {
      if (this.filters.timelineWindow === 'all') {
        this.filters.timelineStart = '';
        this.filters.timelineEnd = '';
      } else if (this.filters.timelineWindow !== 'custom') {
        this.filters.timelineStart = this.localDateTimeInput(new Date(Date.now() - this.timelineWindowDurationMs(this.filters.timelineWindow)));
        this.filters.timelineEnd = '';
      }
      if (options.refresh) {
        this.timelineFilterChanged();
      }
    },

    timelineWindowDurationMs(value) {
      return {
        '-15m': 15 * 60 * 1000,
        '-1h': 60 * 60 * 1000,
        '-6h': 6 * 60 * 60 * 1000,
        '-24h': 24 * 60 * 60 * 1000,
        '-3d': 3 * 24 * 60 * 60 * 1000,
        '-7d': 7 * 24 * 60 * 60 * 1000,
        '-30d': 30 * 24 * 60 * 60 * 1000,
      }[value];
    },

    localDateTimeInput(date) {
      const pad = (value) => String(value).padStart(2, '0');
      return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
    },

    timelineInputIso(value) {
      const text = textValue(value).trim();
      return text ? new Date(text).toISOString() : '';
    },

    timelineTimeMatches(whenMs) {
      if (this.filters.timelineWindow === 'all') return true;
      const startMs = Date.parse(textValue(this.filters.timelineStart));
      const endMs = this.filters.timelineEnd ? Date.parse(textValue(this.filters.timelineEnd)) : Date.now();
      if (startMs && whenMs < startMs) return false;
      if (endMs && whenMs > endMs) return false;
      return true;
    },

    timelineWindowLabel() {
      const start = textValue(this.filters.timelineStart).replace('T', ' ') || 'open';
      const end = textValue(this.filters.timelineEnd).replace('T', ' ') || 'now';
      if (this.filters.timelineWindow === 'all') return 'all';
      return `${start} to ${end}`;
    },

    timelineSearchLabel() {
      const field = this.timelineSearchFieldOptions().find((option) => option.id === this.filters.timelineSearchField)?.label || 'All Fields';
      const query = textValue(this.filters.timelineSearch).trim();
      return query ? `${field}: ${query}` : field;
    },

    timelineFilterEditorOpen(field) {
      return this.timelineFilterEditor === field;
    },

    toggleTimelineFilterEditor(field) {
      this.timelineFilterEditor = this.timelineFilterEditor === field ? '' : field;
    },

    closeTimelineFilterEditor() {
      this.timelineFilterEditor = '';
    },

    timelineFilterTitle(field) {
      return {
        timelineRecordTypes: 'Record',
        timelineCategories: 'Category',
        timelineKinds: 'Kind',
        timelineJobStates: 'State',
        timelineChannels: 'Scope',
      }[field] || '';
    },

    timelineFilterOptionRows(field) {
      const facets = this.timelinePage.facets;
      if (field === 'timelineRecordTypes') return facets.recordTypes.length ? facets.recordTypes.map((value) => ({ id: value, label: value === 'event' ? 'Events' : 'Jobs' })) : this.timelineRecordTypeOptions();
      if (field === 'timelineCategories') return this.timelineCategoryRows();
      if (field === 'timelineKinds') {
        const values = facets.kinds;
        return (values.length ? values : this.timelineKindOptions()).map((value) => ({ id: value, label: value }));
      }
      if (field === 'timelineJobStates') {
        const values = facets.states.length ? facets.states : this.timelineJobStateOptions();
        return values.map((value) => ({ id: value, label: value }));
      }
      if (field === 'timelineChannels') return facets.scopes.length ? facets.scopes.map((scope) => ({ id: scope.id, label: scope.label })) : this.channelOptions();
      return [];
    },

    timelineFilterOptionIds(field) {
      return this.timelineFilterOptionRows(field).map((option) => option.id).filter(Boolean);
    },

    timelineCategoryRows() {
      const categories = this.timelinePage.facets.categories || [];
      const defaults = this.timelineDefaultCategoryIds();
      if (categories.length) {
        return categories.map((category) => ({
          ...category,
          kinds: category.kinds || [],
          defaultSelected: defaults.includes(category.id),
        }));
      }
      return dashboardCategoryIds.map((id) => ({
        id,
        label: id.replaceAll('_', ' '),
        count: 0,
        kinds: [],
        defaultSelected: false,
      }));
    },

    timelineCategoryOptionIds() {
      const categories = this.timelinePage.facets.categories || [];
      return categories.length ? categories.map((category) => category.id) : [...dashboardCategoryIds];
    },

    timelineDrilldownCategories(category = '') {
      return category ? [category] : this.timelineCategoryOptionIds();
    },

    timelineDefaultCategoryIds() {
      return [...(this.timelinePage.facets.defaultCategories || [])];
    },

    timelineCategorySelected(categoryId) {
      return this.timelineFilterValues('timelineCategories').includes(categoryId);
    },

    timelineCategoryCoverageSummary() {
      const total = this.timelineCategoryOptionIds().length;
      const selected = this.timelineFilterValues('timelineCategories').length;
      return `${selected}/${total} categories`;
    },

    timelineKindGroups() {
      return this.timelineCategoryRows();
    },

    timelineKindSelected(categoryId, kind) {
      return this.timelineCategorySelected(categoryId)
        && this.timelineFilterValues('timelineKinds').includes(kind);
    },

    normalizeTimelineCategorySelection(values) {
      const selected = new Set(values);
      const ordered = this.timelineCategoryOptionIds().filter((category) => selected.has(category));
      const defaults = this.timelineDefaultCategoryIds();
      const matchesDefault = ordered.length === defaults.length
        && defaults.every((category) => selected.has(category));
      return matchesDefault ? null : ordered;
    },

    setTimelineCategoryEnabled(categoryId, enabled) {
      const current = this.timelineFilterValues('timelineCategories');
      const next = enabled
        ? current.concat([categoryId])
        : current.filter((category) => category !== categoryId);
      this.filters.timelineCategories = this.normalizeTimelineCategorySelection(next);
    },

    selectAllTimelineKindGroup(categoryId) {
      this.setTimelineCategoryEnabled(categoryId, true);
      const explicitKinds = this.rawTimelineFilterValues('timelineKinds');
      if (explicitKinds !== null) {
        const group = this.timelineKindGroups().find((category) => category.id === categoryId);
        const next = explicitKinds.concat(group?.kinds || []);
        const normalized = this.normalizeTimelineFilterValues('timelineKinds', next);
        this.filters.timelineKinds = normalized.length === this.timelineFilterOptionIds('timelineKinds').length ? null : normalized;
      }
      this.timelineFilterChanged();
    },

    selectNoTimelineKindGroup(categoryId) {
      this.setTimelineCategoryEnabled(categoryId, false);
      this.timelineFilterChanged();
    },

    toggleTimelineKindValue(categoryId, kind) {
      if (!this.timelineCategorySelected(categoryId)) {
        const enabledCategories = new Set(this.timelineFilterValues('timelineCategories'));
        const groups = this.timelineKindGroups();
        const explicitKinds = this.rawTimelineFilterValues('timelineKinds');
        const effectiveEnabledKinds = explicitKinds === null
          ? groups.filter((group) => enabledCategories.has(group.id)).flatMap((group) => group.kinds)
          : explicitKinds.filter((selectedKind) => groups.some((group) => enabledCategories.has(group.id) && group.kinds.includes(selectedKind)));
        this.setTimelineCategoryEnabled(categoryId, true);
        this.filters.timelineKinds = this.normalizeTimelineFilterValues('timelineKinds', effectiveEnabledKinds.concat([kind]));
        this.timelineFilterChanged();
        return;
      }
      this.toggleTimelineFilterValue('timelineKinds', kind);
    },

    rawTimelineFilterValues(field) {
      if (this.filters[field] === null) return null;
      const values = Array.isArray(this.filters[field]) ? this.filters[field] : [];
      return this.normalizeTimelineFilterValues(field, values);
    },

    normalizeTimelineFilterValues(field, values) {
      return Array.from(new Set(values.map(textValue).filter(Boolean)));
    },

    timelineFilterValues(field) {
      const explicit = this.rawTimelineFilterValues(field);
      if (field === 'timelineCategories' && explicit === null) return this.timelineDefaultCategoryIds();
      return explicit === null ? this.timelineFilterOptionIds(field) : explicit;
    },

    effectiveTimelineFilterValues(field) {
      return this.rawTimelineFilterValues(field);
    },

    timelineFilterSelected(field, value) {
      return this.timelineFilterValues(field).includes(value);
    },

    toggleTimelineFilterValue(field, value) {
      const current = this.timelineFilterValues(field);
      const next = current.includes(value)
        ? current.filter((entry) => entry !== value)
        : current.concat([value]);
      if (field === 'timelineCategories') {
        this.filters[field] = this.normalizeTimelineCategorySelection(next);
        this.timelineFilterChanged();
        return;
      }
      const normalized = this.normalizeTimelineFilterValues(field, next);
      this.filters[field] = normalized.length === this.timelineFilterOptionIds(field).length ? null : normalized;
      this.timelineFilterChanged();
    },

    selectAllTimelineFilterValues(field) {
      this.filters[field] = field === 'timelineCategories' ? this.timelineCategoryOptionIds() : null;
      this.timelineFilterChanged();
    },

    selectNoTimelineFilterValues(field) {
      this.filters[field] = [];
      this.timelineFilterChanged();
    },

    timelineFilterSummary(field) {
      const explicit = this.rawTimelineFilterValues(field);
      if (field === 'timelineCategories') {
        if (explicit === null) return `Default · ${this.timelineCategoryCoverageSummary()}`;
        if (!explicit.length) return 'None';
        if (explicit.length === this.timelineCategoryOptionIds().length) return 'All';
        if (explicit.length === 1) return this.timelineFilterDisplay(field, explicit[0]);
        return this.timelineCategoryCoverageSummary();
      }
      if (field === 'timelineKinds') {
        const coverage = this.timelineCategoryCoverageSummary();
        if (explicit === null) return this.timelineFilterValues('timelineCategories').length === this.timelineCategoryOptionIds().length ? 'All' : `All · ${coverage}`;
        if (!explicit.length) return 'None';
        const total = this.timelineFilterOptionIds(field).length;
        if (explicit.length === 1) return `${this.short(this.timelineFilterDisplay(field, explicit[0]), 16)} · ${coverage}`;
        return `${explicit.length}/${total} kinds · ${coverage}`;
      }
      if (explicit === null) return 'All';
      if (!explicit.length) return 'None';
      const values = explicit;
      const total = this.timelineFilterOptionIds(field).length;
      if (values.length === 1) return this.short(this.timelineFilterDisplay(field, values[0]), 22);
      return `${values.length}/${total} selected`;
    },

    timelineFilterDisplay(field, value) {
      if (field === 'timelineChannels') {
        return this.timelinePage.facets.scopes.find((scope) => scope.id === value)?.label
          || this.channelOptions().find((channel) => channel.id === value)?.label
          || this.scopeKindLabel('voice_channel');
      }
      if (field === 'timelineRecordTypes') {
        return this.timelineRecordTypeOptions().find((option) => option.id === value)?.label || value;
      }
      if (field === 'timelineCategories') {
        return this.timelineCategoryRows().find((category) => category.id === value)?.label || value;
      }
      return value;
    },

    timelineDefaultEmpty() {
      return this.timelinePage.loaded
        && this.timelinePage.matched === 0
        && this.filters.timelineCategories === null;
    },

    timelineSearchTerms() {
      return textValue(this.filters.timelineSearch)
        .trim()
        .split(/\s+/)
        .map((term) => term.replace(/^\/+/, '').toLowerCase())
        .filter(Boolean);
    },

    recordMatchesTimelineSearch(recordType, record) {
      const terms = this.timelineSearchTerms();
      if (!terms.length) return true;
      const field = this.filters.timelineSearchField || 'all';
      const values = recordType === 'job'
        ? this.jobTimelineSearchValues(record, field)
        : this.eventTimelineSearchValues(record, field);
      const haystack = values.join(' ').toLowerCase();
      return terms.every((term) => haystack.includes(term));
    },

    eventTimelineSearchValues(event, field) {
      if (field === 'all') {
        return [
          this.eventId(event),
          this.eventTimelineSearchValues(event, 'detail'),
          this.eventTimelineSearchValues(event, 'feedback'),
          this.eventTimelineSearchValues(event, 'kind'),
          this.eventTimelineSearchValues(event, 'job_kind'),
          this.eventTimelineSearchValues(event, 'state'),
          this.eventTimelineSearchValues(event, 'command'),
          this.eventTimelineSearchValues(event, 'room'),
          this.eventTimelineSearchValues(event, 'actor'),
        ].flat().filter(Boolean);
      }
      if (field === 'detail') {
        return [
          event?.text,
          event?.feedback_message,
          event?.reason,
          event?.quality,
          this.eventDetail(event),
          ...this.eventResultSearchValues(event),
        ].filter(Boolean);
      }
      if (field === 'feedback') {
        const kind = this.eventKind(event);
        return kind === 'feedback' ? this.eventTimelineSearchValues(event, 'detail').concat([kind]) : [];
      }
      if (field === 'kind') return [this.eventKind(event)];
      if (field === 'job_kind') return [event?.job_kind].filter(Boolean);
      if (field === 'state') return [event?.state].filter(Boolean);
      if (field === 'command') {
        return [
          event?.command_kind,
          event?.command_name,
          this.slashCommandDetail(event),
        ].filter(Boolean);
      }
      if (field === 'room') {
        return [
          this.eventScopeKind(event),
          event?.guild_slug,
          this.eventGuildId(event),
          this.eventScopeLabel(event),
          this.eventChannelId(event),
          event?.voice_channel_slug,
        ].filter(Boolean);
      }
      if (field === 'actor') {
        return [
          this.eventSpeaker(event),
          event?.speaker_username,
          event?.speaker_user_id,
        ].filter(Boolean);
      }
      return [];
    },

    eventResultSearchValues(event) {
      const values = [];
      ['result', 'command_result', 'command_response'].forEach((key) => {
        const result = event?.[key] || {};
        ['kind', 'status', 'reason', 'action', 'message', 'summary'].forEach((field) => {
          if (result[field] !== undefined && result[field] !== null) values.push(String(result[field]));
        });
      });
      return values;
    },

    jobTimelineSearchValues(job, field) {
      if (field === 'all') {
        return [
          job?.job_id,
          job?.root_job_id,
          job?.parent_job_id,
          this.jobTimelineSearchValues(job, 'detail'),
          this.jobTimelineSearchValues(job, 'kind'),
          this.jobTimelineSearchValues(job, 'job_kind'),
          this.jobTimelineSearchValues(job, 'state'),
          this.jobTimelineSearchValues(job, 'command'),
          this.jobTimelineSearchValues(job, 'room'),
          this.jobTimelineSearchValues(job, 'actor'),
        ].flat().filter(Boolean);
      }
      if (field === 'detail') return [this.jobDetail(job)].filter(Boolean);
      if (field === 'feedback') return [];
      if (field === 'kind') return ['job'];
      if (field === 'job_kind') return [job?.kind].filter(Boolean);
      if (field === 'state') return [job?.state].filter(Boolean);
      if (field === 'command') {
        return [
          this.commandKind(job),
          job?.payload?.command?.command_kind,
          job?.payload?.command?.arguments?.action,
        ].filter(Boolean);
      }
      if (field === 'room') {
        const scopeId = job?.scope_id || '';
        return [
          job?.scope_kind,
          job?.guild_id,
          scopeId,
          this.jobScopeLabel(job),
        ].filter(Boolean);
      }
      if (field === 'actor') return [job?.requested_by_user_id].filter(Boolean);
      return [];
    },

    filteredTimelineEvents(options = {}) {
      const includeGlobal = options.global !== false;
      const kinds = this.effectiveTimelineFilterValues('timelineKinds');
      const states = this.effectiveTimelineFilterValues('timelineJobStates');
      const channels = this.effectiveTimelineFilterValues('timelineChannels');
      const globalKind = includeGlobal ? this.filters.globalEventKind : '';
      const globalChannel = includeGlobal ? this.filters.globalRoom : '';
      const globalGuild = includeGlobal ? this.filters.globalGuild : '';
      const queries = [
        includeGlobal ? this.filters.globalSearch : '',
      ]
        .map((value) => textValue(value).trim().toLowerCase())
        .filter(Boolean);
      return this.timelineEvents.filter((event) => {
        if (!this.timelineTimeMatches(Date.parse(this.eventWhen(event)) || 0)) return false;
        if (kinds !== null && (!kinds.length || (!kinds.includes(this.eventKind(event)) && !kinds.includes(event?.job_kind)))) return false;
        if (states !== null && (!states.length || !states.includes(event?.state))) return false;
        if (globalKind && this.eventKind(event) !== globalKind) return false;
        if (channels !== null && (!channels.length || !channels.includes(this.eventChannelId(event)))) return false;
        if (globalChannel && this.eventChannelId(event) !== globalChannel) return false;
        if (globalGuild && this.eventGuildId(event) !== globalGuild) return false;
        if (!this.recordMatchesTimelineSearch('event', event)) return false;
        if (!queries.length) return true;
        const haystack = [
          this.eventKind(event),
          this.eventGuildId(event),
          this.eventScopeLabel(event),
          this.eventChannelId(event),
          this.eventSpeaker(event),
          this.eventDetail(event),
          this.eventId(event),
        ].join(' ').toLowerCase();
        return queries.every((query) => haystack.includes(query));
      });
    },

    timelinePageRecords() {
      if (!this.timelinePage.loaded) return [];
      return this.timelinePage.records.map((record) => record.recordType === 'event'
        ? this.timelineEventRecord(record.event)
        : this.timelineJobRecord(record.job));
    },

    timelineRecordRows() {
      return this.timelinePageRecords();
    },

    timelineRecordCountLabel() {
      if (this.timelinePage.loaded) {
        const suffix = this.timelinePage.hasMore ? ' (more available)' : '';
        return `${this.timelinePage.returned} of ${this.timelinePage.matched}${suffix}`;
      }
      return 'Loading';
    },

    timelineEventRecord(event) {
      const id = this.eventId(event);
      return {
        rowId: `event:${id}`,
        recordType: 'event',
        recordClass: 'info',
        category: this.timelineFilterDisplay('timelineCategories', event.category),
        when: this.ago(this.eventWhen(event)),
        whenMs: Date.parse(this.eventWhen(event)) || 0,
        eventKind: this.eventKind(event),
        eventClass: this.statusClass(this.eventKind(event)),
        jobKind: event?.job_kind || '',
        jobClass: this.statusClass(event?.job_kind || ''),
        state: event?.state || '',
        stateClass: this.statusClass(event?.state || ''),
        command: event?.command_kind || event?.command_name || '',
        room: this.eventScopeLabel(event),
        actor: this.eventSpeaker(event),
        detail: this.eventDetail(event),
        id,
        __kind: 'event',
        __record: event,
      };
    },

    timelineJobRecord(job) {
      return {
        rowId: `job:${job.job_id}`,
        recordType: 'job',
        recordClass: this.statusClass(job.state),
        category: this.timelineFilterDisplay('timelineCategories', job.category),
        when: this.ago(this.jobTime(job)),
        whenMs: Date.parse(this.jobTime(job)) || 0,
        eventKind: 'job',
        eventClass: 'info',
        jobKind: job.kind,
        jobClass: this.statusClass(job.kind),
        state: job.state,
        stateClass: this.statusClass(job.state),
        command: this.commandKind(job),
        room: this.jobScopeLabel(job),
        actor: job.requestedByLabel || 'Unresolved requester',
        detail: this.jobDetail(job),
        id: job.job_id,
        __kind: 'job',
        __record: job,
      };
    },

    filteredTranscriptEvents() {
      const channel = this.filters.transcriptChannel;
      const queries = [this.filters.transcriptSearch]
        .map((value) => textValue(value).trim().toLowerCase())
        .filter(Boolean);
      return this.transcriptEvents
        .filter((event) => {
          if (!this.transcriptText(event)) return false;
          if (channel && this.eventChannelId(event) !== channel) return false;
          if (!queries.length) return true;
          const haystack = [
            this.transcriptText(event),
            this.eventSpeaker(event),
            this.eventScopeLabel(event),
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
          groups.set(channelId, { channelId, channelName: this.eventScopeLabel(event) || 'Unresolved scope', events: [] });
        }
        groups.get(channelId).events.push(event);
      });
      return Array.from(groups.values());
    },

    selectedAgentTrace() {
      return this.selectedAgentCodex().timeline || [];
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
      return event?.scope_id || event?.scopeId || event?.channelId || event?.voice_channel_id || '';
    },

    eventScopeKind(event) {
      if (event?.scope_kind) return event.scope_kind;
      if (event?.voice_channel_id) return 'voice_channel';
      return '';
    },

    eventScopeLabel(event) {
      const scopeId = this.eventChannelId(event);
      if (!scopeId) return '';
      const scopeKind = this.eventScopeKind(event);
      const resolved = textValue(event?.scopeLabel).trim();
      if (resolved) return resolved;
      if (scopeKind === 'voice_channel') return this.eventChannelName(event);
      if (scopeKind === 'dm') {
        const member = firstText([event?.scope_member_label, event?.requestedByLabel, event?.speakerLabel, event?.speaker_label]);
        return member ? `Direct message with ${member}` : 'Direct message';
      }
      return this.scopeKindLabel(scopeKind);
    },

    eventChannelName(event) {
      return event?.channelName || event?.voice_channel_name || event?.channelSlug || 'Unresolved voice channel';
    },

    eventSpeaker(event) {
      return event?.requestedByLabel || event?.speakerLabel || event?.speaker_label || event?.speaker_username || '';
    },

    eventWhen(event) {
      return event?.startedAt || event?.started_at || event?.created_at || event?.timestamp || '';
    },

    eventId(event) {
      return event?.event_id || event?.eventId || '';
    },

    eventDetail(event) {
      const result = event?.command_result || event?.command_response || event?.result || {};
      return firstText([
        event?.text,
        event?.feedback_message,
        this.slashCommandDetail(event),
        event?.reason,
        event?.job_kind,
        result.reason,
        result.action,
        event?.state,
      ]);
    },

    slashCommandDetail(event) {
      const name = textValue(event?.command_name).trim();
      if (!name) return '';
      const options = this.slashOptionEntries(event?.options).join(', ');
      return [`/${name}`, options].filter(Boolean).join(' ');
    },

    slashOptionEntries(options) {
      if (Array.isArray(options)) {
        return options
          .map((option) => {
            const name = textValue(option?.name).trim();
            const value = this.slashOptionValue(option?.value);
            if (!name) return value;
            return value ? `${name}: ${value}` : name;
          })
          .filter(Boolean);
      }
      if (options && typeof options === 'object') {
        return Object.entries(options)
          .map(([name, value]) => {
            const text = this.slashOptionValue(value);
            return text ? `${name}: ${text}` : name;
          })
          .filter(Boolean);
      }
      const value = this.slashOptionValue(options);
      return value ? [value] : [];
    },

    slashOptionValue(value) {
      if (value === undefined || value === null) return '';
      if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') return String(value);
      if (Array.isArray(value)) return value.map((item) => this.slashOptionValue(item)).filter(Boolean).join(', ');
      if (typeof value === 'object') {
        for (const key of ['String', 'string', 'Integer', 'integer', 'Number', 'number', 'Boolean', 'boolean']) {
          const scalar = value[key];
          if (scalar !== undefined && scalar !== null && textValue(scalar).trim() !== '') return String(scalar);
        }
        if (value.value !== undefined) return this.slashOptionValue(value.value);
        return this.json(value);
      }
      return String(value);
    },

    automationScope(record) {
      const scope = record?.spec?.scope || {};
      const resolved = textValue(scope.scopeLabel).trim();
      if (resolved) return resolved;
      if (scope.scope_kind === 'voice_channel') return this.roomLabel(scope.scope_id || '');
      return this.scopeKindLabel(scope.scope_kind || '');
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
      const resolved = firstText([sink.label, sink.display_name, sink.displayName]);
      if (resolved) return resolved;
      if (id && this.rooms.some((room) => room.channelId === id)) return this.roomLabel(id);
      if (textValue(kind).toLowerCase().includes('user') || textValue(kind).toLowerCase().includes('dm')) return 'Direct message recipient';
      return this.scopeKindLabel(textValue(kind).replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`).replace(/^_/, ''));
    },

    transcriptText(event) {
      return event?.text || event?.text_draft || event?.transcript || '';
    },

    jobTime(job) {
      return job?.updated_at || job?.created_at || job?.started_at || '';
    },

    roomName(room) {
      return room?.channelName || room?.channelSlug || 'Unresolved room';
    },

    roomHumanCount(room) {
      const live = this.liveRoomOccupants(room).length;
      return live || room?.occupancy?.effective_human_count || room?.occupancy?.effectiveHumanCount || '';
    },

    liveRoomOccupants(room) {
      const guildId = room?.guildId || room?.guild_id || '';
      const channelId = room?.channelId || room?.scope_id || '';
      const rooms = this.status().liveOccupancy?.rooms || [];
      const match = rooms.find((entry) => (
        (entry.guild_id || entry.guildId) === guildId
        && (entry.scope_id || entry.scopeId || entry.channelId) === channelId
      ));
      return match?.occupants || [];
    },

    statusClass(value) {
      const text = textValue(value).toLowerCase();
      if (['ok', 'ready', 'present', 'complete', 'queued', 'running', 'waiting', 'active', 'approved', 'capturing', 'idle'].some((part) => text.includes(part))) return 'ok';
      if (['failed', 'error', 'timeout', 'missing', 'degraded', 'down', 'stale'].some((part) => text.includes(part))) return 'bad';
      if (['cancel', 'pending', 'released', 'absent', 'paused', 'truncated', 'unknown'].some((part) => text.includes(part))) return 'warn';
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

    dateTime(iso) {
      if (!iso || !Number.isFinite(Date.parse(iso))) return iso || '';
      return new Date(iso).toLocaleString();
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

    latencyGapLabel(stats) {
      const excluded = stats?.excluded || {};
      const phase = Number(excluded.phaseContaminated || 0);
      const missing = Number(excluded.missingStartedAt || 0);
      const invalid = Number(excluded.invalidTimestampOrder || 0);
      if (!phase && !missing && !invalid) return '0';
      return `${this.int(phase)} phase / ${this.int(missing)} start / ${this.int(invalid)} invalid`;
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

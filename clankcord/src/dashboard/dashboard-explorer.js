(function () {
  function textValue(value) {
    return value === undefined || value === null ? '' : String(value);
  }

  function firstText(values) {
    return values.map(textValue).find((value) => value.trim() !== '') || '';
  }

  const defaultFilters = {
    globalRoom: '',
    globalGuild: '',
    globalJobKind: '',
    globalJobState: '',
    globalRequester: '',
    globalEventKind: '',
    globalSearch: '',
    globalIncludeTerminal: true,
  };

  function initialState() {
    return {
      explorerSelection: { kind: '', record: null },
      renderTimer: null,
    };
  }

  const methods = {
    globalFilterChanged() {
      storeJson(filterStorageKey, this.filters);
      this.scheduleRenderInteractive();
      this.renderExplorerJson();
    },

    clearExploreFilters() {
      Object.assign(this.filters, { ...defaultFilters });
      this.globalFilterChanged();
    },

    applyExploreFilter(key, value) {
      const fields = {
        room: 'globalRoom',
        guild: 'globalGuild',
        jobKind: 'globalJobKind',
        jobState: 'globalJobState',
        requester: 'globalRequester',
        eventKind: 'globalEventKind',
      };
      const field = fields[key];
      if (!field) return;
      this.filters[field] = value || '';
      this.globalFilterChanged();
    },

    scheduleRenderInteractive() {
      if (this.renderTimer) clearTimeout(this.renderTimer);
      this.renderTimer = setTimeout(() => this.renderInteractive(), 0);
    },

    renderInteractive() {
      this.renderTimer = null;
      if (!this.data) return;
      if (this.activeView === 'overview') {
        window.ClankDashboardCharts.render(this);
      }
      if (this.activeView === 'timeline') {
        window.ClankDashboardTables.render(this);
      }
      this.renderExplorerJson();
    },

    renderExplorerJson() {
      if (!window.ClankDashboardJson) return;
      for (const container of [this.$refs?.explorerJson, this.$refs?.timelineJson]) {
        if (container) {
          window.ClankDashboardJson.render(container, this.explorerSelection.record || {}, this.explorerSelection.kind || 'record');
        }
      }
    },

    selectExplorerRecord(kind, record) {
      this.explorerSelection = { kind, record };
      if (kind === 'job') {
        this.selectedJobId = record?.job_id || '';
      }
      this.renderExplorerJson();
    },

    explorerSelectionLabel() {
      return this.explorerSelection.kind ? this.explorerSelectionId() : 'none';
    },

    explorerSelectionId() {
      const record = this.explorerSelection.record || {};
      return record.job_id || record.event_id || record.eventId || record.id || '';
    },

    async copyExplorerJson() {
      if (!this.explorerSelection.record) return;
      await navigator.clipboard.writeText(this.json(this.explorerSelection.record));
    },

    allJobs() {
      const jobs = new Map();
      const add = (job) => {
        if (job?.job_id) jobs.set(job.job_id, job);
      };
      this.activeJobs.forEach(add);
      this.recentJobs.forEach(add);
      this.agentJobs.forEach((entry) => add(entry.job));
      return Array.from(jobs.values()).sort((left, right) => textValue(this.jobTime(right)).localeCompare(textValue(this.jobTime(left))));
    },

    filteredJobs() {
      return this.allJobs().filter((job) => this.jobMatchesExplore(job));
    },

    jobMatchesExplore(job) {
      const filters = this.filters;
      if (!filters.globalIncludeTerminal && this.isTerminalState(job.state)) return false;
      if (filters.globalRoom && job.scope_id !== filters.globalRoom) return false;
      if (filters.globalGuild && job.guild_id !== filters.globalGuild) return false;
      if (filters.globalJobKind && job.kind !== filters.globalJobKind) return false;
      if (filters.globalJobState && job.state !== filters.globalJobState) return false;
      if (filters.globalRequester && job.requested_by_user_id !== filters.globalRequester) return false;
      const query = textValue(filters.globalSearch).trim().toLowerCase();
      if (!query) return true;
      return [
        job.job_id,
        job.root_job_id,
        job.parent_job_id,
        job.scope_kind,
        job.guild_id,
        job.scope_id,
        job.requested_by_user_id,
        job.kind,
        job.state,
        this.commandKind(job),
        this.jobDetail(job),
      ].join(' ').toLowerCase().includes(query);
    },

    isTerminalState(state) {
      return ['complete', 'failed', 'failed_timeout', 'approval_failed', 'failed_draft_retained', 'cancelled', 'canceled'].includes(textValue(state));
    },

    guildOptions() {
      return Array.from(new Set([
        ...this.allJobs().map((job) => job.guild_id),
        ...this.rooms.map((room) => room.guildId),
      ].filter(Boolean))).sort();
    },

    jobKindOptions() {
      return Array.from(new Set(this.allJobs().map((job) => job.kind).filter(Boolean))).sort();
    },

    jobStateOptions() {
      return Array.from(new Set(this.allJobs().map((job) => job.state).filter(Boolean))).sort();
    },

    requesterOptions() {
      return Array.from(new Set(this.allJobs().map((job) => job.requested_by_user_id).filter(Boolean))).sort();
    },

    roomLabel(channelId) {
      const room = this.rooms.find((entry) => entry.channelId === channelId);
      return room ? this.roomName(room) : 'Unresolved voice channel';
    },

    jobScopeLabel(job) {
      const scopeId = job?.scope_id || '';
      if (!scopeId) return '';
      const scopeKind = job?.scope_kind || '';
      const resolved = textValue(job?.scopeLabel).trim();
      if (resolved) return resolved;
      if (scopeKind === 'voice_channel') return this.roomLabel(scopeId);
      if (scopeKind === 'dm') {
        return 'Direct message';
      }
      return this.scopeKindLabel(scopeKind);
    },

    scopeKindLabel(scopeKind) {
      return ({
        dm: 'Direct message',
        voice_channel: 'Voice channel',
        text_channel: 'Text channel',
        thread: 'Thread',
        guild: 'Server',
        global: 'Global',
      })[scopeKind] || textValue(scopeKind).replaceAll('_', ' ') || 'Unscoped';
    },

    selectedExplorerJobLifecycle() {
      if (this.explorerSelection.kind !== 'job') return [];
      const job = this.explorerSelection.record || {};
      const readyAt = firstText([job.ready_at, job.next_run_at, job.created_at]);
      return [
        ['Created', job.created_at],
        ['Ready', readyAt],
        ['Started', job.started_at],
        ['Completed', job.completed_at],
        ['Updated', job.updated_at],
        ['Ready Delay', this.durationBetween(job.created_at, readyAt)],
        ['Queue Time', this.durationBetween(readyAt, job.started_at)],
        ['Run Time', this.durationBetween(job.started_at, job.completed_at || job.updated_at)],
        ['Total', this.durationBetween(job.created_at, job.completed_at || job.updated_at)],
      ]
        .filter(([, value]) => textValue(value).trim() !== '')
        .map(([label, value]) => ({ label, value: textValue(value) }));
    },

    filteredLatencyKindRows() {
      return this.latencyKindRows();
    },

    latencyNumber(row, name, field) {
      const value = Number(row?.[name]?.[field]);
      return Number.isFinite(value) ? value : 0;
    },

    jobMixRows() {
      const rows = new Map();
      for (const aggregate of this.data?.charts?.jobsByKindState || []) {
        const kind = aggregate.kind;
        const state = aggregate.state;
        if (!rows.has(kind)) rows.set(kind, { kind, total: 0, states: {} });
        const row = rows.get(kind);
        const count = Number(aggregate.count || 0);
        row.total += count;
        row.states[state] = count;
      }
      return Array.from(rows.values()).sort((left, right) => right.total - left.total || left.kind.localeCompare(right.kind));
    },

    jobMixStates(rows) {
      const states = new Set();
      rows.forEach((row) => Object.keys(row.states).forEach((state) => states.add(state)));
      const order = ['queued', 'running', 'waiting', 'confirmation_pending', 'cancel_requested', 'complete'];
      return Array.from(states).sort((left, right) => {
        const leftIndex = order.indexOf(left);
        const rightIndex = order.indexOf(right);
        return (leftIndex === -1 ? 99 : leftIndex) - (rightIndex === -1 ? 99 : rightIndex) || left.localeCompare(right);
      });
    },

    eventTrendRows() {
      const charts = this.data?.charts || {};
      const rows = charts.eventsByBucketKind || [];
      const bucketMs = Number(charts.window?.eventBucketSeconds || 0) * 1000;
      const from = Date.parse(charts.window?.from);
      const to = Date.parse(charts.window?.to);
      if (!bucketMs || !Number.isFinite(from) || !Number.isFinite(to)) return { labels: [], series: [] };
      const first = from - (from % bucketMs);
      const last = to - (to % bucketMs);
      const bucketTimes = [];
      for (let at = first; at <= last; at += bucketMs) bucketTimes.push(at);
      const bucketIndexes = new Map(bucketTimes.map((at, index) => [at, index]));
      const kindTotals = new Map();
      const valuesByKind = new Map();
      for (const row of rows) {
        const kind = row.kind;
        const at = Date.parse(row.bucketAt);
        const count = Number(row.count || 0);
        const bucket = bucketIndexes.get(at);
        if (bucket === undefined) continue;
        if (!valuesByKind.has(kind)) valuesByKind.set(kind, Array(bucketTimes.length).fill(0));
        valuesByKind.get(kind)[bucket] += count;
        kindTotals.set(kind, (kindTotals.get(kind) || 0) + count);
      }
      const kinds = Array.from(kindTotals.entries())
        .sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))
        .map(([kind]) => kind);
      return {
        labels: bucketTimes.map((at) => this.clock(new Date(at).toISOString())),
        series: kinds.map((kind) => ({ kind, values: valuesByKind.get(kind) })),
      };
    },

    roomActivityRows() {
      return (this.data?.charts?.scopeActivity || [])
        .map((row) => ({
          channelId: row.scopeId,
          scopeKind: row.scopeKind,
          guildId: row.guildId,
          label: row.scopeLabel,
          jobs: Number(row.jobs || 0),
          speech: Number(row.speech || 0),
          transcripts: Number(row.transcripts || 0),
          wake: Number(row.wake || 0),
          total: Number(row.total || 0),
          latestAt: row.latestAt,
        }))
        .sort((left, right) => right.total - left.total || left.label.localeCompare(right.label));
    },

    jobExplorerRows() {
      return this.filteredJobs().map((job) => ({
        jobId: job.job_id,
        kind: job.kind,
        kindClass: this.statusClass(job.kind),
        state: job.state,
        stateClass: this.statusClass(job.state),
        command: this.commandKind(job),
        room: this.jobScopeLabel(job),
        requester: job.requestedByLabel || 'Unresolved requester',
        attempts: job.attempts ?? 0,
        updatedAgo: this.ago(this.jobTime(job)),
        detail: this.jobDetail(job),
        __record: job,
      }));
    },

    timelineExplorerRows() {
      return this.filteredTimelineEvents().map((event) => ({
        when: this.ago(this.eventWhen(event)),
        kind: this.eventKind(event),
        kindClass: this.statusClass(this.eventKind(event)),
        room: this.eventScopeLabel(event),
        speaker: this.eventSpeaker(event),
        detail: this.eventDetail(event),
        id: this.eventId(event),
        __record: event,
      }));
    },

    exploreCountLabel() {
      return `${this.filteredJobs().length} jobs / ${this.filteredTimelineEvents().length} events`;
    },
  };

  window.ClankDashboardExplorer = { defaultFilters, initialState, methods };
})();

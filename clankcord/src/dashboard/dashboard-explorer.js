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
      if (filters.globalRoom && job.voice_channel_id !== filters.globalRoom) return false;
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
        job.guild_id,
        job.voice_channel_id,
        job.requested_by_user_id,
        job.kind,
        job.state,
        this.commandKind(job),
        this.jobDetail(job),
      ].join(' ').toLowerCase().includes(query);
    },

    isTerminalState(state) {
      return ['complete', 'failed', 'failed_timeout', 'approval_failed', 'agent_dispatch_failed', 'failed_draft_retained', 'cancelled', 'canceled'].includes(textValue(state));
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
      return room ? this.roomName(room) : channelId;
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
      for (const job of this.allJobs()) {
        const kind = job.kind || 'unknown';
        const state = job.state || 'unknown';
        if (!rows.has(kind)) rows.set(kind, { kind, total: 0, states: {} });
        const row = rows.get(kind);
        row.total += 1;
        row.states[state] = (row.states[state] || 0) + 1;
      }
      return Array.from(rows.values()).sort((left, right) => right.total - left.total || left.kind.localeCompare(right.kind)).slice(0, 18);
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
      const events = this.timelineEvents
        .map((event) => ({ event, when: Date.parse(this.eventWhen(event)) }))
        .filter((entry) => Number.isFinite(entry.when))
        .sort((left, right) => left.when - right.when);
      if (!events.length) return { labels: [], series: [] };
      const first = events[0].when;
      const last = events[events.length - 1].when;
      const bucketCount = Math.min(24, Math.max(6, Math.ceil(events.length / 12)));
      const bucketMs = Math.max(60_000, Math.ceil((last - first + 1) / bucketCount));
      const labels = Array.from({ length: bucketCount }, (_, index) => this.clock(new Date(first + index * bucketMs).toISOString()));
      const kindTotals = new Map();
      const buckets = new Map();
      for (const { event, when } of events) {
        const kind = this.eventKind(event);
        const bucket = Math.min(bucketCount - 1, Math.floor((when - first) / bucketMs));
        kindTotals.set(kind, (kindTotals.get(kind) || 0) + 1);
        if (!buckets.has(kind)) buckets.set(kind, Array(bucketCount).fill(0));
        buckets.get(kind)[bucket] += 1;
      }
      const kinds = Array.from(kindTotals.entries())
        .sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))
        .slice(0, 5)
        .map(([kind]) => kind);
      return { labels, series: kinds.map((kind) => ({ kind, values: buckets.get(kind) || Array(bucketCount).fill(0) })) };
    },

    roomActivityRows() {
      const rows = new Map();
      const ensure = (channelId) => {
        const id = channelId || 'unknown';
        if (!rows.has(id)) rows.set(id, { channelId: id, label: this.roomLabel(id), jobs: 0, speech: 0, transcripts: 0, wake: 0 });
        return rows.get(id);
      };
      this.allJobs().forEach((job) => {
        ensure(job.voice_channel_id).jobs += 1;
      });
      this.timelineEvents.forEach((event) => {
        const row = ensure(this.eventChannelId(event));
        const kind = this.eventKind(event);
        if (kind === 'speech_segment') row.speech += 1;
        if (kind === 'transcript') row.transcripts += 1;
        if (kind.startsWith('wake_')) row.wake += 1;
      });
      return Array.from(rows.values())
        .sort((left, right) => (right.jobs + right.speech + right.transcripts + right.wake) - (left.jobs + left.speech + left.transcripts + left.wake) || left.label.localeCompare(right.label))
        .slice(0, 20);
    },

    jobExplorerRows() {
      return this.filteredJobs().map((job) => ({
        jobId: job.job_id,
        kind: job.kind,
        kindClass: this.statusClass(job.kind),
        state: job.state,
        stateClass: this.statusClass(job.state),
        command: this.commandKind(job),
        room: this.roomLabel(job.voice_channel_id),
        requester: job.requested_by_user_id,
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
        room: this.eventChannelName(event),
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

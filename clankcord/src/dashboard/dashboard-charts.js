(function () {
  const charts = new Map();

  function chart(id) {
    const element = document.getElementById(id);
    if (!element) return null;
    if (!window.echarts) throw new Error('ECharts is required by the dashboard');
    if (!charts.has(id)) {
      charts.set(id, window.echarts.init(element, null, { renderer: 'canvas' }));
    }
    return charts.get(id);
  }

  function setChart(id, option, clickHandler) {
    const instance = chart(id);
    if (!instance) return;
    instance.off('click');
    if (clickHandler) instance.on('click', clickHandler);
    instance.setOption(option, true);
  }

  function axisText() {
    return { color: '#a8b1aa', fontSize: 11 };
  }

  function grid() {
    return { top: 28, right: 16, bottom: 44, left: 48 };
  }

  function baseTooltip() {
    return {
      trigger: 'axis',
      backgroundColor: '#181c1b',
      borderColor: '#343b38',
      textStyle: { color: '#eef2ed' },
    };
  }

  function renderJobMix(app) {
    const rows = app.jobMixRows();
    const states = app.jobMixStates(rows);
    const option = {
      color: ['#75aaf8', '#58c98f', '#e0ba5a', '#67c9c1', '#ee776f', '#a8b1aa'],
      tooltip: baseTooltip(),
      legend: { textStyle: axisText(), top: 0, type: 'scroll' },
      grid: grid(),
      xAxis: { type: 'category', data: rows.map((row) => row.kind), axisLabel: axisText() },
      yAxis: { type: 'value', axisLabel: axisText(), splitLine: { lineStyle: { color: '#343b38' } } },
      series: states.map((state) => ({
        name: state,
        type: 'bar',
        stack: 'jobs',
        emphasis: { focus: 'series' },
        data: rows.map((row) => row.states[state] || 0),
      })),
    };
    setChart('job-mix-chart', option, (params) => {
      const row = rows[params.dataIndex];
      if (!row) return;
      app.applyTimelineFilter({
        timelineRecordTypes: ['job'],
        timelineKinds: [row.kind],
        timelineJobStates: [params.seriesName],
      });
    });
  }

  function renderLatency(app) {
    const rows = app.filteredLatencyKindRows();
    const option = {
      color: ['#58c98f', '#e0ba5a', '#ee776f'],
      tooltip: baseTooltip(),
      legend: { textStyle: axisText(), top: 0 },
      grid: grid(),
      xAxis: { type: 'category', data: rows.map((row) => row.kind), axisLabel: { ...axisText(), rotate: 24 } },
      yAxis: { type: 'value', axisLabel: { ...axisText(), formatter: (value) => app.millis(value) }, splitLine: { lineStyle: { color: '#343b38' } } },
      series: [
        { name: 'p50 total', type: 'bar', data: rows.map((row) => app.latencyNumber(row, 'totalMs', 'p50')) },
        { name: 'p95 total', type: 'bar', data: rows.map((row) => app.latencyNumber(row, 'totalMs', 'p95')) },
        { name: 'max total', type: 'bar', data: rows.map((row) => app.latencyNumber(row, 'totalMs', 'max')) },
      ],
    };
    setChart('latency-kind-chart', option, (params) => {
      const row = rows[params.dataIndex];
      if (row) {
        app.applyTimelineFilter({
          timelineRecordTypes: ['job'],
          timelineKinds: [row.kind],
          timelineJobStates: [],
        });
      }
    });
  }

  function renderEvents(app) {
    const trend = app.eventTrendRows();
    const option = {
      color: ['#75aaf8', '#58c98f', '#e0ba5a', '#67c9c1', '#ee776f'],
      tooltip: baseTooltip(),
      legend: { textStyle: axisText(), top: 0, type: 'scroll' },
      grid: grid(),
      dataZoom: [{ type: 'inside' }],
      xAxis: { type: 'category', data: trend.labels, axisLabel: axisText() },
      yAxis: { type: 'value', axisLabel: axisText(), splitLine: { lineStyle: { color: '#343b38' } } },
      series: trend.series.map((series) => ({
        name: series.kind,
        type: 'line',
        smooth: true,
        symbolSize: 5,
        data: series.values,
      })),
    };
    setChart('event-trend-chart', option, (params) => {
      if (params.seriesName) {
        app.applyTimelineFilter({
          timelineRecordTypes: ['event'],
          timelineKinds: [params.seriesName],
          timelineJobStates: [],
        });
      }
    });
  }

  function renderRooms(app) {
    const rows = app.roomActivityRows();
    const option = {
      color: ['#75aaf8', '#58c98f', '#67c9c1', '#e0ba5a'],
      tooltip: baseTooltip(),
      legend: { textStyle: axisText(), top: 0 },
      grid: grid(),
      xAxis: { type: 'category', data: rows.map((row) => row.label), axisLabel: { ...axisText(), rotate: 18 } },
      yAxis: { type: 'value', axisLabel: axisText(), splitLine: { lineStyle: { color: '#343b38' } } },
      series: [
        { name: 'jobs', type: 'bar', data: rows.map((row) => row.jobs) },
        { name: 'speech', type: 'bar', data: rows.map((row) => row.speech) },
        { name: 'transcripts', type: 'bar', data: rows.map((row) => row.transcripts) },
        { name: 'wake', type: 'bar', data: rows.map((row) => row.wake) },
      ],
    };
    setChart('room-activity-chart', option, (params) => {
      const row = rows[params.dataIndex];
      if (row) {
        app.applyTimelineFilter({
          timelineChannels: [row.channelId],
        });
      }
    });
  }

  function render(app) {
    renderJobMix(app);
    renderLatency(app);
    renderEvents(app);
    renderRooms(app);
  }

  window.addEventListener('resize', () => {
    for (const instance of charts.values()) instance.resize();
  });

  window.ClankDashboardCharts = { render };
})();

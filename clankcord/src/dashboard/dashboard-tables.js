(function () {
  const tables = new Map();

  function escapeHtml(value) {
    return String(value ?? '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }

  function ensureTable(id, columns, rowClick) {
    const element = document.getElementById(id);
    if (!element) return null;
    if (!window.Tabulator) throw new Error('Tabulator is required by the dashboard');
    if (!tables.has(id)) {
      const entry = { table: null, built: false, pendingData: null };
      const table = new window.Tabulator(element, {
        data: [],
        columns,
        height: '420px',
        layout: 'fitDataStretch',
        movableColumns: true,
        resizableColumnFit: true,
        persistence: { columns: true, sort: true },
        placeholder: 'No rows match the current filters.',
      });
      table.on('rowClick', rowClick);
      table.on('tableBuilt', () => {
        entry.built = true;
        if (entry.pendingData) {
          table.replaceData(entry.pendingData);
          entry.pendingData = null;
        }
      });
      entry.table = table;
      tables.set(id, entry);
    }
    return tables.get(id);
  }

  function replaceData(entry, rows) {
    if (!entry) return;
    if (entry.built) {
      entry.table.replaceData(rows);
    } else {
      entry.pendingData = rows;
    }
  }

  function pillFormatter(field) {
    return (cell) => {
      const row = cell.getRow().getData();
      return `<span class="pill ${escapeHtml(row[`${field}Class`] || '')}">${escapeHtml(cell.getValue())}</span>`;
    };
  }

  function renderJobs(app) {
    const table = ensureTable('job-explorer-table', [
      { title: 'Job', field: 'jobId', width: 150, frozen: true, headerFilter: 'input' },
      { title: 'Kind', field: 'kind', width: 150, formatter: pillFormatter('kind'), headerFilter: 'list', headerFilterParams: { valuesLookup: true, clearable: true } },
      { title: 'State', field: 'state', width: 150, formatter: pillFormatter('state'), headerFilter: 'list', headerFilterParams: { valuesLookup: true, clearable: true } },
      { title: 'Command', field: 'command', width: 150, headerFilter: 'input' },
      { title: 'Room', field: 'room', width: 180, headerFilter: 'input' },
      { title: 'Requester', field: 'requester', width: 160, headerFilter: 'input' },
      { title: 'Attempts', field: 'attempts', width: 100, hozAlign: 'right', sorter: 'number' },
      { title: 'Updated', field: 'updatedAgo', width: 115 },
      { title: 'Detail', field: 'detail', minWidth: 360, formatter: 'textarea', headerFilter: 'input' },
    ], (_event, row) => app.selectExplorerRecord('job', row.getData().__record));
    replaceData(table, app.jobExplorerRows());
  }

  function renderTimeline(app) {
    const table = ensureTable('timeline-explorer-table', [
      { title: 'When', field: 'when', width: 115, frozen: true },
      { title: 'Event', field: 'kind', width: 190, formatter: pillFormatter('kind'), headerFilter: 'list', headerFilterParams: { valuesLookup: true, clearable: true } },
      { title: 'Room', field: 'room', width: 180, headerFilter: 'input' },
      { title: 'Speaker', field: 'speaker', width: 170, headerFilter: 'input' },
      { title: 'Detail', field: 'detail', minWidth: 420, formatter: 'textarea', headerFilter: 'input' },
      { title: 'Id', field: 'id', width: 180, headerFilter: 'input' },
    ], (_event, row) => app.selectExplorerRecord('event', row.getData().__record));
    replaceData(table, app.timelineExplorerRows());
  }

  function render(app) {
    renderJobs(app);
    renderTimeline(app);
  }

  window.ClankDashboardTables = { render };
})();

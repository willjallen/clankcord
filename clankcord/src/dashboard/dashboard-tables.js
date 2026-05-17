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

  function ensureTable(id, columns, rowClick, options = {}) {
    const element = document.getElementById(id);
    if (!element) return null;
    if (!window.Tabulator) throw new Error('Tabulator is required by the dashboard');
    if (!tables.has(id)) {
      const entry = { table: null, element, built: false, pendingData: null, sort: options.initialSort || [] };
      const table = new window.Tabulator(element, {
        data: [],
        columns,
        height: '420px',
        index: 'rowId',
        layout: 'fitColumns',
        movableColumns: true,
        resizableColumnFit: true,
        persistence: { columns: true },
        placeholder: 'No rows match the current filters.',
        ...options,
      });
      table.on('rowClick', rowClick);
      table.on('tableBuilt', () => {
        entry.built = true;
        if (entry.pendingData) {
          table.replaceData(entry.pendingData);
          entry.pendingData = null;
        }
        if (entry.sort.length) table.setSort(entry.sort);
      });
      entry.table = table;
      tables.set(id, entry);
    }
    return tables.get(id);
  }

  function replaceData(entry, rows) {
    if (!entry) return;
    if (entry.built) {
      const holder = entry.element.querySelector('.tabulator-tableholder');
      const scroll = holder ? { left: holder.scrollLeft, top: holder.scrollTop } : null;
      const replaced = entry.table.replaceData(rows);
      const restore = () => {
        if (entry.sort.length) entry.table.setSort(entry.sort);
        const nextHolder = entry.element.querySelector('.tabulator-tableholder');
        if (nextHolder && scroll) {
          nextHolder.scrollLeft = scroll.left;
          nextHolder.scrollTop = scroll.top;
        }
      };
      if (replaced && typeof replaced.then === 'function') {
        replaced.then(() => requestAnimationFrame(restore));
      } else {
        requestAnimationFrame(restore);
      }
    } else {
      entry.pendingData = rows;
    }
  }

  function pillFormatter(field, classField = `${field}Class`) {
    return (cell) => {
      const row = cell.getRow().getData();
      return `<span class="pill ${escapeHtml(row[classField] || '')}">${escapeHtml(cell.getValue())}</span>`;
    };
  }

  function renderUnifiedTimeline(app) {
    const table = ensureTable('unified-timeline-table', [
      { title: 'When', field: 'whenMs', width: 95, frozen: true, sorter: 'number', formatter: (cell) => escapeHtml(cell.getRow().getData().when) },
      { title: 'Record', field: 'recordType', width: 85, headerSort: false, formatter: pillFormatter('recordType', 'recordClass') },
      { title: 'Event', field: 'eventKind', width: 165, headerSort: false, formatter: pillFormatter('eventKind', 'eventClass') },
      { title: 'Job Type', field: 'jobKind', width: 165, headerSort: false, formatter: pillFormatter('jobKind', 'jobClass') },
      { title: 'State', field: 'state', width: 105, headerSort: false, formatter: pillFormatter('state') },
      { title: 'Command', field: 'command', width: 105, headerSort: false },
      { title: 'Scope', field: 'room', width: 150, headerSort: false },
      { title: 'Actor', field: 'actor', width: 150, headerSort: false },
      { title: 'Detail', field: 'detail', minWidth: 220, widthGrow: 2, headerSort: false, formatter: 'textarea' },
      { title: 'Id', field: 'id', width: 170, headerSort: false },
    ], (_event, row) => {
      const data = row.getData();
      app.selectExplorerRecord(data.__kind, data.__record);
    }, {
      height: '620px',
      initialSort: [{ column: 'whenMs', dir: 'desc' }],
    });
    table.sort = [{ column: 'whenMs', dir: 'desc' }];
    replaceData(table, app.timelineRecordRows());
  }

  function render(app) {
    renderUnifiedTimeline(app);
  }

  window.ClankDashboardTables = { render };
})();

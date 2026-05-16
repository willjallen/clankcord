(function () {
  function escapeHtml(value) {
    return String(value)
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }

  function valueLabel(value) {
    if (Array.isArray(value)) return `Array(${value.length})`;
    if (value && typeof value === 'object') return `Object(${Object.keys(value).length})`;
    if (value === null) return 'null';
    return JSON.stringify(value);
  }

  function scalarNode(key, value) {
    const row = document.createElement('div');
    row.className = 'json-row';
    row.innerHTML = `<span class="json-key">${escapeHtml(key)}</span><span class="json-scalar">${escapeHtml(valueLabel(value))}</span>`;
    return row;
  }

  function treeNode(key, value, depth) {
    if (!value || typeof value !== 'object') return scalarNode(key, value);

    const details = document.createElement('details');
    details.className = 'json-node';
    details.open = depth < 2;

    const summary = document.createElement('summary');
    summary.innerHTML = `<span class="json-key">${escapeHtml(key)}</span><span class="json-summary">${escapeHtml(valueLabel(value))}</span>`;
    details.appendChild(summary);

    const children = document.createElement('div');
    children.className = 'json-children';
    const entries = Array.isArray(value)
      ? value.map((entry, index) => [String(index), entry])
      : Object.entries(value);

    for (const [childKey, childValue] of entries) {
      children.appendChild(treeNode(childKey, childValue, depth + 1));
    }
    details.appendChild(children);
    return details;
  }

  function render(container, value, label = 'record') {
    if (!container) return;
    container.replaceChildren(treeNode(label, value, 0));
  }

  window.ClankDashboardJson = { render };
})();

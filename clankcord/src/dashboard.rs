pub const DASHBOARD_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Clawcord Voice Debug</title>
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; background: #101214; color: #e9edf2; }
    body { margin: 0; }
    header { position: sticky; top: 0; padding: 14px 18px; border-bottom: 1px solid #303640; background: rgba(16,18,20,.94); }
    h1 { font-size: 18px; margin: 0; }
    main { padding: 16px 18px; display: grid; gap: 12px; }
    button, input { background: #1f2329; color: #e9edf2; border: 1px solid #303640; border-radius: 6px; padding: 7px 9px; }
    pre { margin: 0; padding: 12px; border: 1px solid #303640; border-radius: 8px; background: #171a1e; overflow: auto; min-height: 320px; }
    .row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  </style>
</head>
<body>
  <header><h1>Clawcord Voice Debug</h1></header>
  <main>
    <div class="row">
      <button id="refresh">Refresh</button>
      <input id="since" value="-1h" aria-label="Since">
    </div>
    <pre id="output">Loading...</pre>
  </main>
  <script>
    async function load() {
      const since = encodeURIComponent(document.getElementById('since').value || '-1h');
      const response = await fetch('/v1/voice/debug/overview?since=' + since);
      document.getElementById('output').textContent = JSON.stringify(await response.json(), null, 2);
    }
    document.getElementById('refresh').addEventListener('click', load);
    load().catch(error => { document.getElementById('output').textContent = String(error); });
  </script>
</body>
</html>
"#;

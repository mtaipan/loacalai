"""Restart an idle older local Studio backend before launching the new desktop."""
import json
import os
import time
import urllib.request
import urllib.error

base = os.environ.get('LOCAL_GEMMA_BOOTSTRAP_URL', 'http://127.0.0.1:3210')
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
try:
    with http.open(base + '/api/bootstrap', timeout=2) as response:
        app = json.load(response)
except (urllib.error.URLError, TimeoutError):
    raise SystemExit(0)
if app.get('app') != 'Local Gemma Studio':
    raise SystemExit('Another app is using the Studio port.')
from pathlib import Path
import re
expected = re.search(r'^version\s*=\s*"([^"]+)"', (Path(__file__).resolve().parent.parent / 'Cargo.toml').read_text(), re.M)[1]
if app.get('version') == expected:
    raise SystemExit(0)
with http.open(base + '/api/status', timeout=5) as response:
    status = json.load(response)
if status.get('active_tasks', 0) or status.get('quick_busy', False):
    raise SystemExit('Finish or stop the current task, then restart Studio to update.')
request = urllib.request.Request(base + '/api/shutdown', data=b'{}', headers={
    'Content-Type': 'application/json', 'X-Studio-Token': app['token']})
http.open(request, timeout=5).close()
for _ in range(50):
    time.sleep(.1)
    try:
        http.open(base + '/api/bootstrap', timeout=.5).close()
    except (urllib.error.URLError, TimeoutError):
        break
else:
    raise SystemExit('Close the old Studio app and launch again.')
print('Older Studio backend stopped; chats preserved.')

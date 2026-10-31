import json
import os
import urllib.request

base = os.environ.get('LOCAL_GEMMA_BOOTSTRAP_URL', 'http://127.0.0.1:3210')
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
with opener.open(base + '/api/bootstrap') as response:
    token = json.load(response)['token']
with opener.open(base + '/api/status') as response:
    names = {m['name'] for m in (json.load(response).get('models') or [])}
if not {'translategemma:4b', 'qwen2.5:3b'}.issubset(names):
    request = urllib.request.Request(base + '/api/models/pull', data=b'{}',
        headers={'Content-Type': 'application/json', 'X-Studio-Token': token})
    previous = ''
    with opener.open(request, timeout=7200) as response:
        for line in response:
            if not line.startswith(b'data:'):
                continue
            event = json.loads(line[5:])
            if event['type'] == 'error':
                raise RuntimeError(event['message'])
            if event['type'] == 'progress':
                progress = event['progress']
                text = event['model'] + ': ' + progress.get('status', '')
                if progress.get('total'):
                    text += f" {100 * progress.get('completed', 0) // progress['total']}%"
                if text != previous:
                    print(text, flush=True)
                    previous = text
print('Language models ready.')

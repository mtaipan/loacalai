import json
import os
import urllib.request
base = os.environ.get('LOCAL_GEMMA_BOOTSTRAP_URL', 'http://127.0.0.1:3210')
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
try:
    with opener.open(base + '/api/bootstrap', timeout=2) as response:
        app = json.load(response)
    if app['app'] != 'Local Gemma Studio':
        raise RuntimeError('Unexpected server')
    request = urllib.request.Request(base + '/api/shutdown', data=b'{}', headers={'Content-Type':'application/json','X-Studio-Token':app['token']})
    opener.open(request, timeout=5).close()
    print('Local Gemma Studio server stopped. Ollama unloads idle models automatically.')
except Exception as exc:
    print('Server is not running or could not be stopped:', exc)

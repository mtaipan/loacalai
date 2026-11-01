"""Integration test: fake Ollama, real Rust server, no network/model downloads.
Usage: runtime/python/python.exe scripts/tests/test_model_storage.py
"""
from pathlib import Path
import json
import os
import socket
import subprocess
import sys
import time
import urllib.request
import uuid

project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('storage-test-'+uuid.uuid4().hex)
root.mkdir(parents=True)
(root/'serve').write_text('''import os,json
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
store=Path(os.environ['OLLAMA_MODELS']);store.mkdir(parents=True,exist_ok=True)
(store/'started.txt').write_text('started')
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_GET(self):self.handle_request()
 def do_POST(self):self.handle_request()
 def do_DELETE(self):self.handle_request()
 def handle_request(self):
  body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))) or '{}')
  marker=store/'fake-models.json';models=json.loads(marker.read_text()) if marker.exists() else []
  if self.path=='/api/pull':
   if Path('partial-pull').exists():result={'status':'downloading','completed':10,'total':100}
   else:models.append(body['model']);marker.write_text(json.dumps(models));result={'status':'success'}
  elif self.path=='/api/delete':
   models.remove(body['model']);marker.write_text(json.dumps(models));result={}
  elif self.path=='/api/tags':result={'models':[{'name':m,'model':m} for m in models]}
  elif self.path=='/api/show':result={'capabilities':['completion','tools'],'thinking':{'values':[False]}}
  elif self.path=='/api/ps':result={'models':[]}
  else:result={'version':'0.40.13'}
  data=json.dumps(result).encode()+b'\\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
host,port=os.environ['OLLAMA_HOST'].rsplit(':',1)
ThreadingHTTPServer((host,int(port)),Handler).serve_forever()
''',encoding='utf-8')
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
base='http://127.0.0.1:'+str(port())
env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':base.rsplit(':',1)[1],
     'LOCAL_GEMMA_ENGINE_PORT':str(port()),'LOCAL_GEMMA_OLLAMA_BIN':sys.executable}
env.pop('LOCAL_GEMMA_OLLAMA',None)
http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
log=(root/'test.log').open('w',encoding='utf-8')
process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env=env,stdout=log,stderr=log)
def call(path,body=None):
    headers={'Content-Type':'application/json'}
    if body is not None:headers['X-Studio-Token']=token
    return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers=headers),timeout=45)
def read(path,body=None):
    with call(path,body) as r:return json.load(r)
try:
    for _ in range(100):
        try:app=read('/api/bootstrap');break
        except Exception:time.sleep(.15)
    else:raise RuntimeError('Server startup failed')
    token=app['token']
    ids=['hf.co/noctrex/SERA-8B-GGUF:Q4_K_M','qwen3-embedding:0.6b']
    folders=[root/'disk-one',root/'disk-two']
    for model,folder in zip(ids,folders):
        with call('/api/models/pull',{'model':model,'directory':str(folder)}) as r:
            events=[json.loads(l[5:]) for l in r if l.startswith(b'data:')]
        assert not any(e['type']=='error' for e in events),events
        assert json.loads((folder/'fake-models.json').read_text())==[model]
    status=read('/api/status')
    assert len(status['models'])==2,status
    assert {m['storage_path'] for m in status['models']}==set(map(str,folders)),status
    assert not (root/'data/models/fake-models.json').exists()
    try:read('/api/models/pull',{'model':ids[0],'directory':str(root/'third-disk')})
    except urllib.error.HTTPError as e:assert e.code==400
    else:raise AssertionError('Moving an installed model must not silently duplicate it')
    read('/api/models/delete',{'model':ids[0]})
    assert json.loads((folders[0]/'fake-models.json').read_text())==[]
    assert json.loads((folders[1]/'fake-models.json').read_text())==[ids[1]]
    assert ids[0] not in read('/api/status')['model_locations']
    third=next(m['id'] for m in app['catalog'] if m['role']=='assistant' and m['id'] not in ids)
    cold=root/'cold-store';cold.mkdir()
    mapping=root/'data/model-locations.json'
    locations=json.loads(mapping.read_text());locations[third]=str(cold);mapping.write_text(json.dumps(locations))
    read('/api/status')
    assert not (cold/'started.txt').exists(), 'status must not launch a cold store'
    (root/'partial-pull').write_text('truncate stream')
    with call('/api/models/pull',{'model':third,'directory':str(cold)}) as r:
        events=[json.loads(l[5:]) for l in r if l.startswith(b'data:')]
    assert any(e['type']=='error' for e in events), 'EOF without success was incorrectly accepted'
    (root/'partial-pull').unlink()
    replacement=root/'replacement-store'
    with call('/api/models/pull',{'model':third,'directory':str(replacement)}) as r:
        events=[json.loads(l[5:]) for l in r if l.startswith(b'data:')]
    assert not any(e['type']=='error' for e in events),events
    assert read('/api/status')['model_locations'][third]==str(replacement)
    print('PASS passive cold-store status, incomplete download detection and destination change after failure')
    print('PASS separate destinations, aggregated tags, persisted mapping, no central copies, deletion isolation, relocation rejection; no model weights downloaded')
finally:
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=15)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=10)
    owner=root/'data/engine-owner.json'
    if owner.exists():
        import signal
        try:os.kill(json.loads(owner.read_text())['pid'],signal.SIGTERM)
        except OSError:pass
    log.close()

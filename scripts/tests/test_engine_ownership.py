"""Fake private engines: distinct homes, explicit occupied port, owned restart."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,sys,threading,time,urllib.request,urllib.error,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('engine-owner-test-'+uuid.uuid4().hex[:8]);root.mkdir(parents=True)
http=urllib.request.build_opener(urllib.request.ProxyHandler({}));processes=[];logs=[]
class Unowned(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        body=b'{"version":"fixture"}';self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
unowned=ThreadingHTTPServer(('127.0.0.1',0),Unowned);threading.Thread(target=unowned.serve_forever,daemon=True).start()
helper='''import os,json
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  data=json.dumps({'version':'fixture','models':[]}).encode();self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
host,port=os.environ['OLLAMA_HOST'].rsplit(':',1)
ThreadingHTTPServer((host,int(port)),H).serve_forever()
'''
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def call(url,path,token='',body=None):
    with http.open(urllib.request.Request(url+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=8) as response:return json.load(response)
def start(name,engine_port=None):
    home=root/name;home.mkdir();(home/'serve').write_text(helper,encoding='utf-8');url=f'http://127.0.0.1:{port()}'
    env={**os.environ,'LOCAL_GEMMA_HOME':str(home),'LOCAL_GEMMA_PORT':url.rsplit(':',1)[1],'LOCAL_GEMMA_OLLAMA_BIN':sys.executable};env.pop('LOCAL_GEMMA_OLLAMA',None);env.pop('LOCAL_GEMMA_ENGINE_PORT',None)
    if engine_port:env['LOCAL_GEMMA_ENGINE_PORT']=str(engine_port)
    log=(home/'test.log').open('w');logs.append(log);process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=home,env=env,stdout=log,stderr=log);processes.append((process,url))
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((home/'test.log').read_text(encoding='utf-8'))
        try:bootstrap=call(url,'/api/bootstrap');return home,url,bootstrap['token']
        except Exception:time.sleep(.05)
    raise AssertionError('server did not start')
def owned(home):
    for _ in range(300):
        try:return json.loads((home/'data/engine-owner.json').read_text(encoding='utf-8'))
        except FileNotFoundError:time.sleep(.05)
    raise AssertionError((home/'test.log').read_text(encoding='utf-8'))
try:
    blocked,url,token=start('blocked',unowned.server_port)
    try:call(url,'/api/compute/retry',token,{})
    except urllib.error.HTTPError as error:
        assert error.code==400 and 'не владеет' in json.load(error)['error']
    else:raise AssertionError('unowned engine was silently adopted')
    assert not (blocked/'data/engine-owner.json').exists()
    assert call(f'http://127.0.0.1:{unowned.server_port}','/api/version')['version']=='fixture'
    call(url,'/api/shutdown',token,{});processes[-1][0].wait(timeout=8)
    first,url1,token1=start('first');owner1=owned(first)
    second,url2,token2=start('second');owner2=owned(second)
    assert owner1['port']!=owner2['port'] and owner1['pid']!=owner2['pid']
    call(url1,'/api/compute/retry',token1,{})
    assert call(f"http://127.0.0.1:{owner2['port']}",'/api/version')['version']=='fixture'
    assert owned(second)['pid']==owner2['pid']
    print('PASS explicit occupied engine port is rejected without adoption/termination; separate homes get distinct private ports and restart only the owned engine')
finally:
    for process,url in processes:
        if process.poll() is None:
            try:call(url,'/api/shutdown',call(url,'/api/bootstrap')['token'],{})
            except Exception:pass
            try:process.wait(timeout=8)
            except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    unowned.shutdown()
    for log in logs:log.close()

"""Real second launch/crash recovery against isolated homes and blocked fake inference."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,urllib.error,uuid
project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('instance-test-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
exe=project/'target/release/local-gemma-studio.exe';model='qwen2.5:3b'
started=threading.Event();release=threading.Event();http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n'
        try:
            self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except ConnectionError:pass
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':32768},'thinking':{'values':[False]}})
        if self.path=='/api/chat' and body.get('messages'):
            started.set();release.wait(30)
            return self.reply({'message':{'role':'assistant','content':'INSTANCE_COMPLETE'},'done':True})
        self.reply({'done':True,'status':'success'})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
app_port=port();base=f'http://127.0.0.1:{app_port}'
env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(app_port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'}
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w');process=None
def start():return subprocess.Popen([str(exe),'--server'],cwd=root,env=env,stdout=log,stderr=log)
def read(path,body=None):
    with http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=5) as response:return json.load(response)
def ready():
    global token
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('server did not start')
try:
    process=start();ready();chat=read('/api/chats',{});run=str(uuid.uuid4())
    read('/api/runs',{'request_id':run,'conversation':chat['id'],'content':'INSTANCE_FIXTURE inspect this request','mode':'agent','web':'off','attachments':[],'source':'en','target':'ru'})
    assert started.wait(8),'inference did not start'
    before=read('/api/runs/'+run);assert before['status']=='running',before
    for requested_port in [app_port,port()]:
        duplicate=subprocess.run([str(exe),'--server'],cwd=root,env={**env,'LOCAL_GEMMA_PORT':str(requested_port)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=12)
        assert duplicate.returncode==0,duplicate.stderr
        after=read('/api/runs/'+run);assert after['status']=='running' and after['input']==before['input'],after
        assert read('/api/session')['runs'][0]['id']==run
    foreign=root/'different-home';foreign.mkdir()
    collision=subprocess.run([str(exe),'--server'],cwd=foreign,env={**env,'LOCAL_GEMMA_HOME':str(foreign)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=12)
    assert collision.returncode!=0,'a different home must not adopt this server'
    assert not (foreign/'data/tasks.sqlite3').exists(),'collision ran recovery before ownership check'
    assert read('/api/runs/'+run)['status']=='running'
    print('PASS same-home second launch (same/different port) preserves an active run; different-home port collision never opens its database')
    process.kill();process.wait(timeout=5)
    process=start();ready();state=read('/api/runs/'+run)
    assert state['status']=='interrupted',state
    assert read('/api/chats/'+chat['id'])['id']==chat['id']
    print('PASS OS lease releases after crash; only the replacement owner performs interrupted-run recovery')
finally:
    release.set()
    if process and process.poll() is None:
        try:read('/api/shutdown',{})
        except Exception:pass
        try:process.wait(timeout=8)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()

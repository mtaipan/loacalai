"""Settings/runtime failure and disconnect scenarios; isolated fake engine only."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import copy,ctypes,json,os,socket,subprocess,threading,time,urllib.request,urllib.error,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('settings-transaction-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
settings_path=root/'data/settings.json';fallback_path=root/'data/compute-fallback.json'
settings_path.write_text(json.dumps({'workspace':str(root/'data/workspace'),'theme':'dark','source':'en','target':'ru','cpu_only':True}))
fallback=b'{"key":"OLD_FIXTURE_KEY","error":"OLD_FIXTURE_ERROR"}'
fallback_path.write_bytes(fallback)
health_calls=0;failures=0;block=False;entered=threading.Event();release=threading.Event()
class Engine(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        global health_calls,failures
        status=200
        if self.path=='/api/version':
            health_calls+=1
            if block:entered.set();release.wait(10)
            if failures:failures-=1;status=503
        data=json.dumps({'version':'fixture','models':[]}).encode();self.send_response(status);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';token='';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));log=(root/'test.log').open('w')
process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):
    with http.open(urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=20) as response:return json.load(response)
def rejected(value,status=None):
    try:call('/api/settings',value);raise AssertionError('Unexpected save')
    except urllib.error.HTTPError as error:
        if status is not None:assert error.code==status,error.read()
        return json.load(error)['error']
def unchanged(prior,old):
    assert call('/api/bootstrap')['settings']==prior
    assert settings_path.read_bytes()==old
    assert fallback_path.read_bytes()==fallback
    assert not list((root/'data').glob('.local-gemma-*.tmp'))
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(errors='replace'))
        try:token=call('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    else:raise AssertionError('Own server did not start')
    prior=call('/api/bootstrap')['settings'];old=settings_path.read_bytes();value=copy.deepcopy(prior);value['compute']['fallback_to_cpu']=not value['compute']['fallback_to_cpu']
    invalid=copy.deepcopy(value);invalid['workspace']='relative-folder';before=health_calls
    rejected(invalid,400);unchanged(prior,old);assert health_calls==before
    invalid=copy.deepcopy(value);invalid['compute']['backend']='cuda'
    rejected(invalid,400);unchanged(prior,old);assert health_calls==before
    failures=1;message=rejected(value);assert 'не сохранены' in message,message
    unchanged(prior,old);assert health_calls>=before+2
    print('PASS invalid settings do not reconfigure; failed engine transition restores persisted fallback and settings')
    if os.name=='nt':
        kernel=ctypes.WinDLL('kernel32',use_last_error=True)
        kernel.CreateFileW.argtypes=[ctypes.c_wchar_p,ctypes.c_uint32,ctypes.c_uint32,ctypes.c_void_p,ctypes.c_uint32,ctypes.c_uint32,ctypes.c_void_p];kernel.CreateFileW.restype=ctypes.c_void_p
        kernel.CloseHandle.argtypes=[ctypes.c_void_p];kernel.CloseHandle.restype=ctypes.c_int
        handle=kernel.CreateFileW(str(settings_path),0x80000000,1,None,3,0x80,None)
        assert handle not in (None,ctypes.c_void_p(-1).value),ctypes.get_last_error()
        try:
            before=health_calls;rejected(value);unchanged(prior,old);assert health_calls>=before+2
        finally:assert kernel.CloseHandle(handle)
        print('PASS Windows replacement failure preserves old file, restores fallback/runtime and removes staged file')
    block=True;entered.clear();release.clear();data=json.dumps(value).encode()
    connection=socket.create_connection(('127.0.0.1',port));connection.sendall((f'POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Studio-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {len(data)}\r\n\r\n').encode()+data)
    assert entered.wait(10);connection.close();release.set();block=False
    for _ in range(100):
        saved=call('/api/bootstrap')['settings']
        if saved['revision']==prior['revision']+1:break
        time.sleep(.05)
    else:raise AssertionError('Accepted save was abandoned after disconnect')
    assert saved['compute']==value['compute'] and json.loads(settings_path.read_bytes())==saved
    rejected(value,409);assert call('/api/bootstrap')['settings']==saved
    assert not list((root/'data').glob('.local-gemma-*.tmp'))
    print('PASS accepted settings transition finishes after HTTP disconnect; stale revision cannot overwrite committed settings')
finally:
    release.set()
    if process.poll() is None:
        try:call('/api/shutdown',{})
        except Exception:pass
        try:process.wait(timeout=8)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()

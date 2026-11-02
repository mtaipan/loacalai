"""Sidebar listing must not deserialize a large legacy conversation."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import ctypes,json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('chat-headers-'+uuid.uuid4().hex[:8]);(root/'data/chats').mkdir(parents=True);(root/'data/workspace').mkdir()
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'theme':'dark','source':'en','target':'ru','cpu_only':True}))
id=str(uuid.uuid4());path=root/'data/chats'/f'{id}.json'
with path.open('wb') as file:
    file.write(json.dumps({'id':id,'title':'Large legacy fixture','updated':'2026-11-02','mode':'chat'}).encode()[:-1]+b',"messages":[{"id":"one","role":"user","created":"now","content":"')
    chunk=b'x'*65536
    for _ in range(512):file.write(chunk)
    file.write(b'"},{"id":"two","role":"assistant","created":"now","content":"second"}],"history":[{"role":"user","content":"')
    for _ in range(512):file.write(chunk)
    file.write(b'"}]}')
class Engine(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        data=b'{"version":"fixture","models":[]}';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token='';log=(root/'test.log').open('w')
process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):
    with http.open(urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30) as response:return json.load(response)
def peak():
    if os.name!='nt':return None
    class Counters(ctypes.Structure):_fields_=[('cb',ctypes.c_uint32),('faults',ctypes.c_uint32)]+[(key,ctypes.c_size_t) for key in ['peak','working','page_peak','page','nonpage_peak','nonpage','pagefile','pagefile_peak']]
    counter=Counters();counter.cb=ctypes.sizeof(counter);psapi=ctypes.WinDLL('psapi',use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_uint32];psapi.GetProcessMemoryInfo.restype=ctypes.c_int
    assert psapi.GetProcessMemoryInfo(int(process._handle),ctypes.byref(counter),counter.cb)
    return counter.peak
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(errors='replace'))
        try:token=call('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    else:raise AssertionError('Own server did not start')
    before=peak()
    for _ in range(3):
        items=call('/api/chats');assert items==[{'id':id,'title':'Large legacy fixture','updated':'2026-11-02','mode':'chat','count':2}],items
    after=peak()
    if before is not None:
        growth=after-before;assert growth<48*1024*1024,{'peak_growth':growth,'source_bytes':path.stat().st_size}
        print(f'PASS 64 MiB legacy chat lists metadata only; own server peak grew {growth/1024/1024:.2f} MiB')
    view=call('/api/chats/'+id);assert 'history' not in view and 'history_model' not in view
    assert len(view['messages'][0]['content'])==32*1024*1024 and view['messages'][1]['content']=='second'
    assert path.stat().st_size>64*1024*1024
    print('PASS browser conversation view excludes working model history while retaining full visible messages and original file')
    new=call('/api/chats',{});assert len(call('/api/chats'))==2
    request=urllib.request.Request(base+'/api/chats/'+id,method='DELETE',headers={'X-Studio-Token':token})
    with http.open(request):pass
    assert call('/api/chats')[0]['id']==new['id'] and not path.exists()
    print('PASS repeated sidebar refresh, new chat and large legacy deletion preserve expected metadata')
finally:
    if process.poll() is None:
        try:call('/api/shutdown',{})
        except Exception:pass
        try:process.wait(timeout=8)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()

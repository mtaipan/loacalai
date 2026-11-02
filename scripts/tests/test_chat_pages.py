"""Bounded browser history, revision fences and full explicit export; isolated data."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import ctypes,json,os,socket,subprocess,threading,time,urllib.request,urllib.error,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('chat-pages-'+uuid.uuid4().hex[:8]);(root/'data/chats').mkdir(parents=True);(root/'data/workspace').mkdir()
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'theme':'dark','source':'en','target':'ru','cpu_only':True}))
id=str(uuid.uuid4());path=root/'data/chats'/f'{id}.json';count=320
with path.open('wb') as file:
    file.write(json.dumps({'id':id,'title':'Paged history fixture','updated':'2026-11-02','mode':'chat'}).encode()[:-1]+b',"messages":[')
    for i in range(count):
        if i:file.write(b',')
        file.write(json.dumps({'id':str(i),'role':'user','created':'2026-11-02','content':f'VISIBLE_MARKER_{i:04d} '+('x'*16384)}).encode()[:-1])
        if i==count-1:
            file.write(b',"material":"')
            for _ in range(512):file.write(b'M'*65536)
            file.write(b'"')
        file.write(b'}')
    file.write(b'],"history":[{"role":"user","content":"')
    for _ in range(512):file.write(b'H'*65536)
    file.write(b'"}]}')
class Engine(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        body=b'{"version":"fixture","models":[]}';self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token='';log=(root/'test.log').open('w')
process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(route,body=None):
    with http.open(urllib.request.Request(base+route,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30) as response:return json.load(response)
def peak():
    if os.name!='nt':return None
    class Counters(ctypes.Structure):_fields_=[('cb',ctypes.c_uint32),('faults',ctypes.c_uint32)]+[(key,ctypes.c_size_t) for key in ['peak','working','page_peak','page','nonpage_peak','nonpage','pagefile','pagefile_peak']]
    counter=Counters();counter.cb=ctypes.sizeof(counter);psapi=ctypes.WinDLL('psapi',use_last_error=True);psapi.GetProcessMemoryInfo.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_uint32];psapi.GetProcessMemoryInfo.restype=ctypes.c_int
    assert psapi.GetProcessMemoryInfo(int(process._handle),ctypes.byref(counter),counter.cb);return counter.peak
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(errors='replace'))
        try:token=call('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    else:raise AssertionError('Own server did not start')
    before=peak();latest=call('/api/chats/'+id+'/messages');p=latest['message_page'];assert (p['start'],p['end'],p['total'],p['at_latest'])==(296,320,320,True)
    assert len(latest['messages'])==24 and latest['messages'][0]['id']=='296' and latest['messages'][-1]['id']=='319'
    assert all('material' not in m for m in latest['messages']) and 'history' not in latest
    query=urllib.parse.urlencode({'before':p['start'],'limit':24,'revision':p['revision']})
    older=call('/api/chats/'+id+'/messages?'+query);assert older['message_page']['start']==272 and older['messages'][-1]['id']=='295'
    first=call('/api/chats/'+id+'/messages?before=8');assert [m['id'] for m in first['messages']]==[str(i) for i in range(8)]
    after=peak()
    if before is not None:
        growth=after-before;assert growth<32*1024*1024,{'peak_growth':growth,'source_bytes':path.stat().st_size};print(f'PASS 69 MiB source, selected-message material and model history skipped; server peak growth {growth/1024/1024:.2f} MiB for history pages')
    try:call('/api/chats/'+id+'/messages?limit=0');raise AssertionError('Invalid page accepted')
    except urllib.error.HTTPError as error:assert error.code==400
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':1280,'height':900});errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
        page.goto(base);page.wait_for_selector('[data-chat="'+id+'"]');page.locator('[data-chat="'+id+'"]').click();page.wait_for_selector('[data-message-page="older"]')
        assert page.locator('#conversation .message').count()==24
        page.locator('[data-message-page="older"]').click();page.wait_for_function("()=>document.querySelector('[data-history-position]')?.textContent.includes('273–296')")
        assert page.locator('#conversation .message').count()==24 and page.evaluate("()=>document.activeElement.dataset.messagePage==='older'")
        assert not page.locator('#conversation [data-action="retry"]').count()
        page.locator('[data-edit-message="0"]').click();assert page.locator('#prompt').input_value().startswith('VISIBLE_MARKER_0272 ')
        page.locator('[data-message-page="newer"]').click();page.wait_for_function("()=>document.querySelector('[data-history-position]')?.textContent.includes('297–320')")
        assert page.locator('#conversation .message').count()==24
        with page.expect_download() as download:page.locator('[data-action="export"]').click()
        export=Path(download.value.path()).read_text(encoding='utf-8');assert export.count('VISIBLE_MARKER_')==320 and 'VISIBLE_MARKER_0000 ' in export and 'VISIBLE_MARKER_0319 ' in export
        pending=[];page.route('**/api/chats/'+id+'/messages?*',lambda route:pending.append(route),times=1)
        page.locator('[data-message-page="older"]').click()
        for _ in range(100):
            if pending:break
            page.wait_for_timeout(10)
        assert pending;page.locator('[data-action="new-chat"]').first.click();pending[0].continue_();page.wait_for_timeout(500)
        assert not page.locator('#conversation .message').count()
        assert not errors,errors;browser.close()
    with path.open(encoding='utf-8') as file:source=json.load(file)
    source['messages'].append({'id':'320','role':'user','created':'now','content':'new revision'})
    path.write_text(json.dumps(source),encoding='utf-8');del source
    try:call('/api/chats/'+id+'/messages?'+query);raise AssertionError('Stale revision accepted')
    except urllib.error.HTTPError as error:assert error.code==409
    assert call('/api/chats/'+id+'/messages')['message_page']['total']==321
    print('PASS Edge history window stays24 DOM messages, previous/next focus, old-page edit and no incorrect retry, full320-message export, navigation fence and stale-revision409')
finally:
    if process.poll() is None:
        try:call('/api/shutdown',{})
        except Exception:pass
        try:process.wait(timeout=8)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()

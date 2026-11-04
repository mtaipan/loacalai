"""Durable installation jobs against a disposable fake engine; no real downloads."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json,os,socket,sqlite3,subprocess,threading,time,urllib.request,urllib.error,urllib.parse,uuid

project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('installation-jobs-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
model='qwen2.5:3b';other='qwen2.5:14b';installed=set();pulls=[];mode='wait';started=threading.Event();finish=threading.Event()
class Engine(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):self.reply({'models':[{'name':name} for name in installed]} if self.path=='/api/tags' else {'version':'fixture'})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path!='/api/pull':return self.reply({'done':True,'status':'success','model_info':{'qwen.context_length':32768},'thinking':{'values':[False]}})
        selected=body['model'];request_mode=mode;pulls.append(selected);started.set()
        self.send_response(200);self.send_header('Content-Type','application/x-ndjson');self.end_headers()
        try:
            def emit(value):self.wfile.write(json.dumps(value).encode()+b'\n');self.wfile.flush()
            emit({'status':'fixture partial','total':100,'completed':40})
            if request_mode=='wait':
                while not finish.wait(.06):emit({'status':'fixture partial','total':100,'completed':40})
            if request_mode=='broken':return
            if request_mode!='false_success':installed.add(selected)
            emit({'status':'success'})
        except ConnectionError:pass
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token='';process=None;log=(root/'test.log').open('w')
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'cpu_only':True,'theme':'dark','source':'en','target':'ru','agent_model':model,'chat_model':model}))
def call(path,body=None):
    return http.open(urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=20)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def start():
    global process,token
    process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
    for _ in range(120):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(errors='replace'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('Own fixture server did not start')
def stop(crash=False):
    if process is None or process.poll() is not None:return
    if crash:process.kill()
    else:
        try:read('/api/shutdown',{})
        except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
def create(selected=model,request=None):return read('/api/installations',{'request_id':request or str(uuid.uuid4()),'spec':{'kind':'models','models':[selected]}})
def wait_status(id,statuses):
    for _ in range(160):
        job=read('/api/installations/'+id)
        if job['status'] in statuses:return job
        time.sleep(.05)
    raise AssertionError(job)
try:
    start();job=create();id=job['id'];assert started.wait(5);assert read('/api/installations/'+id)['status']=='running'
    assert create(request=id)['id']==id and create()['id']==id and len(pulls)==1
    queued=create(other);assert queued['status']=='queued';read('/api/installations/'+queued['id']+'/cancel',{});assert read('/api/installations/'+queued['id'])['status']=='cancelled' and other not in pulls
    read('/api/installations/'+id+'/cancel',{});wait_status(id,['cancelled']);assert read('/api/installations/'+id+'/events')['items']
    print('PASS idempotent and equivalent admission; queued and active cancellation retain durable progress without downloading the queued model')
    mode='broken';read('/api/installations/'+id+'/retry',{});job=wait_status(id,['failed']);assert 'подтверждения' in job['error']
    mode='false_success';read('/api/installations/'+id+'/retry',{});job=wait_status(id,['failed']);assert 'отсутствует' in job['error']
    mode='success';read('/api/installations/'+id+'/retry',{});job=wait_status(id,['succeeded']);assert job['attempt']==4
    print('PASS truncated stream and false success never become installed; retry rechecks actual tags before successful completion')
    installed.clear();mode='wait';started.clear();job=create();id=job['id'];assert started.wait(5)
    with call('/api/models/pull',{'model':model}) as stream:
        for line in stream:
            if line.startswith(b'data:'):
                assert json.loads(line[5:])['type']=='installation';break
    assert read('/api/installations/'+id)['status']=='running';count=len(pulls);assert count>=1
    stop(crash=True);time.sleep(.2);start();assert read('/api/installations/'+id)['status']=='interrupted';time.sleep(.2);assert len(pulls)==count
    finish.set();mode='success';read('/api/installations/'+id+'/retry',{});wait_status(id,['succeeded'])
    print('PASS legacy stream disconnect does not own the job; crash records interrupted without automatic replay and explicit retry reconciles installed state')
    # Populate only fixture-owned durable rows to exercise large journals/list pages in the real UI.
    with sqlite3.connect(root/'data/tasks.sqlite3') as db:
        for number in range(115):db.execute('INSERT INTO installation_jobs(id,spec,status,created,updated) VALUES(?,?,?, ?,?)',(str(uuid.uuid4()),json.dumps({'kind':'browser'}),'cancelled',f'2020-01-01T00:{number//60:02d}:{number%60:02d}Z','2020-01-01'))
        for number in range(150):db.execute('INSERT INTO installation_events(job,payload) VALUES(?,?)',(id,json.dumps({'type':'progress','message':f'JOURNAL_MARKER_{number}'})))
    seen=set();cursor=None
    while True:
        page=read('/api/installations'+('?' + urllib.parse.urlencode(cursor) if cursor else ''));assert len(page['items'])<=50
        for row in page['items']:assert row['id'] not in seen;seen.add(row['id'])
        cursor=page['next']
        if cursor is None:break
    assert len(seen)>=118
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':390,'height':844});errors=[];page.on('pageerror',lambda error:errors.append(str(error)))
        page.goto(base);page.wait_for_selector('[data-action="installations"]');page.evaluate('showInstallations()');page.wait_for_selector('.installation-item');assert page.locator('.installation-item').count()==50
        row=page.locator('[data-job="'+id+'"]');row.locator('summary').click();row.locator('[data-installation-action="journal"]').focus();page.wait_for_timeout(1500);assert row.locator('details').evaluate('(node)=>node.open') and page.evaluate("()=>document.activeElement.dataset.installationAction==='journal'")
        row.locator('[data-installation-action="journal"]').click();page.wait_for_selector('.journal-next');parts=[]
        for _ in range(20):
            parts.append(page.locator('dialog').last.locator('pre').text_content())
            if page.locator('.journal-next').is_disabled():break
            previous=parts[-1];page.locator('.journal-next').click();page.wait_for_function("previous=>document.querySelector('dialog:last-of-type pre').textContent!==previous",arg=previous)
        assert 'JOURNAL_MARKER_149' in '\n'.join(parts);page.locator('.journal-close').click();page.wait_for_timeout(100)
        assert page.evaluate("()=>document.activeElement.dataset.installationAction==='journal'")
        page.locator('.installations-older').click();page.wait_for_function("()=>document.querySelector('.installation-item h3')?.textContent==='Браузер'");assert page.locator('.installation-item').count()==50
        page.locator('.installations-newer').click();page.wait_for_selector('[data-job="'+id+'"]');assert page.locator('.installation-item').count()==50
        assert page.evaluate("()=>document.querySelector('#installations-dialog').getBoundingClientRect().right<=innerWidth")
        assert not errors,errors;browser.close()
    print('PASS real API journal/list cursors and Edge390 bounded50-job DOM; refresh preserves details/focus, full paged journal, older/newer navigation')
finally:
    finish.set();stop();fixture.shutdown();log.close()

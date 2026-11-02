"""Isolated durable project discovery: fake inference, no real model or user files."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json, os, socket, sqlite3, subprocess, threading, time, urllib.request, uuid

project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('discovery-test-'+uuid.uuid4().hex[:8])
workspace=root/'data/workspace';deep=workspace/'a/b/c/d/e/f/g/h';deep.mkdir(parents=True)
for index in range(401):(deep/f'файл-{index}.txt').write_text('fixture',encoding='utf-8')
(workspace/'large.md').write_text('я'*1_100_000+'НУЖНЫЙ ТЕКСТ',encoding='utf-8')
(workspace/'utf16.md').write_bytes(('один\r\nНУЖНЫЙ ТЕКСТ 🦀\r\n').encode('utf-16'))
(workspace/'README.md').write_text('Own discovery fixture',encoding='utf-8')
outside=root/'outside';outside.mkdir();(outside/'outside.txt').write_text('not project')
model='qwen2.5:3b'

class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True})
        messages=body['messages'];latest=max(i for i,m in enumerate(messages) if m['role']=='user');tools=[m for m in messages[latest+1:] if m['role']=='tool']
        if tools:return self.reply({'message':{'role':'assistant','content':'DISCOVERY_DONE: recorded observation only.'},'done':True})
        spec=json.loads(messages[latest]['content'].split('DISCOVERY_CASE:',1)[1].splitlines()[0])
        self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':spec['tool'],'arguments':spec['arguments']}}]},'done':True})

fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';token='';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));process=None
(root/'data/settings.json').write_text(json.dumps({'workspace':str(workspace),'computer_access':False,'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w')
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=60)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def start():
    global process,token
    process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('Own server did not start')
def stop():
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
def turn(chat,tool,args,new_task=False,failed=False):
    content='Inspect project files. DISCOVERY_CASE:'+json.dumps({'tool':tool,'arguments':args},ensure_ascii=False)
    with call('/api/chat',{'conversation':chat['id'],'content':content,'mode':'agent','web':'off','source':'en','target':'ru','attachments':[],'new_task':new_task}) as response:
        for line in response:
            if line.startswith(b'data:'):
                event=json.loads(line[5:]);assert event['type']!='approval',event
                assert event['type']!='error',event
    answer=read('/api/chats/'+chat['id'])['messages'][-1];record=answer['tools'][-1]
    assert record['status']==('failed' if failed else 'succeeded'),record
    with call('/api/artifacts/'+record['result_artifact']['id']+'/download') as response:body=response.read().decode()
    return body if failed else json.loads(body)

try:
    start();chat=read('/api/chats',{})
    first=turn(chat,'find_files',{'name':'*.txt','limit':17});assert first['has_more'];cursor=first['next_cursor'];found=list(first['matches'])
    stop();start()
    second=turn(chat,'find_files',{'name':'*.txt','cursor':cursor,'limit':17});assert turn(chat,'find_files',{'name':'*.txt','cursor':cursor,'limit':100})==second
    found+=second['matches'];page=second
    while page['has_more']:
        page=turn(chat,'find_files',{'name':'*.txt','cursor':page['next_cursor'],'limit':100});found+=page['matches']
    assert page['complete'] and len(found)==401 and len(set(found))==401
    assert not any('outside.txt' in path for path in found)
    foreign=read('/api/chats',{});assert 'Cursor' in turn(foreign,'find_files',{'name':'*.txt','cursor':cursor},failed=True)
    assert 'Cursor' in turn(chat,'find_files',{'name':'*.txt','cursor':cursor},new_task=True,failed=True)
    assert turn(chat,'list_files',{'path':'.','limit':100})['traversal_finished']
    text=turn(chat,'search_files',{'query':'нужный текст','pattern':'*.md','limit':100});hits=list(text['matches'])
    while text['has_more']:
        text=turn(chat,'search_files',{'query':'нужный текст','cursor':text['next_cursor'],'limit':100});hits+=text['matches']
    assert text['complete'] and len(hits)==2 and any(hit['encoding']=='utf-16le' and hit['line']==2 for hit in hits)
    inspection=read('/api/project/inspect',{'path':'.','limit':31});files=list(inspection['files'])
    assert inspection['readme']['text']=='Own discovery fixture' and 'suggested_checks' in inspection
    while inspection['has_more']:
        inspection=read('/api/project/inspect',{'path':'.','cursor':inspection['next_cursor'],'limit':100});files+=inspection['files']
    assert inspection['complete'] and sum(not item['directory'] for item in files)==404
    assert turn(chat,'find_files',{'name':'*.txt','directory':str(outside),'cursor':cursor},failed=True)
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True)
        page=browser.new_page(viewport={'width':320,'height':760});errors=[]
        page.on('pageerror',lambda error:errors.append(str(error)))
        page.goto(base);page.wait_for_selector('[data-action="projects"]',state='attached')
        page.evaluate('showProjects()');page.wait_for_selector('#projects-dialog[open]')
        page.locator('[data-action="inspect-project"]').click()
        page.wait_for_selector('[data-project-cursor]')
        initial=page.locator('.project-overview>pre').inner_text()
        cursor=page.locator('[data-project-cursor]').last.get_attribute('data-project-cursor')
        def fail_next(route):route.fulfill(status=503,content_type='application/json',body=json.dumps({'error':'OWN_FIXTURE_RETRY'}))
        page.route('**/api/project/inspect',fail_next,times=1)
        page.locator('[data-project-cursor]').last.click()
        page.wait_for_function("document.querySelector('#project-page-status').textContent.includes('OWN_FIXTURE_RETRY')")
        assert page.locator('.project-overview>pre').inner_text()==initial
        page.locator('#project-page-status button').click()
        page.wait_for_selector('[data-project-cursor="'+cursor.rsplit(':',1)[0]+':0"]')
        assert page.locator('.project-overview>pre').inner_text()!=initial
        page.locator('[data-project-cursor]').first.click()
        page.wait_for_function('(value)=>document.querySelector(".project-overview>pre").textContent===value',arg=initial)
        assert page.locator('.project-overview>pre').inner_text()==initial
        for button in page.locator('[data-project-cursor]').all():
            bounds=button.bounding_box();assert bounds['x']>=0 and bounds['x']+bounds['width']<=320,bounds
        page.screenshot(path=str(root/'project-inspector-mobile.png'),full_page=True)
        assert not errors,errors
        browser.close()
    print('PASS Edge320 inspector changes packets, replays previous packet and retains page on failed request/retry')
    # Deletion only removes internal cursor/queue receipts; source files survive.
    with sqlite3.connect(root/'data/tasks.sqlite3') as db:task_ids=[row[0] for row in db.execute('SELECT id FROM work_tasks WHERE conversation=?',(chat['id'],))]
    request=urllib.request.Request(base+'/api/chats/'+chat['id'],method='DELETE',headers={'X-Studio-Token':token})
    with http.open(request):pass
    assert (deep/'файл-0.txt').exists()
    with sqlite3.connect(root/'data/discovery.sqlite') as db:
        assert all(db.execute('SELECT COUNT(*) FROM scans WHERE owner=?',('task:'+task,)).fetchone()[0]==0 for task in task_ids)
    print('PASS discovery pages retain >400 deep files, task scope/restart/replay, large UTF-8/UTF-16, project inspector compatibility and project-only defaults')
finally:
    if process is not None and process.poll() is None:stop()
    fixture.shutdown();log.close()

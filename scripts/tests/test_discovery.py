"""Isolated durable project discovery: fake inference, no real model or user files."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json, os, socket, sqlite3, subprocess, threading, time, urllib.request, urllib.parse, urllib.error, uuid

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
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=90)
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
def turn(chat,tool,args,new_task=False,failed=False,approval_text=None):
    approvals=[]
    content='Inspect project files. DISCOVERY_CASE:'+json.dumps({'tool':tool,'arguments':args},ensure_ascii=False)
    with call('/api/chat',{'conversation':chat['id'],'content':content,'mode':'agent','web':('auto' if tool=='web_search' else 'off'),'source':'en','target':'ru','attachments':[],'new_task':new_task}) as response:
        for line in response:
            if line.startswith(b'data:'):
                event=json.loads(line[5:])
                if event['type']=='approval':
                    approvals.append(event);assert approval_text is not None,event
                    for text in approval_text:assert text in event['details'],event
                    read('/api/approve/'+event['id'],{'allow':True,'duration':'once'})
                assert event['type']!='error',event
    assert len(approvals)==(1 if approval_text is not None else 0),approvals
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
    tree=read('/api/tree?'+urllib.parse.urlencode({'path':'a/b/c/d/e/f/g/h'}));tree_files=list(tree['files'])
    while tree['has_more']:
        tree=read('/api/tree?'+urllib.parse.urlencode({'path':'a/b/c/d/e/f/g/h','cursor':tree['next_cursor']}));tree_files+=tree['files']
    assert tree['complete'] and len(tree_files)==401
    try:read('/api/tree?'+urllib.parse.urlencode({'path':str(outside)}));raise AssertionError('Panel escaped project')
    except urllib.error.HTTPError as error:assert error.code==400
    encoded=workspace/'encoded';encoded.mkdir()
    for codec,prefix,explicit in [('utf-8',b'\xef\xbb\xbf',False),('utf-16le',b'\xff\xfe',False),('utf-16be',b'\xfe\xff',False),('utf-32le',b'\xff\xfe\x00\x00',False),('utf-32be',b'\x00\x00\xfe\xff',False),('cp1251',b'',True),('cp866',b'',True)]:
        file=encoded/(codec+'.txt');original=prefix+'строка\r\nстарое значение\r\n'.encode(codec);file.write_bytes(original)
        encoding={'cp1251':'windows-1251','cp866':'ibm866'}.get(codec,'auto')
        args={'path':str(file),'old_text':'старое значение','new_text':'новое значение','encoding':encoding}
        result=turn(chat,'edit_file',args,approval_text=['-старое значение','+новое значение','CRLF 2, LF 0'])
        expected=prefix+'строка\r\nновое значение\r\n'.encode(codec)
        assert result['verified'] and file.read_bytes()==expected
        if explicit:
            bad={**args,'old_text':'новое значение','new_text':'🦀'}
            assert turn(chat,'edit_file',bad,failed=True);assert file.read_bytes()==expected
        read('/api/changes/'+result['change_id']+'/restore',{})
        assert file.read_bytes()==original
    print('PASS approved UTF BOM/endianness and explicit legacy edits preserve CRLF; unrepresentable text never reaches approval; undo restores original bytes')
    for endpoint,args in [('/api/project/inspect',{'path':42}),('/api/project/inspect',{'limit':0}),('/api/project/git',{'action':42})]:
        try:read(endpoint,args);raise AssertionError('Malformed inspection arguments accepted')
        except urllib.error.HTTPError as error:assert error.code==400
    chosen=read('/api/project/inspect',{'path':str(outside)})
    assert any(item['path']=='outside.txt' for item in chosen['files'])
    assert not read('/api/bootstrap')['settings']['computer_access']
    (workspace/'package.json').write_text(json.dumps({'scripts':{'test':'echo OWN_NEVER_RUN'}}))
    replay_chat=read('/api/chats',{'mode':'search'})
    original_scope=read('/api/bootstrap')['settings']['workspace']
    replay_body={'request_id':str(uuid.uuid4()),'conversation':replay_chat['id'],'expected_workspace':original_scope,'content':'OWN_SCOPE_ONLY','mode':'search','web':'off','source':'en','target':'ru','attachments':[],'search':{'depth':'results','focus':'materials'}}
    accepted=read('/api/runs',replay_body)
    with call('/api/runs/'+accepted['run_id']+'/events') as response:
        for line in response:
            if line.startswith(b'data:'):assert json.loads(line[5:])['type']!='error',line
    assert read('/api/runs/'+accepted['run_id'])['status']=='completed'
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True)
        page=browser.new_page(viewport={'width':320,'height':760});errors=[]
        page.on('pageerror',lambda error:errors.append(str(error)))
        page.goto(base);page.wait_for_selector('[data-action="projects"]',state='attached')
        page.locator('header [data-action="sidebar"]').click();page.locator('.sidebar [data-action="projects"]').click();page.wait_for_selector('#projects-dialog[open]')
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
        assert page.evaluate('()=>document.activeElement.matches("[data-project-cursor]")')
        for button in page.locator('[data-project-cursor]').all():
            bounds=button.bounding_box();assert bounds['x']>=0 and bounds['x']+bounds['width']<=320,bounds
        page.screenshot(path=str(root/'project-inspector-mobile.png'),full_page=True)
        page.locator('[data-project-command]').first.click()
        assert not page.locator('#projects-dialog').evaluate('(dialog)=>dialog.open')
        assert page.evaluate('()=>document.activeElement.id==="prompt"')
        assert 'Запусти проверку' in page.locator('#prompt').input_value()
        page.set_viewport_size({'width':1280,'height':900})
        page.locator('header [data-action="files"]').click()
        for directory in ['a','a/b','a/b/c','a/b/c/d','a/b/c/d/e','a/b/c/d/e/f','a/b/c/d/e/f/g','a/b/c/d/e/f/g/h']:
            page.locator('[data-tree-directory="'+directory+'"]').click()
        page.wait_for_selector('[data-tree-path]');initial_files=page.locator('[data-tree-path]').all_text_contents()
        assert len(initial_files)==64
        assert page.evaluate('()=>document.activeElement.matches(".file-current")')
        page.locator('[data-tree-cursor]').last.click()
        page.wait_for_function('()=>document.querySelectorAll("[data-tree-cursor]").length===2')
        page.locator('[data-tree-cursor]').first.click()
        page.wait_for_function('(expected)=>JSON.stringify([...document.querySelectorAll("[data-tree-path]")].map(b=>b.textContent))===JSON.stringify(expected)',arg=initial_files)
        selected=page.locator('[data-tree-path]').first.get_attribute('data-tree-path')
        page.locator('#prompt').fill('')
        page.locator('[data-tree-path]').first.click()
        assert Path(json.loads(page.locator('#prompt').input_value())).resolve()==(workspace/selected).resolve()
        assert page.locator('[data-tree-path]').count()==64
        page.route('**/api/tree?*',lambda route:route.fulfill(status=503,content_type='application/json',body=json.dumps({'error':'OWN_FOLDER_RETRY'})),times=1)
        page.locator('#file-tree .file-location [data-tree-directory=""]').click()
        page.wait_for_function("()=>document.querySelector('#file-page-status')?.textContent.includes('OWN_FOLDER_RETRY')")
        assert page.locator('[data-tree-path]').count()==64
        page.locator('#file-page-status button').click();page.wait_for_function("()=>document.querySelector('.file-current')?.textContent==='Корень проекта'")
        assert page.evaluate("()=>document.activeElement.matches('.file-current')")
        page.locator('#prompt').fill('до 🦀 после')
        page.evaluate("()=>{const prompt=document.getElementById('prompt');prompt.setSelectionRange(prompt.value.indexOf(' после'),prompt.value.indexOf(' после'));window.originalRead=navigator.clipboard.read;Object.defineProperty(navigator.clipboard,'read',{configurable:true,value:async()=>[{types:['text/plain'],getType:async()=>new Blob(['X\\r\\nY'])}]});}")
        page.locator('[data-action="paste"]').click()
        page.wait_for_function("()=>document.getElementById('prompt').value.includes('X\\nY')")
        assert page.locator('#prompt').input_value()=='до 🦀X\nY после'
        assert page.evaluate("()=>document.activeElement.id==='prompt'&&document.activeElement.selectionStart===document.activeElement.value.indexOf(' после')")
        page.keyboard.insert_text('!');assert page.locator('#prompt').input_value()=='до 🦀X\nY! после'
        page.evaluate("()=>Object.defineProperty(navigator.clipboard,'read',{configurable:true,value:()=>new Promise(resolve=>window.resolveOwnClipboard=resolve)})")
        page.locator('[data-action="paste"]').click();page.locator('#prompt').fill('changed draft');page.evaluate("()=>document.getElementById('prompt').setSelectionRange(2,2)")
        page.evaluate("()=>resolveOwnClipboard([{types:['text/plain'],getType:async()=>new Blob(['LATE_TEXT'])}])")
        page.wait_for_function("()=>document.getElementById('prompt').value.includes('LATE_TEXT')")
        assert page.evaluate("()=>document.activeElement.id==='prompt'&&document.activeElement.selectionStart===2")
        page.evaluate("()=>Object.defineProperty(navigator.clipboard,'read',{configurable:true,value:originalRead})")
        if page.locator('body').evaluate("(body)=>body.classList.contains('sidebar-hidden')"):page.locator('header [data-action="sidebar"]').click()
        page.locator('.sidebar [data-action="projects"]').click();page.wait_for_selector('#projects-dialog[open]')
        pending=[];page.route('**/api/settings',lambda route:pending.append(route),times=1)
        page.locator('[data-action="use-project"]').click()
        for _ in range(100):
            if pending:break
            page.wait_for_timeout(20)
        assert pending
        page.locator('#project-path').fill(str(outside));pending[0].continue_()
        page.wait_for_function("()=>!document.querySelector('#projects-dialog [data-action=use-project]').disabled")
        assert page.locator('#projects-dialog').evaluate('(dialog)=>dialog.open')
        assert page.locator('#project-path').input_value()==str(outside)
        assert Path(read('/api/bootstrap')['settings']['workspace']).resolve()==workspace.resolve()
        page.locator('[data-action="use-project"]').click()
        page.wait_for_function("()=>!document.getElementById('projects-dialog').open")
        assert Path(read('/api/bootstrap')['settings']['workspace']).resolve()==outside.resolve()
        assert page.evaluate("()=>document.activeElement.id==='prompt'")
        assert not errors,errors
        browser.close()
    assert read('/api/runs',replay_body)['run_id']==accepted['run_id']
    stale_body={**replay_body,'request_id':str(uuid.uuid4())}
    try:read('/api/runs',stale_body);raise AssertionError('Stale workspace accepted')
    except urllib.error.HTTPError as error:assert error.code==409
    try:read('/api/runs/'+stale_body['request_id']);raise AssertionError('Rejected workspace created a run')
    except urllib.error.HTTPError as error:assert error.code==404
    assert read('/api/runs/'+accepted['run_id'])['status']=='completed'
    print('PASS stale project scope creates no Run; accepted idempotent replay retains original project after settings change')
    print('PASS Edge320 paging/focus/retry, CRLF/Unicode and delayed paste caret, modal insertion, late project edits and explicit root selection without agent grant expansion')
    if os.environ.get('LOCAL_GEMMA_SLOW_PROTOCOL')=='1':
        (root/'runtime').mkdir()
        subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:TEST_PY_LINK -Target $env:TEST_PY_TARGET | Out-Null'],env={**os.environ,'TEST_PY_LINK':str(root/'runtime/python'),'TEST_PY_TARGET':str(project/'runtime/python')},check=True)
        (root/'runtime/web_worker.py').write_text("import json,sys,time\nfor line in sys.stdin:\n request=json.loads(line);time.sleep(65);print(json.dumps({'query':request['args'].get('query',''),'results':[],'worker_wait_seconds':65}),flush=True)\n")
        started=time.monotonic();result=turn(chat,'web_search',{'query':'slow protocol fixture'})
        assert time.monotonic()-started>=65 and result['worker_wait_seconds']==65,result
        print('PASS own web protocol worker completes after 65 seconds without the former 60-second deadline; no network requests')

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

"""Real HTTP and Edge against disposable data and fake, delayed inference."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json, os, socket, subprocess, threading, time, uuid, urllib.request, urllib.error

project = Path(__file__).resolve().parents[2]
root = project / 'build-tools' / ('task-test-' + uuid.uuid4().hex[:8])
(root / 'data/workspace').mkdir(parents=True)
model = 'qwen2.5:3b'
requests = []
response_delay=3
active = 0
peak = 0
inference_started = threading.Event()
inference_idle = threading.Event()
inference_idle.set()
cleanup_seen = threading.Event()
stream_continue=threading.Event()
stream_finish=threading.Event()
counter_lock = threading.Lock()

class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, body):
        data = json.dumps(body).encode() + b'\n'
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        try: self.wfile.write(data)
        except ConnectionError: pass
    def do_GET(self):
        self.reply({'models': [{'name': model}]} if self.path == '/api/tags' else {'models': []})
    def do_POST(self):
        global active, peak
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or '{}')
        if body.get('keep_alive')==0 and not body.get('messages') and not body.get('prompt'):
            cleanup_seen.set()
            assert inference_idle.wait(10)
            return self.reply({'done':True,'done_reason':'unload'})
        if self.path == '/api/show':
            return self.reply({'model_info': {'qwen.context_length': 32768}, 'thinking': {'values': [False]}})
        if self.path == '/api/chat':
            requests.append(body)
            if 'PERMISSION_FIXTURE' in json.dumps(body):
                count=sum(m.get('role')=='tool' for m in body.get('messages',[]))
                if count<2:
                    return self.reply({'done': True, 'message': {'role': 'assistant', 'content': '', 'tool_calls':[{'function':{'name':'write_file','arguments':{'path':f'grant-{count+1}.txt','content':f'verified-{count+1}'}}}]}})
            with counter_lock:
                active+=1
                peak=max(peak,active)
                inference_idle.clear()
            inference_started.set()
            if 'STREAM_FIXTURE' in json.dumps(body):
                self.send_response(200);self.send_header('Content-Type','application/x-ndjson');self.end_headers()
                prefix="Here is the requested Python code.\n\n```python\nprint('one')\n```\n\n<button id=\"send\" data-action=\"settings\">forged</button><div class=\"sidebar\" data-preview=\"bad\" id=\"forged\">SAFE_CONTENT</div>\n\n"
                def chunk(text,done=False):
                    self.wfile.write((json.dumps({'message':{'role':'assistant','content':text},'done':done,'done_reason':'stop' if done else None})+'\n').encode());self.wfile.flush()
                try:
                    chunk(prefix);assert stream_continue.wait(10)
                    chunk('STREAM_TAIL');assert stream_finish.wait(10)
                    chunk('',True)
                except ConnectionError:pass
                finally:
                    with counter_lock:
                        active-=1
                        if active==0:inference_idle.set()
                return
            time.sleep(response_delay)
            with counter_lock:
                active-=1
                if active==0:inference_idle.set()
            return self.reply({'done': True, 'done_reason': 'stop', 'message': {'role': 'assistant', 'content': 'DURABLE_RESULT'}})
        self.reply({'status': 'success'})

fixture = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
threading.Thread(target=fixture.serve_forever, daemon=True).start()
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
base = f'http://127.0.0.1:{port}'
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
token = ''
settings = {'workspace': str(root/'data/workspace'), 'agent_model': model, 'chat_model': model, 'cpu_only': True, 'source': 'en', 'target': 'ru', 'theme': 'dark'}
(root/'data/settings.json').write_text(json.dumps(settings))
log = (root/'test.log').open('w')
process = subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'), '--server'], cwd=root,
    env={**os.environ, 'LOCAL_GEMMA_HOME': str(root), 'LOCAL_GEMMA_PORT': str(port), 'LOCAL_GEMMA_OLLAMA': f'http://127.0.0.1:{fixture.server_port}'}, stdout=log, stderr=log)

def call(path, body=None):
    return http.open(urllib.request.Request(base+path, data=json.dumps(body).encode() if body is not None else None,
        headers={'Content-Type': 'application/json', 'X-Studio-Token': token}), timeout=20)
def read(path, body=None):
    with call(path, body) as response: return json.load(response)
def events(path, body=None):
    with call(path, body) as response: return [json.loads(line[5:]) for line in response if line.startswith(b'data:')]

try:
    for _ in range(100):
        try:
            token = read('/api/bootstrap')['token']
            break
        except Exception: time.sleep(.1)
    else: raise AssertionError('isolated server did not start')
    chat = read('/api/chats', {})
    rid = str(uuid.uuid4())
    body = {'request_id': rid, 'conversation': chat['id'], 'content': 'Explain durable events', 'mode': 'chat', 'source': 'en', 'target': 'ru', 'web': 'off'}
    response = call('/api/chat', body)
    first = None
    for line in response:
        if line.startswith(b'data:'):
            first = json.loads(line[5:])
            break
    assert first['type'] == 'start'
    response.close()  # Subscriber disappears while inference is still running.
    replay = events(f'/api/runs/{rid}/events?after={first["sequence"]}')
    assert replay[-1]['type'] == 'done', replay
    assert any(e.get('text') == 'DURABLE_RESULT' for e in replay)
    assert len(requests) == 1
    events('/api/chat', body)  # Same request must only replay the saved result.
    assert len(requests) == 1
    assert len(read('/api/chats/'+chat['id'])['messages']) == 2
    assert read('/api/runs/'+rid)['status'] == 'completed'
    assert read('/api/runs',body)['status']=='completed'
    assert len(requests)==1
    print('PASS disconnect, ordered replay, idempotent submission and persisted result')

    from playwright.sync_api import sync_playwright
    with sync_playwright() as p:
        browser = p.chromium.launch(channel='msedge', headless=True)
        page = browser.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(base)
        page.wait_for_selector('#prompt')
        page.locator('[data-mode="chat"]').click()
        # Rejected request keeps both draft formats and survives navigation/reload.
        page.locator('#file-input').set_input_files({'name':'draft.xml','mimeType':'text/plain','buffer':b'<Root id="send" data-action="settings">DRAFT_ATTACHMENT</Root>'})
        page.wait_for_function('()=>state.pending.length===1&&!state.uploading')
        page.locator('#prompt').fill('UNSENT_DRAFT_ONE')
        page.route('**/api/runs',lambda route:route.fulfill(status=503,content_type='application/json',body='{"error":"TEMPORARILY_UNAVAILABLE"}') if route.request.method=='POST' else route.continue_())
        page.locator('#send').click()
        page.wait_for_function('()=>!state.busy&&!state.sending&&state.chat!==null')
        draft_chat=page.evaluate('state.chat.id')
        assert page.locator('#prompt').input_value()=='UNSENT_DRAFT_ONE'
        assert page.evaluate('state.pending.length')==1
        page.locator('#pending-attachments [data-preview]').click()
        assert page.locator('#preview-body pre code').inner_text()=='<Root id="send" data-action="settings">DRAFT_ATTACHMENT</Root>'
        assert page.locator('#preview-body [data-action]').count()==0
        page.keyboard.press('Escape')
        page.unroute('**/api/runs')
        page.reload();page.wait_for_selector('#prompt')
        assert page.locator('#prompt').input_value()=='UNSENT_DRAFT_ONE'
        assert page.evaluate('state.pending.length')==1
        page.locator('[data-action="new-chat"]').click()
        page.locator('#prompt').fill('UNSENT_DRAFT_TWO')
        page.locator(f'[data-chat="{draft_chat}"]').click()
        page.wait_for_function('()=>document.getElementById("prompt").value==="UNSENT_DRAFT_ONE"')
        assert page.evaluate('state.pending.length')==1
        page.locator('[data-action="new-chat"]').click()
        assert page.locator('#prompt').input_value()=='UNSENT_DRAFT_TWO'
        # Closing settings retains unsaved fields. Discard is explicit.
        page.locator('[data-action="settings"]').click()
        page.locator('[data-settings-tab="agent"]').click()
        page.locator('#agent-instructions').fill('UNSAVED_INSTRUCTIONS')
        page.keyboard.press('Escape')
        page.locator('[data-action="settings"]').click()
        assert page.locator('#agent-instructions').input_value()=='UNSAVED_INSTRUCTIONS'
        page.get_by_role('button',name='Сбросить изменения',exact=True).click()
        page.wait_for_function('()=>document.getElementById("agent-instructions").value!=="UNSAVED_INSTRUCTIONS"')
        page.locator('#agent-instructions').fill('SAVED_SNAPSHOT')
        def delayed_settings(route):
            response=route.fetch()
            page.evaluate('()=>{const field=document.getElementById("agent-instructions");field.value="LATE_EDIT";field.dispatchEvent(new Event("input",{bubbles:true}));}')
            route.fulfill(response=response)
        page.route('**/api/settings',delayed_settings)
        page.locator('[data-action="save-settings"]').click()
        page.wait_for_function('()=>!settingsSaving')
        assert page.locator('#settings-dialog').evaluate('(dialog)=>dialog.open')
        assert page.locator('#agent-instructions').input_value()=='LATE_EDIT'
        assert read('/api/bootstrap')['settings']['assistant']['agent_instructions']=='SAVED_SNAPSHOT'
        page.unroute('**/api/settings')
        page.get_by_role('button',name='Сбросить изменения',exact=True).click()
        page.wait_for_function('()=>document.getElementById("agent-instructions").value==="SAVED_SNAPSHOT"')
        page.keyboard.press('Escape')
        response_delay=6
        page.locator('#prompt').fill('Explain browser reload')
        page.locator('#send').click()
        page.wait_for_function('()=>state.run!==null')
        active_chat=page.evaluate('state.chat.id')
        page.locator('[data-action="settings"]').click()
        page.keyboard.press('Escape')
        assert page.evaluate('state.busy'), 'closing a dialog must not stop the task'
        page.locator('#prompt').fill('NEXT_DRAFT_DURING_RUN')
        page.locator('#prompt').press('Enter')
        assert page.evaluate('state.busy'), 'Enter in a draft must not act as Stop'
        page.locator('[data-action="new-chat"]').click()
        assert not page.evaluate('state.busy')
        page.locator('#prompt').fill('BACKGROUND_DRAFT')
        page.locator(f'[data-chat="{active_chat}"]').click()
        page.wait_for_function('()=>state.busy')
        page.reload()
        page.wait_for_function('()=>state.busy && state.run!==null')
        assert page.locator('#send').get_attribute('aria-label') == 'Остановить ответ'
        page.wait_for_function('()=>!state.busy', timeout=15000)
        assert 'DURABLE_RESULT' in page.locator('#conversation').inner_text()
        response_delay=3
        page.locator('[data-action="new-chat"]').click()
        page.locator('[data-mode="agent"]').click()
        page.locator('#prompt').fill('PERMISSION_FIXTURE Create two files in the workspace')
        page.locator('#send').click()
        page.wait_for_selector('[data-grant-duration]')
        page.reload();page.wait_for_selector('[data-grant-duration]')
        scope=page.locator('[data-grant-duration]')
        scope.select_option('task')
        scope.focus()
        page.evaluate('()=>{state.streamThinking="streaming update";scheduleStream();}')
        page.wait_for_timeout(200)
        assert scope.input_value()=='task'
        assert scope.evaluate('(element)=>element===document.activeElement')
        page.locator('[data-approve]').click()
        page.wait_for_function('()=>!state.busy',timeout=15000)
        assert (root/'data/workspace/grant-1.txt').read_text()=='verified-1'
        assert (root/'data/workspace/grant-2.txt').read_text()=='verified-2'
        assert not read('/api/permissions'), 'task grant must expire when run finishes'
        page.locator('[data-action="new-chat"]').click();page.locator('[data-mode="chat"]').click()
        page.locator('#prompt').fill('STREAM_FIXTURE provide code')
        page.locator('#send').click()
        code_button=page.locator('#stream-body [data-copy-code]');code_button.wait_for()
        code_button.focus();page.evaluate('()=>window.savedCodeButton=document.querySelector("#stream-body [data-copy-code]")')
        stream_continue.set()
        page.wait_for_function('()=>document.getElementById("stream-body")?.textContent.includes("STREAM_TAIL")')
        assert page.evaluate('()=>savedCodeButton.isConnected&&document.activeElement===savedCodeButton')
        assert page.locator('#stream-body [id],#stream-body [data-action],#stream-body [data-preview],#stream-body .sidebar').count()==0
        stream_finish.set();page.wait_for_function('()=>!state.busy')
        assert page.evaluate('()=>savedCodeButton.isConnected&&document.activeElement===savedCodeButton')
        page.locator('#prompt').focus();page.wait_for_selector('#stream-message',state='detached')
        assert page.evaluate("async()=>{Object.defineProperty(navigator.clipboard,'writeText',{value:async()=>{throw Error('denied')},configurable:true});document.execCommand=()=>false;return await copy('fixture');}") is False
        assert 'Не удалось скопировать' in page.locator('#toasts').last.inner_text()
        page.screenshot(path=str(root/'desktop.png'),full_page=True)
        page.set_viewport_size({'width':320,'height':700})
        page.locator('[data-action="sidebar"]').click()
        page.locator('[data-action="settings"]').click()
        page.locator('[data-settings-tab="permissions"]').click()
        assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
        footer=page.locator('#settings-dialog>.dialog-footer').bounding_box()
        assert footer['y']+footer['height']<=700,footer
        page.screenshot(path=str(root/'settings-mobile.png'),full_page=True)
        page.keyboard.press('Escape')
        assert not errors, errors
        browser.close()
    print('PASS Edge reload reconnects, restores stop control and receives saved answer')
    print('PASS scoped approval survives stream/focus, authorizes two files and expires after run')
    print('PASS per-chat text/attachment drafts, explicit settings discard and background chat navigation')
    print('PASS late settings edits, streaming code focus, completed-response focus and untrusted HTML boundaries')
    # New request waits until cleanup of a cancelled request is acknowledged.
    first=read('/api/chats',{})
    rid=str(uuid.uuid4())
    body={'request_id':rid,'conversation':first['id'],'content':'Explain cancellation cleanup','mode':'chat','source':'en','target':'ru','web':'off'}
    inference_started.clear()
    response=call('/api/chat',body)
    for line in response:
        if line.startswith(b'data:'): break
    response.close()
    assert inference_started.wait(10)
    read('/api/stop/'+rid,{})
    events('/api/runs/'+rid+'/events')
    second=read('/api/chats',{})
    body={**body,'request_id':str(uuid.uuid4()),'conversation':second['id'],'content':'Explain queued cleanup'}
    events('/api/chat',body)
    assert cleanup_seen.is_set()
    assert peak==1, f'overlapping inference: {peak}'
    print('PASS cancellation requests model unload and the next inference waits for cleanup')
    for _ in range(100):
        if read('/api/status')['active_tasks']==0:break
        time.sleep(.02)
    request=urllib.request.Request(base+'/api/chats/'+second['id'],method='DELETE',headers={'X-Studio-Token':token})
    with http.open(request,timeout=20) as response:assert response.status==200
    assert not any(run['conversation']==second['id'] for run in read('/api/runs'))
    for path in ['/api/chats/'+second['id'],'/api/runs/'+body['request_id']]:
        try:read(path)
        except urllib.error.HTTPError as error:assert error.code==404
        else:raise AssertionError('Deleted chat/run remains accessible: '+path)
    assert not (root/'data/chats'/f"{second['id']}.json").exists()
    print('PASS deleting a conversation removes its JSON, run inputs and event journal')

finally:
    try: call('/api/shutdown', {}).close()
    except Exception: pass
    try: process.wait(timeout=10)
    except subprocess.TimeoutExpired: process.terminate(); process.wait(timeout=5)
    fixture.shutdown()
    log.close()

"""Isolated Rust server + fake inference: context transport, not model quality.
Never executes the user's banking task or downloads models.
"""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json, os, socket, subprocess, threading, time, uuid, urllib.request, urllib.error

project = Path(__file__).resolve().parents[2]
root = project/'build-tools'/('context-'+uuid.uuid4().hex[:8])
(root/'data/workspace').mkdir(parents=True)
requests = []
model_limit = 262144
foundation = 'hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M'
model = 'qwen2.5:3b'

class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, body):
        data = json.dumps(body).encode()+b'\n'
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        self.reply({'models':[{'name':model},{'name':foundation}]} if self.path=='/api/tags' else {'models':[]} if self.path=='/api/ps' else {'version':'0.40.13'})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if body.get('keep_alive')==0 and not body.get('messages') and not body.get('prompt'):
            return self.reply({'done':True,'done_reason':'unload'})
        if self.path=='/api/show':
            return self.reply({'model_info':{'qwen.context_length':model_limit},'thinking':{'values':[False,True]}})
        if self.path in ['/api/chat','/api/generate']:
            requests.append(body)
            text = body.get('prompt','')+'\n'.join(m.get('content','') for m in body.get('messages',[]))
            limited = 'OUTPUT_LIMIT_FIXTURE' in text
            completion = {'done':True,'done_reason':'length' if limited else 'stop','prompt_eval_count':1234,'eval_count':77}
            if self.path=='/api/generate':completion['response']='Specialist observations only. No tests were executed.'
            else:completion['message']={'role':'assistant','content':'CONTEXT_ACCEPTED. This is a transport fixture, no project was created or tested.'}
            if 'PARTIAL_TOOL_CALL_FIXTURE' in text:
                completion['done_reason']='length'
                completion['message']['tool_calls']=[{'function':{'name':'write_file','arguments':{'path':'should-never-exist.txt','content':'incomplete'}}}]
            if 'BROKEN_STREAM_FIXTURE' in text:
                completion['done']=False
            return self.reply(completion)
        self.reply({'status':'success'})

server = ThreadingHTTPServer(('127.0.0.1',0),Fixture)
threading.Thread(target=server.serve_forever,daemon=True).start()
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
base='http://127.0.0.1:'+str(port)
http=urllib.request.build_opener(urllib.request.ProxyHandler({})); token=''
settings={'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'team_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark','context':{'window_tokens':0,'auto_max_tokens':32768,'output_tokens':4096}}
(root/'data/settings.json').write_text(json.dumps(settings))
log=(root/'test.log').open('w')
process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':'http://127.0.0.1:'+str(server.server_port)},stdout=log,stderr=log)

def call(path, body=None):
    return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30)
def read(path, body=None):
    with call(path,body) as response:return json.load(response)
def turn(text,mode='agent',thinking='off',chat=None,allow_error=False,web='off',attachments=None,retry=False,new_task=None,task_id=None):
    chat=chat or read('/api/chats',{})
    with call('/api/chat',{'conversation':chat['id'],'content':text,'mode':mode,'source':'en','target':'ru','web':web,'thinking':thinking,'attachments':attachments or [],'retry':retry,'new_task':new_task,'task_id':task_id}) as response:
        for line in response:
            if line.startswith(b'data:'):
                event=json.loads(line[5:]); assert event['type']!='approval',event
                assert allow_error or event['type']!='error',event
    return read('/api/chats/'+chat['id'])

try:
    for _ in range(100):
        try:token=read('/api/bootstrap')['token']; break
        except Exception:time.sleep(.1)
    else:raise RuntimeError('Studio startup failed')
    for path in ['/api/chats/'+str(uuid.uuid4()),'/api/runs/'+str(uuid.uuid4())]:
        try:read(path)
        except urllib.error.HTTPError as error:
            assert error.code==404
            assert json.load(error)['code']=='not_found'
        else:raise AssertionError('missing resource must return typed 404')
    config=read('/api/bootstrap')['settings']
    config['assistant']['chat_instructions']='CHAT_PERSONA_FIXTURE'
    config['assistant']['agent_instructions']='AGENT_PERSONA_FIXTURE'
    config['assistant']['chat_temperature']=.63
    config['assistant']['agent_temperature']=.17
    read('/api/settings',config)
    history=turn('Remember CONTEXT_URL_MARKER','chat')
    turn('Explain http://example.com/article','chat',chat=history,web='auto')
    body=requests[-1]
    assert any('CONTEXT_URL_MARKER' in m.get('content','') for m in body['messages']),body
    assert 'CHAT_PERSONA_FIXTURE' in body['messages'][0]['content']
    assert 'AGENT_PERSONA_FIXTURE' not in body['messages'][0]['content']
    assert body['options']['temperature']==.63,body
    turn('Explain the bug and fix it','agent')
    assert requests[-1]['tools'] and requests[-1]['options']['temperature']==.17
    turn('Объясни ошибку и почини проект','agent')
    assert requests[-1].get('tools'), 'an explanation prefix must not disable agent capabilities'
    goal=turn('Build TASK_GOAL_FIXTURE and preserve its tests','agent')
    task=read('/api/chats/'+goal['id']+'/tasks')[0]
    # Remove the model working history to simulate a fresh context window.
    saved_path=root/'data/chats'/f"{goal['id']}.json"
    saved=json.loads(saved_path.read_text(encoding='utf-8'));saved['history']=[];saved['messages']=[]
    saved_path.write_text(json.dumps(saved),encoding='utf-8')
    continued=turn('продолжай работу','agent',chat=goal)
    assert 'TASK_GOAL_FIXTURE' in requests[-1]['messages'][0]['content']
    assert any(t['name']=='read_task' and t['status']=='succeeded' for t in continued['messages'][-1]['tools'])
    state=read('/api/tasks/'+task['id'])
    assert len(state['requests'])==2 and state['completion_verified'] is False
    assert read('/api/tasks/'+task['id']+'/requests/'+str(state['requests'][0]['sequence']))['text']=='Build TASK_GOAL_FIXTURE and preserve its tests'
    other=turn('Build SECOND_TASK_FIXTURE','agent',chat=continued,new_task=True)
    assert 'TASK_GOAL_FIXTURE' not in json.dumps(requests[-1]['messages'])
    restored=turn('continue','agent',chat=other,task_id=task['id'])
    assert 'TASK_GOAL_FIXTURE' in requests[-1]['messages'][0]['content']
    assert 'SECOND_TASK_FIXTURE' not in json.dumps(requests[-1]['messages'])
    assert len(read('/api/chats/'+goal['id']+'/tasks'))==2
    print('PASS persistent goal and requests survive fresh model history; continuation restores task state')
    boundary='test-'+uuid.uuid4().hex
    data=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="context.txt"\r\nContent-Type: text/plain\r\n\r\nATTACHMENT_MATERIAL_FIXTURE\r\n--{boundary}--\r\n').encode()
    req=urllib.request.Request(base+'/api/upload',data=data,headers={'X-Studio-Token':token,'Content-Type':'multipart/form-data; boundary='+boundary})
    with http.open(req) as response: item=json.load(response)
    history=turn('Explain the attached material','chat',attachments=[item['id']])
    history=turn('Explain it again','chat',chat=history)
    turn('Explain it again','chat',chat=history,retry=True)
    assert any('ATTACHMENT_MATERIAL_FIXTURE' in m.get('content','') for m in requests[-1]['messages'])
    print('PASS URL history, chat search persona/sampling, mixed action intent and material reconstruction on retry')
    task=(project/'scripts/tests/fixtures/banking-task.md').read_text(encoding='utf-8')
    for mode in ['chat','agent','team']:
        requests.clear(); chat=turn(task,mode)
        assert 'CONTEXT_ACCEPTED' in chat['messages'][-1]['content'],chat
        native=[r for r in requests if 'messages' in r]
        assert native and any(task in m.get('content','') for m in native[0]['messages']),native
        for body in requests:
            assert body['options']['num_predict']==4096 and 8192<=body['options']['num_ctx']<=32768,body
        assert chat['messages'][-1]['diagnostics']['usage']['eval_count']==77
    print('PASS banking requirements reach inference intact in Chat, Agent and Team; shared budget and real backend usage saved')
    history=turn('HISTORY_FIRST_TURN Keep this marker in the conversation.','chat')
    for index in range(14):history=turn('History item '+str(index),'chat',chat=history)
    for index in range(2):history=turn('History material '+str(index)+' '+('abcdefghij '*2200),'chat',chat=history)
    turn('Now explain the earlier conversation.','chat',chat=history)
    assert any('HISTORY_FIRST_TURN' in message.get('content','') for message in requests[-1]['messages'])
    print('PASS useful chat history survives beyond 12 messages and 48000 serialized bytes when it fits the selected window')
    requests.clear(); chat=turn('Multilingual current task:\n'+('Точный полный текст 中文 🦀\n'*900),'chat','on')
    body=requests[-1]; assert body['options']['num_ctx']==32768 and body['options']['num_predict']==8192,body
    print('PASS multilingual task expands Auto window with thinking reserve')
    requests.clear(); chat=turn('OUTPUT_LIMIT_FIXTURE Please explain the response.','chat')
    answer=chat['messages'][-1]
    assert answer['diagnostics']['length_limited'] is True and '\u26a0' in answer['content'],answer
    assert answer['diagnostics']['usage']['done_reason']=='length',answer
    print('PASS output-limit warning and backend stop reason survive chat reload')
    chat=turn('PARTIAL_TOOL_CALL_FIXTURE Create the file requested by the fixture.','agent',allow_error=True)
    assert not (root/'data/workspace/should-never-exist.txt').exists() and not chat['messages'][-1]['tools'],chat
    chat=turn('BROKEN_STREAM_FIXTURE Explain the response.','chat',allow_error=True)
    assert '\u26a0' in chat['messages'][-1]['content'],chat
    current=read('/api/bootstrap')['settings'];current['chat_model']=foundation;current=read('/api/settings',current)
    chat=turn('OUTPUT_LIMIT_FIXTURE Explain the supplied text.','chat')
    assert chat['messages'][-1]['diagnostics']['length_limited'] is True and '\u26a0' in chat['messages'][-1]['content'],chat
    current['chat_model']=model;current=read('/api/settings',current)
    print('PASS partial tool calls never execute; broken streams and adapted-model output limits remain visible')
    chat=turn('OUTPUT_LIMIT_FIXTURE This translation must not be reported as complete.','translate',allow_error=True)
    assert '\u26a0' in chat['messages'][-1]['content'],chat
    print('PASS truncated translation returns an explicit incomplete result instead of successful text')
    current=read('/api/bootstrap')['settings']; current['context']={'window_tokens':8192,'auto_max_tokens':32768,'output_tokens':1024}; current=read('/api/settings',current)
    requests.clear(); long_task='CURRENT_TASK_PRESERVED '+('完整要求 Русский текст\n'*3000)
    chat=turn(long_task,'chat',allow_error=True)
    assert not requests and chat['messages'][-2]['content']==long_task,chat
    assert '8192' in chat['messages'][-1]['content'],chat
    print('PASS real manual-cap overflow preserves current request and does not call inference')
    before=(root/'data/settings.json').read_bytes(); current['context']['output_tokens']=8192
    try:read('/api/settings',current);raise AssertionError('Invalid settings accepted')
    except urllib.error.HTTPError as error:assert error.code==400
    assert (root/'data/settings.json').read_bytes()==before
    current['context']={'window_tokens':0,'auto_max_tokens':32768,'output_tokens':1024};read('/api/settings',current)
    model_limit=8192; requests.clear();chat=turn('Short explanation without actions.','chat')
    assert requests[-1]['options']['num_ctx']==8192
    print('PASS settings validation preserves persisted data; model native context maximum respected')
    numeric=read('/api/bootstrap')['settings']
    numeric['assistant'].update(agent_temperature=0.17,chat_temperature=0.63,repeat_penalty=1.057)
    numeric['agent_timeout_seconds']=61
    read('/api/settings',numeric)
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True)
        page=browser.new_page(viewport={'width':1280,'height':900})
        errors=[];page.on('pageerror',lambda error:errors.append(str(error)))
        page.goto(base);page.locator('[data-action="settings"]').click()
        page.locator('[data-settings-tab="context"]').click()
        page.locator('#context-window').wait_for(state='visible')
        assert page.locator('#context-window').input_value()=='0'
        page.locator('#context-window').select_option('16384')
        page.locator('#context-auto-max').fill('65537')
        page.locator('#context-output').fill('2049')
        page.wait_for_function('()=>document.querySelector("#compute-device").options.length>0')
        page.locator('[data-action="save-settings"]').click()
        page.wait_for_function('()=>!settingsSaving')
        assert not page.locator('#settings-dialog').evaluate('(dialog)=>dialog.open'),page.evaluate('()=>({toasts:document.getElementById("toasts").textContent,invalid:[...document.querySelectorAll("#settings-dialog input,#settings-dialog select,#settings-dialog textarea")].filter(input=>!input.checkValidity()).map(input=>({id:input.id,value:input.value,reason:input.validationMessage})),dirty:settingsDirty,version:settingsEditVersion})')
        assert read('/api/bootstrap')['settings']['context']=={'window_tokens':16384,'auto_max_tokens':65537,'output_tokens':2049}
        saved=read('/api/bootstrap')['settings']
        assert saved['agent_timeout_seconds']==61
        assert saved['assistant']==numeric['assistant']
        stale=saved
        read('/api/settings',{**stale,'theme':'light'})
        before=(root/'data/settings.json').read_bytes()
        try:read('/api/settings',stale)
        except urllib.error.HTTPError as error:assert error.code==409
        else:raise AssertionError('stale settings must not overwrite a newer revision')
        assert (root/'data/settings.json').read_bytes()==before
        page.locator('[data-action="settings"]').click()
        page.locator('[data-settings-tab="context"]').click()
        assert page.locator('#context-window').input_value()=='16384'
        page.set_viewport_size({'width':390,'height':844})
        assert page.evaluate('document.documentElement.scrollWidth')<=390
        page.locator('#context-output').scroll_into_view_if_needed()
        page.screenshot(path=str(root/'context-settings-mobile.png'))
        assert not errors,errors
        browser.close()
    print('PASS context settings load, save and reopen; mobile layout and JavaScript checked in Edge')
finally:
    try:call('/api/shutdown',{}).close()
    except Exception:pass
    try:process.wait(timeout=10)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
    server.shutdown();log.close()

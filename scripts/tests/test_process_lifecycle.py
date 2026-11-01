"""Managed process tools, real stdin/logs, task ownership and isolated Edge UI."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json, os, socket, subprocess, threading, time, urllib.request, urllib.error, uuid

project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('process-test-'+uuid.uuid4().hex[:8])
(root/'data/workspace').mkdir(parents=True)
model='qwen2.5:3b';execution=None
script="[Console]::WriteLine('READY_PROCESS'); while($true){$line=[Console]::ReadLine(); if($null -eq $line){break}; [Console]::WriteLine('ECHO:'+$line)}" if os.name=='nt' else "printf 'READY_PROCESS\\n'; while IFS= read -r line; do printf 'ECHO:%s\\n' \"$line\"; done"
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        global execution
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':32768},'thinking':{'values':[False]}})
        if self.path!='/api/chat':return self.reply({'done':True,'status':'success'})
        messages=body['messages'];last=max(i for i,m in enumerate(messages) if m.get('role')=='user');text=messages[last].get('content','')
        tools=[m for m in messages[last+1:] if m.get('role')=='tool']
        for message in messages:
            if message.get('role')=='tool' and message.get('tool_name')=='start_process':
                result=json.loads(message['content']);raw=result.get('result',result)
                if isinstance(raw,str):
                    try:raw=json.loads(raw)
                    except ValueError:continue
                if isinstance(raw,dict) and raw.get('execution_id'):execution=raw['execution_id']
        call=None;answer='Process operation observed.'
        if ('START_SESSION_FIXTURE' in text or 'FAIL_SESSION_FIXTURE' in text) and not tools:call=('start_process',{'command':script})
        elif 'FAIL_SESSION_FIXTURE' in text:
            self.send_response(500);self.end_headers();self.wfile.write(b'model fixture failure');return
        elif 'CONTINUE_SESSION_FIXTURE' in text:
            if not any(t.get('tool_name')=='write_stdin' for t in tools):call=('write_stdin',{'execution_id':execution,'text':'hello\n'})
            elif not any(t.get('tool_name')=='poll_process' and 'ECHO:hello' in t.get('content','') for t in tools):call=('poll_process',{'execution_id':execution,'wait_ms':500})
            else:answer='Observed ECHO:hello in the actual process log.'
        elif 'FOREIGN_SESSION_FIXTURE' in text and not tools:call=('stop_process',{'execution_id':execution})
        elif 'STOP_SESSION_FIXTURE' in text and not tools:call=('stop_process',{'execution_id':execution})
        message={'role':'assistant','content':answer}
        if call:message={'role':'assistant','content':'','tool_calls':[{'function':{'name':call[0],'arguments':call[1]}}]}
        self.reply({'done':True,'done_reason':'stop','message':message})

fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'permission_mode':'full','cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w')
process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None,method=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def turn(chat,text,allow_error=False):
    with call('/api/chat',{'conversation':chat['id'],'content':text,'mode':'agent','source':'en','target':'ru','web':'off'}) as response:
        for line in response:

            if line.startswith(b'data:'):
                event=json.loads(line[5:]);assert allow_error or event['type']!='error',line
                if event['type']=='approval':
                    assert event['private_preview'] is True and 'hello' not in event['details'],event
                    with call('/api/approve/'+event['id']+'/preview') as response:
                        assert response.headers['Cache-Control']=='no-store'
                        assert json.load(response)['text']=='hello\n'
                    unauthorized=urllib.request.Request(base+'/api/approve/'+event['id']+'/preview')
                    try:http.open(unauthorized)
                    except urllib.error.HTTPError as error:assert error.code==403
                    else:raise AssertionError('private preview must require the app token')
                    read('/api/approve/'+event['id'],{'allow':True})
                    try:call('/api/approve/'+event['id']+'/preview')
                    except urllib.error.HTTPError as error:assert error.code==404
                    else:raise AssertionError('closed approval must erase its memory-only preview')

    return read('/api/chats/'+chat['id'])
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.1)
    else:raise AssertionError('server startup failed')
    chat=turn(read('/api/chats',{}),'START_SESSION_FIXTURE start a managed process in the project')
    result=next(tool for tool in chat['messages'][-1]['tools'] if tool['name']=='start_process');execution=json.loads(result['result'])['execution_id']
    for _ in range(100):
        report=read('/api/executions/'+execution)
        if 'READY_PROCESS' in report['stdout']:break
        time.sleep(.05)
    assert report['status']=='running' and 'READY_PROCESS' in report['stdout']
    task=read('/api/chats/'+chat['id']+'/tasks')[0]
    assert read('/api/tasks/'+task['id'])['active_processes'][0]['execution_id']==execution
    try:call('/api/chats/'+chat['id'],method='DELETE')
    except urllib.error.HTTPError as error:assert error.code==400
    else:raise AssertionError('deletion must not orphan a live process')
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':1280,'height':900});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base)
        page.wait_for_selector('#prompt');page.evaluate('(id)=>switchConversation(id)',chat['id']);page.locator('[data-action="tasks"]').click();page.locator('#task-dialog button',has_text='Процесс:').click()
        page.locator('.process-output').wait_for();page.locator('.process-input').fill('UIhello<button data-action="settings">literal</button>\n');page.locator('.process-send').click()
        page.wait_for_function('()=>document.querySelector(".process-output").textContent.includes("ECHO:UIhello")')
        assert page.locator('.process-output [data-action]').count()==0
        page.set_viewport_size({'width':320,'height':700});assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
        footer=page.locator('#process-dialog>.dialog-footer').bounding_box();assert footer['y']+footer['height']<=700,footer
        page.screenshot(path=str(root/'process-mobile.png'));page.locator('.process-close').click();assert not errors,errors;browser.close()
    settings=read('/api/bootstrap')['settings'];settings['permission_mode']='manual';read('/api/settings',settings)
    chat=turn(chat,'CONTINUE_SESSION_FIXTURE send input to the existing process and inspect the output')
    assert 'ECHO:hello' in chat['messages'][-1]['content']
    foreign=turn(read('/api/chats',{}),'FOREIGN_SESSION_FIXTURE try to stop the process from another task')
    assert any(t['name']=='stop_process' and t['status']=='failed' for t in foreign['messages'][-1]['tools'])
    assert read('/api/executions/'+execution)['status']=='running'
    turn(chat,'STOP_SESSION_FIXTURE stop the process')
    report=read('/api/executions/'+execution);assert report['status']=='cancelled' and 'ECHO:hello' in report['stdout'] and report['logs_synced']
    with call('/api/executions/'+execution+'/logs/stdout') as response:raw=response.read()
    assert b'READY_PROCESS' in raw and b'ECHO:hello' in raw
    settings=read('/api/bootstrap')['settings'];settings['permission_mode']='full';read('/api/settings',settings)
    failed=turn(read('/api/chats',{}),'FAIL_SESSION_FIXTURE start a process before a model failure',allow_error=True)
    failed_start=next(tool for tool in failed['messages'][-1]['tools'] if tool['name']=='start_process')
    failed_id=json.loads(failed_start['result'])['execution_id']
    assert read('/api/executions/'+failed_id)['status']=='cancelled'
    assert next(run for run in read('/api/runs') if run['conversation']==failed['id'])['status']=='failed'
    print('PASS failed run waits for cleanup of the processes it started')
    print('PASS process survives its run, accepts UI/agent stdin, preserves raw logs, rejects another task and stops its owned process')
    print('PASS process inspector uses literal output and fits 320 px with accessible controls')
finally:
    try:call('/api/shutdown',{}).close()
    except Exception:pass
    try:process.wait(timeout=10)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
    fixture.shutdown();log.close()

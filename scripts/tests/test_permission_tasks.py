"""Own data/fake inference: logical grants, restart, new tasks and exact cwd."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,urllib.error,uuid,re
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('permission-tasks-'+uuid.uuid4().hex[:8]);workspace=root/'data/workspace';(workspace/'sub').mkdir(parents=True);outside=root/'outside';outside.mkdir();model='qwen2.5:3b'
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True})
        messages=body['messages'];latest=max(index for index,message in enumerate(messages) if message['role']=='user');text=messages[latest]['content'];tools=[message for message in messages[latest+1:] if message['role']=='tool']
        if tools:return self.reply({'message':{'role':'assistant','content':'PERMISSION_CASE_DONE: tool result recorded.'},'done':True})
        match=re.search(r'WRITECASE:([a-z-]+\.txt)',text)
        if match:name,args='write_file',{'path':match.group(1),'content':'Own fixture content'}
        else:name,args='run_command',{'command':'echo PERMISSION_COMMAND','directory':str(outside) if 'OUTSIDE_COMMAND' in text else 'sub'}
        self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':name,'arguments':args}}]},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';token='';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));process=None;log=(root/'test.log').open('w')
(root/'data/settings.json').write_text(json.dumps({'workspace':str(workspace),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark','permission_mode':'manual'}))
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def start():
    global process,token
    process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('Server did not start')
def stop():
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
def turn(chat,text,duration='once',expected=0,new_task=False):
    approvals=[];body={'conversation':chat['id'],'content':text,'mode':'agent','web':'off','source':'en','target':'ru','attachments':[],'new_task':new_task}
    with call('/api/chat',body) as response:
        for line in response:
            if not line.startswith(b'data:'):continue
            event=json.loads(line[5:]);assert event['type']!='error',event
            if event['type']=='approval':
                approvals.append(event);assert event['scope']['project']==str(workspace)
                if 'OUTSIDE_COMMAND' in text:
                    assert event['reusable'] is False and event['scope']['directory']==str(outside)
                    try:read('/api/approve/'+event['id'],{'allow':True,'duration':'project'})
                    except urllib.error.HTTPError:pass
                    else:raise AssertionError('Outside-project command received reusable permission')
                read('/api/approve/'+event['id'],{'allow':True,'duration':duration})
    assert len(approvals)==expected,(text,approvals)
    for _ in range(100):
        if not read('/api/session')['runs']:break
        time.sleep(.02)
    answer=read('/api/chats/'+chat['id'])['messages'][-1];assert answer['tools'][-1]['status']=='succeeded',answer
    return approvals
try:
    start();chat=read('/api/chats',{});approval=turn(chat,'WRITECASE:a.txt fix project','work_task',1)[0];goal=approval['scope']['work_task'];assert goal and approval['scope']['task']
    turn(chat,'WRITECASE:b.txt continue project',expected=0);stop();start();turn(chat,'WRITECASE:c.txt continue project',expected=0)
    assert json.loads((root/'data/permissions.json').read_text())['version']==2
    another=turn(chat,'WRITECASE:d.txt new goal',expected=1,new_task=True)[0];assert another['scope']['work_task']!=goal
    turn(chat,'WRITECASE:e.txt continue new goal','work_task',1);grants=read('/api/permissions');selected=next(grant for grant in grants if grant['scope']['work_task']==another['scope']['work_task']);read('/api/permissions/'+selected['id']+'/revoke',{})
    turn(chat,'WRITECASE:f.txt continue after revoke',expected=1)
    request=urllib.request.Request(base+'/api/chats/'+chat['id'],method='DELETE',headers={'X-Studio-Token':token})
    with http.open(request):pass
    assert not read('/api/permissions');assert all((workspace/name).read_text()=='Own fixture content' for name in ['a.txt','b.txt','c.txt','d.txt','e.txt','f.txt'])
    print('PASS logical-task write grant spans runs/restart, not a new task; revoke/delete remove permission without touching project files')
    stop();legacy={'id':'legacy-cwd','scope':{'kind':'command','project':str(workspace/'sub'),'paths':[],'command':'echo PERMISSION_COMMAND','task':None},'created':'2020-01-01T00:00:00Z'};(root/'data/permissions.json').write_text(json.dumps([legacy]));start()
    commands=read('/api/chats',{});turn(commands,'COMMANDCASE inspect project command',expected=0)
    turn(commands,'OUTSIDE_COMMAND inspect project command',expected=1)
    print('PASS legacy exact cwd grants remain usable inside the project; an outside cwd cannot receive or reuse project/task grants')
    ui_chat=read('/api/chats',{});accepted=read('/api/runs',{'conversation':ui_chat['id'],'content':'WRITECASE:ui.txt fix project','mode':'agent','web':'off','source':'en','target':'ru','attachments':[]})
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':320,'height':760});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('#prompt');page.evaluate('(id)=>switchConversation(id)',ui_chat['id']);page.wait_for_selector('[data-grant-duration]')
        page.locator('#prompt').focus();page.evaluate('setSidebarOpen(false)');assert page.evaluate('document.activeElement.id==="prompt"')
        page.locator('#approval-shortcut').click();select=page.locator('[data-grant-duration]');assert page.evaluate('document.activeElement.matches("[data-grant-duration]")');assert select.locator('option[value="work_task"]').count()==1;select.select_option('work_task');page.screenshot(path=str(root/'permission-choice-mobile.png'));assert page.evaluate('document.documentElement.scrollWidth<=innerWidth'),page.evaluate('()=>[...document.querySelectorAll("body *")].filter(el=>{const r=el.getBoundingClientRect();return r.width>0&&(r.right>innerWidth+1||r.left< -1)}).map(el=>({tag:el.tagName,id:el.id,cls:el.className,rect:el.getBoundingClientRect().toJSON()}))');assert page.evaluate('()=>{const field=document.querySelector("[data-grant-duration]").getBoundingClientRect(),scroll=document.querySelector("#scroll").getBoundingClientRect();return field.top>=scroll.top&&field.bottom<=scroll.bottom;}'),page.evaluate('()=>({field:document.querySelector("[data-grant-duration]").getBoundingClientRect().toJSON(),scroll:document.querySelector("#scroll").getBoundingClientRect().toJSON(),ancestors:[...function*(el){while(el){yield el;el=el.parentElement}}(document.querySelector("[data-grant-duration]"))].map(el=>({tag:el.tagName,id:el.id,cls:el.className,top:el.getBoundingClientRect().top,overflow:getComputedStyle(el).overflow,scrollTop:el.scrollTop,scrollHeight:el.scrollHeight,clientHeight:el.clientHeight}))})');page.locator('[data-approve]').click();page.wait_for_function('()=>!state.busy',timeout=15000);assert (workspace/'ui.txt').exists();assert not errors,errors;
        assert page.locator('#send').evaluate('(el)=>{const r=el.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth;}')
        for menu in ['web-menu','thinking-menu']:
            page.locator('#'+menu+' summary').click();page.wait_for_function('(id)=>{const el=document.querySelector("#"+id+" .tool-control-options"),r=el.getBoundingClientRect();return getComputedStyle(el).position==="fixed"&&r.left>=0&&r.right<=innerWidth&&r.top>=0&&r.bottom<=innerHeight;}',arg=menu);page.evaluate('(id)=>{window.savedControl=document.querySelector("#"+id+" button");savedControl.focus();updateSend();}',menu);assert page.evaluate('(id)=>savedControl.isConnected&&savedControl===document.activeElement&&document.getElementById(id).open',menu);page.keyboard.press('Escape')
        page.screenshot(path=str(root/'permission-mobile.png'));browser.close()
    assert any(grant['scope']['work_task'] and grant.get('task_title','').startswith('WRITECASE:ui.txt') for grant in read('/api/permissions'))
    stop();start();turn(ui_chat,'WRITECASE:after-ui.txt continue project',expected=0);turn(commands,'COMMANDCASE inspect project command',expected=0)
    print('PASS Edge 320px logical-task duration selection executes the approved action and lists the actual task title')
finally:
    if process:stop()
    fixture.shutdown();log.close()

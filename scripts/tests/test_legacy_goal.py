"""Pre-journal JSON continuation restores exact user requirements, not claims."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('legacy-goal-test-'+uuid.uuid4().hex[:8]);(root/'data/chats').mkdir(parents=True);(root/'data/workspace').mkdir()
chat_id=str(uuid.uuid4());model='qwen2.5:3b';goal='ORIGINAL_LEGACY_GOAL: build the original project, preserve Unicode 🦀 and requirements.\r\n'*2300+'TAIL_REQUIREMENT_42';saw_original=False;saw_tail=False
def message(role,text):return {'id':str(uuid.uuid4()),'role':role,'content':text,'attachments':[],'tools':[],'created':'2020-01-01T00:00:00Z'}
original=[message('user',goal),message('assistant','All tests passed; everything is complete.'),message('user','Also preserve the later correction.')]
(root/'data/chats'/f'{chat_id}.json').write_text(json.dumps({'id':chat_id,'title':'Original project','updated':'2020-01-01T00:00:00Z','mode':'agent','messages':original,'history':[],'history_model':model,'history_workspace':'old-unverified-workspace'}),encoding='utf-8')
def payload(message):
    text=message['content']
    if text.startswith('STATUS: succeeded\nSOURCE_TOOL: '):
        value=json.loads(next(line[len('SOURCE_PAGE: '):] for line in text.splitlines() if line.startswith('SOURCE_PAGE: ')));value['text']=text.split('\nSOURCE_TEXT_BEGIN\n',1)[1].rsplit('\nSOURCE_TEXT_END',1)[0];return value
    value=json.loads(text);result=value.get('result',value);return json.loads(result) if isinstance(result,str) else result
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n'
        try:self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except ConnectionError:pass
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        global saw_original,saw_tail
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True,'status':'success'})
        messages=body['messages'];saw_original|='ORIGINAL_LEGACY_GOAL' in messages[0]['content'];last=payload(next(m for m in reversed(messages) if m['role']=='tool'))
        if 'selected_group' in last:
            names={tool['function']['name'] for tool in body['tools']}
            if last['selected_group']=='processes':
                assert 'run_command' in names and 'write_file' not in names
                return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'discover_tools','arguments':{'group':'files'}}}]},'done':True})
            assert last['selected_group']=='files' and 'write_file' in names and 'run_command' not in names and 'read_task' in names
            return self.reply({'message':{'role':'assistant','content':'LEGACY_REQUIREMENTS_RESTORED. Historical success claims are unverified; no actions were replayed.'},'done':True})
        if 'text' not in last:
            assert last['legacy_context']['effects_verified'] is False and last['completion_verified'] is False
            request=next(row for row in last['requests'] if 'ORIGINAL_LEGACY_GOAL' in row['preview'])
            return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'read_task','arguments':{'request':request['sequence'],'offset':request['characters']-1000,'max_characters':1000}}}]},'done':True})
        saw_tail='TAIL_REQUIREMENT_42' in last['text']
        self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'discover_tools','arguments':{'group':'processes'}}}]},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark','context':{'window_tokens':16384}}))
log=(root/'test.log').open('w');process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=15)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    with call('/api/chat',{'conversation':chat_id,'content':'продолжай работу','mode':'agent','web':'off','source':'en','target':'ru','attachments':[]}) as response:
        for line in response:
            if line.startswith(b'data:'):assert json.loads(line[5:])['type']!='error',line
    assert saw_original and saw_tail
    saved=read('/api/chats/'+chat_id);assert saved['messages'][:3]==original
    assert 'LEGACY_REQUIREMENTS_RESTORED' in saved['messages'][-1]['content']
    task=read('/api/tasks/'+read('/api/chats/'+chat_id+'/tasks')[0]['id']);assert task['legacy_context']['request_count']==2 and len(task['requests'])==3 and task['completion_verified'] is False
    assert task['legacy_context']['original_workspace']=='old-unverified-workspace' and task['project']==str(root/'data/workspace')
    print('PASS pre-journal continuation retains the original goal and exact late requirements through compaction; no historical success is promoted to verified effects')
finally:
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()

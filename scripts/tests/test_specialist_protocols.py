"""Real Rust server + protocol fixture; never downloads model weights."""
import json,os,socket,subprocess,sys,threading,time,uuid,urllib.request
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('protocols-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
(root/'data/workspace/fixture.txt').write_text('FOUNDATION_PROTOCOL_EVIDENCE')
foundation='hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M'
websailor='hf.co/mradermacher/WebSailor-7B-GGUF:Q4_K_M'
team_requests=[]
review_requests=[]
specialist_available=True
class Fixture(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_GET(self):
  if self.path.startswith('/search'):
   body={'results':[{'url':site+'/page','title':'SEARCH_PROTOCOL_EVIDENCE','content':'Fixture source'}, {'url':site+'/other','title':'Other source','content':'Other'}]}
  elif self.path=='/browser-case':
   data=b'''<html><body><form onsubmit="event.preventDefault();document.querySelector('#result').textContent='LOGIN_REJECTED_FIXTURE'">
    <label>Username<input name="username"></label><label>Password<input type="password"></label><button>Sign in</button></form><p id="result">LOGIN_FORM_FIXTURE</p></body></html>'''
   self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(data);return
  elif self.path in ['/page','/other']:
   data=b'<html><title>Fixture</title><p>WEB_PROTOCOL_EVIDENCE</p></html>';self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(data);return
  elif self.path=='/api/tags':body={'models':([{'name':foundation}] if specialist_available else [])+[{'name':websailor},{'name':'qwen2.5:3b'}]}
  elif self.path=='/api/ps':body={'models':[]}
  else:body={'version':'0.40.13'}
  self.reply(body)
 def reply(self,body):
  data=json.dumps(body).encode()+b'\n';self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_POST(self):
  b=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
  if self.path=='/api/show':return self.reply({'capabilities':['completion'],'thinking':{'values':[False]},'model_info':{'test.context_length':262144}})
  if self.path=='/api/generate':
   if not b.get('format') and 'TEAM_WORKFLOW_FIXTURE' in b.get('prompt',''):team_requests.append(('specialist',b))
   assert b.get('raw') is True and 'messages' not in b and 'tools' not in b,b
   prompt=b['prompt']
   if b['model']==foundation:
    if 'BROWSER_FALSE_SUCCESS_FIXTURE' in prompt:
     if 'ZERO_BROWSER_ACTIONS' in prompt or '"role":"tool"' in prompt or '"role": "tool"' in prompt:
      answer=json.dumps({'answer':'Successfully logged in.','tool_calls':[]})
     else:
      answer=json.dumps({'answer':'Already logged in before tool execution.','tool_calls':[{'function':{'name':'browser_open','arguments':{'url':site+'/page'}}}]})
    elif not b.get('format'):answer='Supplied SQL concatenates user input; use bound parameters. No live tests were performed.'
    elif 'FOUNDATION_PROTOCOL_EVIDENCE' in prompt:answer=json.dumps({'answer':'FOUNDATION_PROTOCOL_EVIDENCE','tool_calls':[]})
    else:
     assert b['format']['type']=='object',b
     answer=json.dumps({'answer':'','tool_calls':[{'function':{'name':'read_file','arguments':{'path':'fixture.txt'}}}]})
   else:
    if 'WEB_PROTOCOL_EVIDENCE' in prompt:answer='<think>Checked source</think><answer>WEB_PROTOCOL_EVIDENCE ['+site+'/page]('+site+'/page)</answer>'
    elif 'SEARCH_PROTOCOL_EVIDENCE' in prompt:answer='<tool_call>'+json.dumps({'name':'visit','arguments':{'url':[site+'/page'],'goal':'Read evidence'}})+'</tool_call>'
    else:answer='<tool_call>'+json.dumps({'name':'search','arguments':{'query':['fixture topic']}})+'</tool_call>'
   return self.reply({'response':answer,'done':True})
  if self.path=='/api/chat':
   messages=b['messages']
   if any('REVIEW_ATTACHMENT_FIXTURE' in m.get('content','') for m in messages if m.get('role')=='user'):
    assert not b.get('tools') and b['options']['num_ctx']>=16384,b
    review_requests.append(b)
    return self.reply({'message':{'role':'assistant','content':'REVIEW_ATTACHMENT_COMPLETE: static source observations only; no commands or edits were performed.'},'done':True})
   if any('TEAM_WORKFLOW_FIXTURE' in m.get('content','') for m in messages if m.get('role')=='user'):
    team_requests.append(('controller',b))
    if any('TEAM_BROWSER_FIXTURE' in m.get('content','') for m in messages if m.get('role')=='user'):
     pages=[json.loads(m['content']) for m in messages if m.get('role')=='tool' and m.get('tool_name') in ['browser_open','browser_read','browser_action']]
     stage=sum(m.get('role')=='tool' and m.get('tool_name')=='browser_action' for m in messages)
     if stage>=3:
      assert 'LOGIN_REJECTED_FIXTURE' in pages[-1]['text'],pages
      return self.reply({'message':{'role':'assistant','content':'Observed LOGIN_REJECTED_FIXTURE: test login rejected; account access was not demonstrated.'},'done':True})
     refs={e['name']:e['ref'] for e in pages[-1]['elements']}
     args=({'action':'fill','ref':refs['Username'],'text':'probe-user'} if stage==0 else {'action':'fill','ref':refs['Password'],'text':'probe-secret'} if stage==1 else {'action':'click','ref':refs['Sign in']})
     return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'browser_action','arguments':args}}]},'done':True})
    if any(m.get('tool_name')=='security_analyze' for m in messages):return self.reply({'message':{'role':'assistant','content':'TEAM_WORKFLOW_COMPLETE. The specialist reviewed supplied evidence; no live exploit was verified.'},'done':True})
    if any('NO_OBSERVATIONS' in m.get('content','') for m in messages if m.get('role')=='user'):
     return self.reply({'message':{'role':'assistant','content':'Premature final without expert.','tool_calls':[]},'done':True})
    return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'read_file','arguments':{'path':'fixture.txt'}}}]},'done':True})
   if any(m.get('role')=='tool' for m in messages):return self.reply({'message':{'role':'assistant','content':'The supplied evidence needs parameterized SQL. No live testing was performed.'},'done':True})
   return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'security_analyze','arguments':{'question':'Review supplied SQL','evidence':'query = "SELECT * FROM users WHERE name=" + request.args["name"]'}}}]},'done':True})
  self.reply({'status':'success'})
server=ThreadingHTTPServer(('127.0.0.1',0),Fixture);site='http://127.0.0.1:'+str(server.server_port);threading.Thread(target=server.serve_forever,daemon=True).start()
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base='http://127.0.0.1:'+str(port);http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
settings={'workspace':str(root/'data/workspace'),'agent_model':foundation,'chat_model':foundation,'source':'en','target':'ru','theme':'dark','cpu_only':True,'web':{'provider':'searxng','searxng_url':site,'reranker_enabled':True}}
(root/'data/settings.json').write_text(json.dumps(settings))
(root/'runtime').mkdir();subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:TEST_PY_LINK -Target $env:TEST_PY_TARGET | Out-Null'],env={**os.environ,'TEST_PY_LINK':str(root/'runtime/python'),'TEST_PY_TARGET':str(project/'runtime/python')},check=True)
(root/'runtime/web_worker.py').write_bytes((project/'runtime/web_worker.py').read_bytes())
log=(root/'test.log').open('w');p=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':site},stdout=log,stderr=log)
def call(path,body=None):
 return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=60)
def read(path,body=None):
 with call(path,body) as r:return json.load(r)
def run(model,text,web,mode='agent',allow_error=False,attachments=None):
 settings=read('/api/bootstrap')['settings'];settings['agent_model']=settings['chat_model']=model;settings=read('/api/settings',settings)
 if mode=='team':settings['team_model']=model;read('/api/settings',settings)
 chat=read('/api/chats',{})
 with call('/api/chat',{'conversation':chat['id'],'content':text,'mode':mode,'source':'en','target':'ru','web':web,'thinking':'off','attachments':attachments or []}) as r:
  for line in r:
   if line.startswith(b'data:'):
    e=json.loads(line[5:]);assert allow_error or e['type']!='error',e
    if e['type']=='approval':
     assert site+'/browser-case' in e['details'],e
     read('/api/approve/'+e['id'],{'allow':True})
 return read('/api/chats/'+chat['id'])['messages'][-1]
try:
 for _ in range(80):
  try:token=read('/api/bootstrap')['token'];break
  except Exception:time.sleep(.1)
 a=run(foundation,'Read fixture.txt and return its contents.','off');assert 'FOUNDATION_PROTOCOL_EVIDENCE' in a['content'] and any(t['name']=='read_file' and t['status']=='succeeded' for t in a['tools']),a
 a=run(websailor,'Investigate '+site+'/page and cite your source.','auto');assert 'WEB_PROTOCOL_EVIDENCE' in a['content'],a
 assert [t['name'] for t in a['tools']][:2]==['web_search','fetch_url'],a
 search=json.loads(a['tools'][0]['result']);assert search['reranking']['applied'] is False and search['reranking']['original_order_preserved'] is True
 a=run('qwen2.5:3b','Review the supplied SQL security evidence using the specialist.','off');assert any(t['name']=='security_analyze' and t['status']=='succeeded' for t in a['tools']),a
 for extra,expected_tools in [('',1),(' ZERO_BROWSER_ACTIONS',0)]:
  a=run(foundation,'BROWSER_FALSE_SUCCESS_FIXTURE'+extra+' Check the login at '+site+'/page','auto')
  assert 'Successfully logged in' not in a['content'] and 'Already logged in' not in a['content'],a
  assert len(a['tools'])==expected_tools and a['diagnostics']['browser_claim_retry'] is True and a['diagnostics']['browser_claim_warning'] is True,a
  assert a['content'].startswith('\u041f\u043e\u0434\u0442\u0432\u0435\u0440\u0436\u0434\u0435\u043d\u0438\u044f'),a
  if expected_tools:assert a['tools'][0]['name']=='browser_open' and a['tools'][0]['status']=='succeeded',a
 print('PASS fabricated browser login rejected after open-only and zero-action responses; bounded retry and deterministic evidence fallback')
 a=run('qwen2.5:3b','TEAM_WORKFLOW_FIXTURE Read fixture.txt and review its security.','off','team')
 assert 'TEAM_WORKFLOW_COMPLETE' in a['content'],a
 assert [t['name'] for t in a['tools']]==['read_file','security_analyze'],a
 assert a['diagnostics']['team']['controller']=='qwen2.5:3b' and a['diagnostics']['team']['specialist']==foundation and a['diagnostics']['team']['specialist_status']=='succeeded',a
 assert [role for role,_ in team_requests]==['controller','specialist','controller'],team_requests
 assert all(b.get('keep_alive')==0 for _,b in team_requests),team_requests
 assert 'FOUNDATION_PROTOCOL_EVIDENCE' in team_requests[1][1]['prompt'],team_requests
 print('PASS team controller -> actual file evidence -> automatic Foundation consultation -> controller final, with sequential unload policy')
 team_requests.clear()
 a=run('qwen2.5:3b','TEAM_WORKFLOW_FIXTURE NO_OBSERVATIONS Explain the supplied security question.','off','team')
 assert [t['name'] for t in a['tools']]==['security_analyze'] and a['diagnostics']['team']['specialist_status']=='succeeded',a
 assert 'no verified tool observations yet' in team_requests[1][1]['prompt'],team_requests
 assert 'TEAM_WORKFLOW_COMPLETE' in a['content'],a
 team_requests.clear()
 a=run('qwen2.5:3b','TEAM_WORKFLOW_FIXTURE TEAM_BROWSER_FIXTURE security test my site '+site+'/browser-case','auto','team')
 assert [t['name'] for t in a['tools']]==['browser_open','security_analyze','browser_action','browser_action','browser_action'],a
 assert all(t['status']=='succeeded' for t in a['tools']),a
 assert 'LOGIN_REJECTED_FIXTURE' in a['content'] and 'not demonstrated' in a['content'],a
 assert team_requests[0][0]=='specialist' and 'LOGIN_FORM_FIXTURE' in team_requests[0][1]['prompt'],team_requests
 print('PASS real browser open -> Foundation observes form -> controller fills test login/password and clicks with approvals -> observed rejection, not invented access')
 def upload_source(source):
  boundary='AuditAttachmentBoundary';body=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="files.rs"\r\nContent-Type: text/plain\r\n\r\n'.encode()+source.encode()+f'\r\n--{boundary}--\r\n'.encode())
  req=urllib.request.Request(base+'/api/upload',data=body,headers={'Content-Type':'multipart/form-data; boundary='+boundary,'X-Studio-Token':token})
  with http.open(req,timeout=10) as r:return json.load(r)['id']
 source=Path(os.environ['REVIEW_REGRESSION_FILE']).read_text(encoding='utf-8') if os.environ.get('REVIEW_REGRESSION_FILE') else '// SOURCE_START\n'+('// source code line\n'*850)+'// SOURCE_END\n'
 review_requests.clear();attachment=upload_source(source)
 a=run('qwen2.5:3b','REVIEW_ATTACHMENT_FIXTURE \u043e\u0446\u0435\u043d\u0438 \u043a\u043e\u0434','off',attachments=[attachment])
 assert 'REVIEW_ATTACHMENT_COMPLETE' in a['content'] and a['diagnostics']['source_parts']==1,a
 assert len(review_requests)==1 and source in review_requests[0]['messages'][-1]['content'],a
 assert not any(t.get('status') for t in a['tools']),a
 for mode in ['agent','chat']:
  review_requests.clear()
  pasted=source+'\n---\n\nREVIEW_ATTACHMENT_FIXTURE \u043e\u0446\u0435\u043d\u0438 \u043a\u043e\u0434'
  a=run('qwen2.5:3b',pasted,'off',mode)
  assert 'REVIEW_ATTACHMENT_COMPLETE' in a['content'] and a['diagnostics']['attachment_review'] is True,a
  assert len(review_requests)==1 and review_requests[0]['messages'][-1]['content'].count(source)==1,review_requests
 print('PASS pasted source followed by a review instruction, in both Agent and Chat, without duplicating source in the prompt')
 review_requests.clear();large_source=source*8;attachment=upload_source(large_source)
 a=run('qwen2.5:3b','REVIEW_ATTACHMENT_FIXTURE review attached code','off',attachments=[attachment])
 assert 'REVIEW_ATTACHMENT_COMPLETE' in a['content'] and a['diagnostics']['source_parts']>1 and a['diagnostics']['summary_based'] is True,a
 pages=[b['messages'][-1]['content'].split('Report concrete findings briefly, with exact supporting excerpts.\n',1)[1] for b in review_requests if 'Source fragment ' in b['messages'][-1]['content']]
 assert large_source in ''.join(pages),'Source lost during automatic paging'
 print('PASS uploaded 16 KB source reaches model intact without tools; larger source automatically paged and consolidated with complete source coverage')
 specialist_available=False;team_requests.clear()
 a=run('qwen2.5:3b','TEAM_WORKFLOW_FIXTURE Read fixture.txt','off','team',allow_error=True)
 assert not a['tools'] and not team_requests and foundation in a['content'],a
 print('PASS automatic consultation before an early final and missing-specialist preflight without model downloads or tool execution')
 print('PASS Foundation raw/JSON tool loop, WebSailor search/visit/source loop, missing-reranker fallback without downloads, and independent specialist analysis')
finally:
 try:call('/api/shutdown',{}).close()
 except Exception:pass
 try:p.wait(timeout=10)
 except subprocess.TimeoutExpired:p.terminate();p.wait(timeout=5)
 server.shutdown();log.close()

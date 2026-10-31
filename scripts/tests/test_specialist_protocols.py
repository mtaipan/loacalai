"""Real Rust server + protocol fixture; never downloads model weights."""
import json,os,socket,subprocess,sys,threading,time,uuid,urllib.request
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
project=Path(__file__).resolve().parents[2]
root=project/'build-tools'/('protocols-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
(root/'data/workspace/fixture.txt').write_text('FOUNDATION_PROTOCOL_EVIDENCE')
foundation='hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M'
websailor='hf.co/mradermacher/WebSailor-7B-GGUF:Q4_K_M'
class Fixture(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_GET(self):
  if self.path.startswith('/search'):
   body={'results':[{'url':site+'/page','title':'SEARCH_PROTOCOL_EVIDENCE','content':'Fixture source'}, {'url':site+'/other','title':'Other source','content':'Other'}]}
  elif self.path in ['/page','/other']:
   data=b'<html><title>Fixture</title><p>WEB_PROTOCOL_EVIDENCE</p></html>';self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(data);return
  elif self.path=='/api/tags':body={'models':[{'name':foundation},{'name':websailor},{'name':'qwen2.5:3b'}]}
  elif self.path=='/api/ps':body={'models':[]}
  else:body={'version':'0.40.13'}
  self.reply(body)
 def reply(self,body):
  data=json.dumps(body).encode()+b'\n';self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_POST(self):
  b=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
  if self.path=='/api/show':return self.reply({'capabilities':['completion'],'thinking':{'values':[False]}})
  if self.path=='/api/generate':
   assert b.get('raw') is True and 'messages' not in b and 'tools' not in b,b
   prompt=b['prompt']
   if b['model']==foundation:
    if not b['stream']:answer='Supplied SQL concatenates user input; use bound parameters. No live tests were performed.'
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
def run(model,text,web):
 settings=read('/api/bootstrap')['settings'];settings['agent_model']=settings['chat_model']=model;read('/api/settings',settings)
 chat=read('/api/chats',{})
 with call('/api/chat',{'conversation':chat['id'],'content':text,'mode':'agent','source':'en','target':'ru','web':web,'thinking':'off'}) as r:
  for line in r:
   if line.startswith(b'data:'):
    e=json.loads(line[5:]);assert e['type']!='error',e
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
 print('PASS Foundation raw/JSON tool loop, WebSailor search/visit/source loop, missing-reranker fallback without downloads, and independent specialist analysis')
finally:
 try:call('/api/shutdown',{}).close()
 except Exception:pass
 try:p.wait(timeout=10)
 except subprocess.TimeoutExpired:p.terminate();p.wait(timeout=5)
 server.shutdown();log.close()

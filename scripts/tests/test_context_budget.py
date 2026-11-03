"""Exact9,987 context regression with real Rust request construction and fake inference."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('context-budget-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
model='qwen2.5:3b';translation='translategemma:4b';foundation='hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M';payloads=[]
class Fake(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def reply(self,body):
  data=json.dumps(body).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_GET(self):self.reply({'models':[{'name':m} for m in [model,translation,foundation]]} if self.path=='/api/tags' else {'models':[]} if self.path=='/api/ps' else {'version':'fixture'})
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
  if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':262144},'thinking':{'values':[False,True]}})
  if not body.get('messages') and not body.get('prompt'):return self.reply({'done':True})
  payloads.append(body);self.reply({'message':{'role':'assistant','content':'Transport response.'},'response':'Transport response.','done':True,'done_reason':'stop'})
server=ThreadingHTTPServer(('127.0.0.1',0),Fake);threading.Thread(target=server.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
settings={'workspace':str(root/'data/workspace'),'source':'en','target':'ru','theme':'dark','cpu_only':True,'agent_model':model,'team_model':model,'chat_model':model,'search_model':model,'translation_model':translation,'context':{'window_tokens':9987,'auto_max_tokens':32768,'output_tokens':4096}}
(root/'data/settings.json').write_text(json.dumps(settings));log=(root/'server.log').open('w')
process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{server.server_port}'},stdout=log,stderr=log)
http=urllib.request.build_opener(urllib.request.ProxyHandler({}));base=f'http://127.0.0.1:{port}';token=''
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30)
def read(path,body=None):
 with call(path,body) as response:return json.load(response)
try:
 for _ in range(100):
  try:token=read('/api/bootstrap')['token'];break
  except Exception:time.sleep(.1)
 else:raise AssertionError('startup failed')
 for mode in ['search','agent','chat','team','translate']:
  chat=read('/api/chats',{'mode':mode});events=[]
  with call('/api/chat',{'conversation':chat['id'],'content':'симонс','mode':mode,'source':'en','target':'ru','web':'auto' if mode=='search' else 'off','thinking':'auto','attachments':[],'search':{'depth':'deep'} if mode=='search' else None}) as response:
   for line in response:
    if line.startswith(b'data:'):events.append(json.loads(line[5:]))
  assert not [e for e in events if e['type']=='error'],events
  plans=[e['context'] for e in events if e['type']=='model_context'];assert plans,(mode,events)
  for p in plans:assert p['input_estimate']+p['tools_estimate']+p['output_tokens']+512<=p['window_tokens']<=9987,p
  print('PASS',mode,'9,987-token window: actual inference payload is reached with request, tools and output fitting; no search/model quality claim')
 print('PASS all payload num_ctx/num_predict are finite and compatible with9,987 regression window:',len(payloads))
 assert all(0<p['options']['num_predict']<p['options']['num_ctx']<=9987 for p in payloads)
finally:process.terminate();process.wait(timeout=15);log.close();server.shutdown();server.server_close()

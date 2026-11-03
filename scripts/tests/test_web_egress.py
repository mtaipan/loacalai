"""Owned HTTP/browser fixtures: web-tool egress, not an OS firewall test."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util, json, os, subprocess, sys, threading, uuid, queue
project=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('web_worker',project/'runtime/web_worker.py');worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
# Offline classification must happen before any DNS resolution.
b=worker.Browser();b.network='loopback';b.local_origins={worker.origin('http://localhost:8123/'),worker.origin('http://[::1]:8123/')}
resolve=worker.socket.getaddrinfo
worker.socket.getaddrinfo=lambda *a,**kw:(_ for _ in ()).throw(AssertionError('Unexpected DNS lookup'))
try:
    b.allow('http://localhost:8123/ok');b.allow('http://[::1]:8123/ok')
    for raw in ['https://example.invalid/','http://localhost:8124/','http://127.0.0.1:8123/','http://192.168.0.1/','http://localhost.evil.invalid/']:
        try:b.allow(raw)
        except ValueError:pass
        else:raise AssertionError(raw)
finally:worker.socket.getaddrinfo=resolve
class Socket:
    def __init__(self,value):self.url=value;self.connected=False;self.closed=False
    def connect_to_server(self):self.connected=True
    def close(self,**kwargs):self.closed=True
for value,allowed in [('ws://localhost:8123/live',True),('wss://example.invalid/live',False),('ws://localhost:8124/live',False)]:
    ws=Socket(value);b.socket_route(ws);b.close_blocked_sockets();assert ws.connected==allowed and ws.closed!=allowed
captured=[];original_connect=worker.socket.create_connection
worker.socket.create_connection=lambda addr,*a:(captured.append(addr) or object())
try:
    connection=worker.loopback_connection(worker.http.client.HTTPConnection,'localhost:8123')
    connection._create_connection(('localhost',8123));assert captured[0][0]=='::1' and connection.host=='localhost'
finally:worker.socket.create_connection=original_connect
print('PASS loopback/DNS-before-network and exact-origin WebSocket policy')
seen=[];foreign=[]
class Foreign(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):foreign.append(self.path);self.send_response(200);self.end_headers()
other=ThreadingHTTPServer(('127.0.0.1',0),Foreign);foreign_url=f'http://127.0.0.1:{other.server_port}';threading.Thread(target=other.serve_forever,daemon=True).start()
class Site(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        seen.append(self.path)
        if self.path=='/streamoversize':self.send_response(200);self.end_headers();self.wfile.write(b'x'*2048);return
        if self.path=='/short':self.send_response(200);self.send_header('Content-Length','1024');self.end_headers();self.wfile.write(b'x');return
        if self.path=='/cookie-set':self.send_response(200);self.send_header('Set-Cookie','one=1; HttpOnly');self.send_header('Set-Cookie','two=2; SameSite=Lax');self.end_headers();self.wfile.write(b'COOKIES_SET');return
        if self.path=='/cookie-read':self.send_response(200);self.end_headers();self.wfile.write(self.headers.get('Cookie','').encode());return
        if self.path=='/oversize':self.send_response(200);self.send_header('Content-Length',str(worker.BROWSER_RESOURCE_BYTES+1));self.end_headers();return
        if self.path=='/redirect':self.send_response(302);self.send_header('Location',foreign_url+'/redirect-target');self.end_headers();return
        self.send_response(404 if self.path=='/missing' else 200);self.send_header('Content-Type','text/html; charset=utf-8');self.end_headers()
        if self.path=='/page':
            self.wfile.write(('<html><title>Owned local site</title><body>LOCAL_EVIDENCE<label>Username<input name="username"></label><label>Password<input type="password"></label><img src="/image"><img src="/oversize"><img src="/redirect"><img src="'+foreign_url+'/image"><img src="https://external.invalid/image"><a href="'+foreign_url+'/clicked">Foreign service</a><script>fetch("/same-origin");fetch("/redirect").catch(()=>{});fetch("'+foreign_url+'/fetch").catch(()=>{});new WebSocket("'+foreign_url.replace('http:','ws:')+'/socket");</script></body></html>').encode())
        else:self.wfile.write(b'LOCAL_RESPONSE')
site=ThreadingHTTPServer(('127.0.0.1',0),Site);local=f'http://127.0.0.1:{site.server_port}';threading.Thread(target=site.serve_forever,daemon=True).start()
root=project/'build-tools'/('egress-'+uuid.uuid4().hex[:8]);root.mkdir();log=(root/'worker.log').open('w',encoding='utf-8');print(root,flush=True)
database=root/'authority.sqlite3';db=worker.sqlite3.connect(database)
db.executescript('PRAGMA user_version=9; CREATE TABLE runs(id TEXT PRIMARY KEY,input TEXT); CREATE TABLE task_runs(run TEXT PRIMARY KEY,task TEXT); CREATE TABLE task_requests(run TEXT,content TEXT); CREATE TABLE web_user_origins(run TEXT,origin TEXT);')
for i in range(21):
    content='Use alice and secret-from-original-message' if i==0 else 'continue'
    db.execute('INSERT INTO runs VALUES (?,?)',(str(i),json.dumps({'content':content})));db.execute('INSERT INTO task_runs VALUES (?,?)',(str(i),'task'));db.execute('INSERT INTO task_requests VALUES (?,?)',(str(i),content))
db.execute('INSERT INTO runs VALUES (?,?)',('foreign',json.dumps({'content':'different task'})));db.execute('INSERT INTO task_runs VALUES (?,?)',('foreign','foreign'))
db.execute('INSERT INTO web_user_origins VALUES (?,?)',('0',local))
for port in range(8100,8400):db.execute('INSERT INTO web_user_origins VALUES (?,?)',('0',f'http://localhost:{port}'))
db.commit();db.close()
# The new document tables do not change the web-authority contract.
compat=worker.Authority(str(database),'20');compat.db.close()
with worker.sqlite3.connect(database) as upgrade:upgrade.execute('PRAGMA user_version=10')
authority=worker.Authority(str(database),'20')
assert authority.user_origin(local) and authority.supplied('secret-from-original-message') and not authority.supplied('invented')
for port in range(8100,8400):assert authority.user_origin(f'http://localhost:{port}')
assert len(authority.origins)<=256 and authority.user_origin('http://localhost:8100')
try:authority.db.execute('DELETE FROM runs')
except worker.sqlite3.OperationalError:pass
else:raise AssertionError('Authority connection must be read-only')
authority.close();authority=worker.Authority(str(database),'foreign');assert not authority.user_origin(local) and not authority.supplied('secret-from-original-message');authority.close()
print('PASS read-only durable user authority, full-task credentials beyond last twelve messages, cache eviction without access loss and foreign-task denial')
p=subprocess.Popen([sys.executable,'-u','-c',"import runpy,faulthandler;faulthandler.dump_traceback_later(40,repeat=True);runpy.run_path("+repr(str(project/'runtime/web_worker.py'))+",run_name='__main__')"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,encoding='utf-8',env={**os.environ,'PYTHONUTF8':'1'})
lines=queue.Queue()
def output():
    for line in p.stdout:lines.put(line)
threading.Thread(target=output,daemon=True).start()
def request(name,args,network='loopback',expected_error=False,authority_run=None):
    payload={'name':name,'args':args,'network':network,'user_urls':[local+'/page']}
    if authority_run is not None:payload['authority']={'database':str(database),'run':authority_run};payload['user_urls']=[]
    p.stdin.write(json.dumps(payload)+'\n');p.stdin.flush();result=json.loads(lines.get(timeout=35));assert bool(result.get('error'))==expected_error,result;return result
try:
    bound=worker.BROWSER_RESOURCE_BYTES;worker.BROWSER_RESOURCE_BYTES=1024
    try:
        for endpoint,error_type in [('/streamoversize',worker.ResourceTooLarge),('/short',OSError)]:
            try:worker.browser_resource(local+endpoint,'GET',{},None,'loopback')
            except error_type:pass
            else:raise AssertionError(endpoint)
    finally:worker.BROWSER_RESOURCE_BYTES=bound
    assert worker.get(local.replace('127.0.0.1','localhost')+'/text',network='loopback')[0]==200
    assert 'LOCAL_RESPONSE' in request('fetch_url',{'url':local+'/text'})['text']
    redirect=request('fetch_url',{'url':local+'/redirect'});assert redirect['status']==302 and redirect['redirect_to'].startswith(foreign_url)
    request('fetch_url',{'url':foreign_url+'/redirect-target'},expected_error=True)
    request('fetch_url',{'url':'https://external.invalid/'},expected_error=True)
    request('web_search',{'query':'must not reach provider'},expected_error=True)
    page=request('browser_open',{'url':local+'/page'});assert 'LOCAL_EVIDENCE' in page['text']
    assert any(item['reason']=='resource_buffer_exceeded' for item in page['blocked_reasons']),page
    assert any('external.invalid' in value for value in page['blocked_requests']),page
    assert any(foreign_url in value for value in page['blocked_requests']),page
    assert '/same-origin' in seen and '/image' in seen and not foreign,(seen,foreign)
    login=next(e['ref'] for e in page['elements'] if e['name']=='Username');password=next(e['ref'] for e in page['elements'] if e['name']=='Password')
    request('browser_prepare',{'action':'fill','ref':login,'text':'alice'},authority_run='20')
    request('browser_prepare',{'action':'fill','ref':password,'text':'secret-from-original-message'},authority_run='20')
    denied=request('browser_prepare',{'action':'fill','ref':password,'text':'secret-from-original-message'},authority_run='foreign',expected_error=True);assert denied['error_code']=='needs_input'
    ref=next(e['ref'] for e in page['elements'] if e['name']=='Foreign service')
    request('browser_action',{'action':'click','ref':ref},expected_error=True)
    assert not foreign,foreign
    redirect=request('browser_open',{'url':local+'/redirect'});assert redirect['navigation']=='redirect_requires_open' and redirect['status']==302
    request('browser_open',{'url':redirect['redirect_to']},expected_error=True)
    assert not foreign,foreign
    oversized=request('browser_open',{'url':local+'/oversize'},expected_error=True);assert oversized['error_code']=='resource_buffer_exceeded'
    page=request('browser_open',{'url':local+'/missing'});assert page['status']==404
    page=request('browser_read',{});assert page['status']==404 and page['state']=='unavailable'
    request('browser_open',{'url':local+'/cookie-set'})
    cookie=request('browser_open',{'url':local+'/cookie-read'});assert 'one=1' in cookie['text'] and 'two=2' in cookie['text'],cookie
    # A previous page's live controls cannot cross a policy transition.
    page=request('browser_open',{'url':local+'/page'},network='external');ref=page['elements'][0]['ref']
    request('browser_prepare',{'action':'click','ref':ref},network='loopback',expected_error=True)
    request('browser_read',{},network='disabled',expected_error=True)
    count=len(seen);request('fetch_url',{'url':local+'/text'},network='disabled',expected_error=True);assert len(seen)==count
    print('PASS real fetch/browser loopback, subresources/fetch/redirect/click denial, closed pages on policy changes and material-only denial')
finally:
    try:
        p.stdin.write('{"name":"close"}\n');p.stdin.flush();p.wait(timeout=10)
    finally:
        if p.poll() is None:
            if sys.platform=='win32':subprocess.run(['taskkill','/PID',str(p.pid),'/T','/F'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            else:p.kill()
            p.wait(timeout=10)
        site.shutdown();other.shutdown();log.close()
    assert p.returncode==0,(root/'worker.log').read_text(encoding='utf-8')

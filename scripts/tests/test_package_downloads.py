"""Local HTTP range fixtures: resumable archive blocks, never real engine downloads."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import hashlib,json,re,sys,tempfile,threading,shutil,subprocess
project=Path(__file__).resolve().parents[2];sys.path.insert(0,str(project/'runtime'));import compute_packages as package
root=Path(tempfile.mkdtemp(prefix='package-downloads-',dir=project/'build-tools')).resolve()
body=bytes(range(256))*400;etag='"version-one"';modified='Mon, 02 Nov 2026 12:00:00 GMT';calls=[];fault=None
class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        match=re.fullmatch(r'bytes=(\d+)-(\d+)',self.headers['Range']);start,end=map(int,match.groups());calls.append((start,end,self.headers.get('If-Range')))
        data=body[start:end+1];self.send_response(200 if fault=='ignored' and end>0 else 206)
        self.send_header('Content-Range',f'bytes {start}-{end}/{len(body)}')
        if etag is not None:self.send_header('ETag',etag)
        self.send_header('Last-Modified',modified);self.end_headers()
        self.wfile.write(data[:-1] if fault=='truncated' and end>0 else data)
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
url=f'http://127.0.0.1:{server.server_port}/archive';package.RangeZip.block_size=4096
try:
    with package.RangeZip(url,root) as archive:
        assert archive.read(9000)==body[:9000]
        cache=archive.disk;assert len(list(cache.glob('*.json')))==4
    first=len(calls)
    with package.RangeZip(url,root) as archive:
        assert archive.read(9000)==body[:9000]
        assert len(calls)==first+1,'completed blocks must survive a new process/session; only identity probe is fetched'
        archive.seek(999999);assert archive.read()==b''
        try:archive.seek(-1);raise AssertionError('negative offset')
        except ValueError:assert archive.tell()==999999
    before=len(calls)
    child="import sys,hashlib;sys.path.insert(0,sys.argv[1]);import compute_packages as p;p.RangeZip.block_size=4096;z=p.RangeZip(sys.argv[2],sys.argv[3]);print(hashlib.sha256(z.read(9000)).hexdigest())"
    result=subprocess.run([sys.executable,'-c',child,str(project/'runtime'),url,str(root)],capture_output=True,text=True,check=True)
    assert result.stdout.strip()==hashlib.sha256(body[:9000]).hexdigest() and len(calls)==before+1
    assert all(c[2]=='"version-one"' for c in calls if c[1]>0)
    print('PASS fsynced blocks resume across sessions, exact HTTP ranges/If-Range, bounded blocks and EOF/seek validation')
    # An interrupted write has no trusted metadata and corrupt committed bytes must be refetched.
    (cache/'4096').write_bytes(b'X'*4096);(cache/'8192.json').unlink()
    (cache/'12288.own-fixture.part').write_bytes(b'PARTIAL')
    before=len(calls)
    with package.RangeZip(url,root) as archive:assert archive.read(13000)==body[:13000]
    assert len(calls)==before+4,'probe + corrupt/unsealed/new blocks'
    print('PASS corrupt blocks and interrupted/unsealed bytes are never treated as complete')
    old=cache;etag='"version-two"';body=b'Z'*len(body)
    with package.RangeZip(url,root) as archive:
        assert archive.disk!=old and archive.read(8)==b'Z'*8
        etag='"version-three"';archive.seek(5000)
        try:archive.read(10);raise AssertionError('changed archive accepted')
        except RuntimeError as e:assert 'identity changed' in str(e)
    assert old.exists(),'previous bytes remain separate'
    print('PASS changed same-length archive gets separate cache; mid-download identity change fails before publication')
    for validator in [None,'W/"weak-version"']:
        etag=validator;before=len(calls)
        for _ in range(2):
            with package.RangeZip(url,root) as archive:assert archive.disk is None and archive.read(8)==body[:8]
        assert len(calls)==before+4
    print('PASS missing/weak ETag never reuses persistent bytes based only on size/date')
    etag='"fault-version"'
    for fault in ['truncated','ignored']:
        with package.RangeZip(url,root) as archive:
            try:archive.read(5);raise AssertionError('bad range accepted')
            except RuntimeError:assert not (archive.disk/'0.json').exists()
    fault=None
    print('PASS short/ignored HTTP ranges do not create completed blocks')
finally:
    server.shutdown();server.server_close();shutil.rmtree(root)
